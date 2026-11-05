//! 編集の取り消し（`StorageService::revert_edit`・#1049）の結合テスト。
//!
//! 実 Postgres＋OpenFGA＋S3 互換ストア（`STORAGE_TEST_DATABASE_URL` / `OPENFGA_TEST_URL` /
//! `STORAGE_TEST_S3_ENDPOINT`）が揃う時のみ実行する（未設定はスキップ）。
//!
//! 検証すること:
//! - 編集の直前の版を**新しい版として**復元する（履歴は残る）
//! - 二度目は何もしない（版を積まない）
//! - 編集の後に別の版があれば force なしでは 409 相当、force で戻せる
//! - 新規作成の版・存在しない版は NotFound、閲覧のみの利用者は拒否

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::{sync::Arc, time::Duration};

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, Principal, Relation,
};
use sqlx::postgres::PgPoolOptions;
use storage::{
    object_store::S3Config, ObjectStore, RevertOutcome, S3ObjectStore, StorageError, StorageService,
};
use uuid::Uuid;

struct Env {
    service: StorageService,
    authz: Arc<dyn AuthzClient>,
}

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url,
            store_name: format!("shiki-revert-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("fga");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    }));
    store.ensure_bucket().await.expect("bucket");
    let service = StorageService::new(
        pool,
        store,
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    );
    Some(Env { service, authz })
}

fn user(org: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.into(),
        "default".into(),
    )
}

async fn content(env: &Env, ctx: &AuthContext, id: Uuid) -> Vec<u8> {
    env.service
        .read_file_internal(ctx, id, None)
        .await
        .expect("read")
        .1
}

#[tokio::test]
async fn revert_edit_restores_previous_version_as_new_version() {
    let Some(env) = setup().await else { return };
    let org = format!("org-{}", Uuid::new_v4().simple());
    let alice = user(&org, "alice");
    env.authz
        .write_tuple(
            &alice.subject(),
            Relation::Member,
            &alice.ns().organization(&org),
        )
        .await
        .expect("member");
    // 内容は実行ごとに一意にする（同じ DB を使い回しても content-addressing で混ざらない）。
    let tag = Uuid::new_v4();
    let v1 = format!("a,b\n1,{tag}\n");
    let v2 = format!("a,b\n1,{tag}\n2,ai\n");
    let file = env
        .service
        .write_file_internal(
            &alice,
            None,
            &format!("r-{tag}.csv"),
            v1.as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("create v1");
    let edited = env
        .service
        .update_file_content_internal(&alice, file.id, v2.as_bytes(), "text/csv", None)
        .await
        .expect("edit v2");
    assert_eq!(edited.version, file.version + 1);

    // 取り消し → 直前の版（v1）の内容が新しい版として入る。
    let out = env
        .service
        .revert_edit(&alice, file.id, edited.version, false, None)
        .await
        .expect("revert");
    let RevertOutcome::Reverted {
        node,
        restored_from_version,
    } = out
    else {
        panic!("新しい版として戻る: {out:?}");
    };
    assert_eq!(restored_from_version, file.version);
    assert_eq!(node.version, edited.version + 1, "履歴は巻き戻さず追記");
    assert_eq!(content(&env, &alice, file.id).await, v1.as_bytes());

    // 二度目は何もしない（版を積まない）。
    let again = env
        .service
        .revert_edit(&alice, file.id, edited.version, false, None)
        .await
        .expect("revert again");
    match again {
        RevertOutcome::AlreadyReverted(n) => assert_eq!(n.version, node.version),
        other => panic!("既に戻っているなら何もしない: {other:?}"),
    }

    // 新規作成の版（その前の内容版が無い）・存在しない版は NotFound。
    for v in [file.version, 9_999] {
        assert!(
            matches!(
                env.service
                    .revert_edit(&alice, file.id, v, false, None)
                    .await,
                Err(StorageError::NotFound)
            ),
            "version {v}"
        );
    }
}

#[tokio::test]
async fn revert_edit_refuses_to_drop_later_edits_without_force() {
    let Some(env) = setup().await else { return };
    let org = format!("org-{}", Uuid::new_v4().simple());
    let alice = user(&org, "alice");
    env.authz
        .write_tuple(
            &alice.subject(),
            Relation::Member,
            &alice.ns().organization(&org),
        )
        .await
        .expect("member");
    let tag = Uuid::new_v4();
    let base = format!("x\n{tag}\n");
    let ai = format!("x\n{tag}\nai\n");
    let human = format!("x\n{tag}\nai\nhuman\n");
    let file = env
        .service
        .write_file_internal(
            &alice,
            None,
            &format!("c-{tag}.csv"),
            base.as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("create");
    let ai_edit = env
        .service
        .update_file_content_internal(&alice, file.id, ai.as_bytes(), "text/csv", None)
        .await
        .expect("ai edit");
    let later = env
        .service
        .update_file_content_internal(&alice, file.id, human.as_bytes(), "text/csv", None)
        .await
        .expect("later edit");

    // AI の編集の後に別の版がある → force なしでは拒否し、内容は変えない。
    match env
        .service
        .revert_edit(&alice, file.id, ai_edit.version, false, None)
        .await
    {
        Err(StorageError::VersionConflict { current }) => assert_eq!(current, later.version),
        other => panic!("後の編集があれば競合: {other:?}"),
    }
    assert_eq!(content(&env, &alice, file.id).await, human.as_bytes());

    // force なら AI の編集前（base）まで戻す（後の変更も失われる）。
    match env
        .service
        .revert_edit(&alice, file.id, ai_edit.version, true, None)
        .await
        .expect("force revert")
    {
        RevertOutcome::Reverted {
            restored_from_version,
            ..
        } => assert_eq!(restored_from_version, file.version),
        other => panic!("{other:?}"),
    }
    assert_eq!(content(&env, &alice, file.id).await, base.as_bytes());
}

#[tokio::test]
async fn revert_edit_requires_editor() {
    let Some(env) = setup().await else { return };
    let org = format!("org-{}", Uuid::new_v4().simple());
    let alice = user(&org, "alice");
    let bob = user(&org, "bob");
    env.authz
        .write_tuple(
            &alice.subject(),
            Relation::Member,
            &alice.ns().organization(&org),
        )
        .await
        .expect("member");
    let tag = Uuid::new_v4();
    let file = env
        .service
        .write_file_internal(
            &alice,
            None,
            &format!("p-{tag}.csv"),
            format!("a\n{tag}\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("create");
    let edited = env
        .service
        .update_file_content_internal(
            &alice,
            file.id,
            format!("a\n{tag}\n2\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("edit");
    // bob は閲覧のみ。
    env.authz
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &bob.ns().file(&file.id.to_string()),
        )
        .await
        .expect("viewer");
    let res = env
        .service
        .revert_edit(&bob, file.id, edited.version, false, None)
        .await;
    assert!(
        matches!(res, Err(StorageError::Forbidden)),
        "閲覧のみは取り消せない: {res:?}"
    );
    assert_eq!(
        content(&env, &alice, file.id).await,
        format!("a\n{tag}\n2\n").as_bytes(),
        "内容は変わらない"
    );
}
