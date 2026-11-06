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
    pool: sqlx::PgPool,
    authz: Arc<dyn AuthzClient>,
}

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
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
        pool.clone(),
        store,
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    );
    Some(Env {
        service,
        pool,
        authz,
    })
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
        .revert_edit(&alice, file.id, &[edited.version], false, None)
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
        .revert_edit(&alice, file.id, &[edited.version], false, None)
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
                    .revert_edit(&alice, file.id, &[v], false, None)
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
        .revert_edit(&alice, file.id, &[ai_edit.version], false, None)
        .await
    {
        Err(StorageError::VersionConflict { current }) => assert_eq!(current, later.version),
        other => panic!("後の編集があれば競合: {other:?}"),
    }
    assert_eq!(content(&env, &alice, file.id).await, human.as_bytes());

    // force なら AI の編集前（base）まで戻す（後の変更も失われる）。
    match env
        .service
        .revert_edit(&alice, file.id, &[ai_edit.version], true, None)
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
        .revert_edit(&bob, file.id, &[edited.version], false, None)
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

/// 同じファイルへの連続した編集（v2, v3）は、最初の版＋期待する現在版（v3）でまとめて取り消せる。
/// その後に別の編集が入っていれば競合する。
#[tokio::test]
async fn revert_edit_can_undo_consecutive_edits_together() {
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
    let file = env
        .service
        .write_file_internal(
            &alice,
            None,
            &format!("m-{tag}.csv"),
            base.as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("create");
    let first = env
        .service
        .update_file_content_internal(
            &alice,
            file.id,
            format!("{base}1\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("edit 1");
    let second = env
        .service
        .update_file_content_internal(
            &alice,
            file.id,
            format!("{base}1\n2\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("edit 2");

    // 最初の編集だけを取り消そうとすると、2 回目の編集が「後から入った別の内容版」なので競合。
    assert!(matches!(
        env.service
            .revert_edit(&alice, file.id, &[first.version], false, None)
            .await,
        Err(StorageError::VersionConflict { current }) if current == second.version
    ));
    // 両方の編集の版を渡す → 最初の編集の前まで戻る。
    let out = env
        .service
        .revert_edit(
            &alice,
            file.id,
            &[first.version, second.version],
            false,
            None,
        )
        .await
        .expect("revert both");
    assert!(
        matches!(out, RevertOutcome::Reverted { restored_from_version, .. } if restored_from_version == file.version)
    );
    assert_eq!(content(&env, &alice, file.id).await, base.as_bytes());
}

/// まとめて取り消す 2 つの編集の**間**に別の人の内容版が入っていたら、まとめて取り消しても競合する
/// （最後の編集が現在版でも、間の他人の変更を黙って消さない）。
#[tokio::test]
async fn revert_edit_detects_foreign_edit_between_batched_edits() {
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
    let file = env
        .service
        .write_file_internal(
            &alice,
            None,
            &format!("b-{tag}.csv"),
            base.as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("create");
    let ai1 = env
        .service
        .update_file_content_internal(
            &alice,
            file.id,
            format!("{base}ai1\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("ai1");
    let human = env
        .service
        .update_file_content_internal(
            &alice,
            file.id,
            format!("{base}ai1\nhuman\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("human");
    let ai2 = env
        .service
        .update_file_content_internal(
            &alice,
            file.id,
            format!("{base}ai1\nhuman\nai2\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("ai2");
    assert!(human.version > ai1.version && ai2.version > human.version);
    assert!(
        matches!(
            env.service
                .revert_edit(&alice, file.id, &[ai1.version, ai2.version], false, None)
                .await,
            Err(StorageError::VersionConflict { current }) if current == ai2.version
        ),
        "間の他人の版は確認なしに消さない"
    );
    assert_eq!(
        content(&env, &alice, file.id).await,
        format!("{base}ai1\nhuman\nai2\n").as_bytes()
    );
}

/// 名前変更・移動のような内容の変わらない版上げは、取り消しの競合にしない。
#[tokio::test]
async fn revert_edit_ignores_rename_after_edit() {
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
    let file = env
        .service
        .write_file_internal(
            &alice,
            None,
            &format!("n-{tag}.csv"),
            base.as_bytes(),
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
            format!("{base}ai\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("edit");
    let renamed = env
        .service
        .rename_file(&alice, file.id, &format!("renamed-{tag}.csv"), None)
        .await
        .expect("rename");
    assert!(
        renamed.version > edited.version,
        "名前変更でも node.version は進む"
    );
    let out = env
        .service
        .revert_edit(&alice, file.id, &[edited.version], false, None)
        .await
        .expect("名前変更は競合にならない");
    assert!(matches!(out, RevertOutcome::Reverted { .. }));
    assert_eq!(content(&env, &alice, file.id).await, base.as_bytes());
}

/// ブラウザの Office エディタ（WOPI ロック）で開かれている間は戻さない（次の自動保存で書き戻されるため）。
/// 期限切れのロックは無視する。
#[tokio::test]
async fn revert_edit_refuses_while_office_editor_holds_lock() {
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
    let file = env
        .service
        .write_file_internal(
            &alice,
            None,
            &format!("l-{tag}.csv"),
            base.as_bytes(),
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
            format!("{base}ai\n").as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("edit");
    sqlx::query(
        "INSERT INTO office_lock (file_id, lock_id, locked_by, tenant_id, expires_at) \
         VALUES ($1, 'L', 'user:alice', $2, now() + interval '10 minutes')",
    )
    .bind(file.id)
    .bind(&alice.tenant_id)
    .execute(&env.pool)
    .await
    .expect("lock");
    assert!(matches!(
        env.service
            .revert_edit(&alice, file.id, &[edited.version], false, None)
            .await,
        Err(StorageError::Locked)
    ));
    assert_eq!(
        content(&env, &alice, file.id).await,
        format!("{base}ai\n").as_bytes()
    );

    // 期限切れのロックは助言的に無視する（クラッシュ残留で永久に戻せない、を作らない）。
    sqlx::query(
        "UPDATE office_lock SET expires_at = now() - interval '1 minute' WHERE file_id = $1",
    )
    .bind(file.id)
    .execute(&env.pool)
    .await
    .expect("expire");
    assert!(matches!(
        env.service
            .revert_edit(&alice, file.id, &[edited.version], false, None)
            .await,
        Ok(RevertOutcome::Reverted { .. })
    ));
}
