//! 同じ名前のファイルがあるときの番号付け（#381・BUG-97・D-112）の結合テスト。実 Postgres・RustFS・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する（未設定ならスキップ）。
//!
//! 番号を付けるのは StorageService の書込の入口だけ。ドライブの新規作成（`write_file_unique_internal`）・
//! 扱いを選ぶ新規作成（`write_file_named`）・ワークフローの「ファイルを保存」（`write_file_internal_idempotent`）が
//! 同じ規則（`storage::numbered_name`・上限 `MAX_NUMBERED_NAME`）で名前を決めることを、1 か所で確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::{sync::Arc, time::Duration};

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, Consistency, Principal, Relation,
};
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{
    numbered_name, object_store::S3Config, ObjectStore, OnNameTaken, S3ObjectStore, StorageError,
    StorageService, MAX_NUMBERED_NAME,
};
use uuid::Uuid;

struct Ctx {
    service: StorageService,
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
}

async fn setup() -> Option<Ctx> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(openfga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let access_key =
        std::env::var("STORAGE_TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let secret_key =
        std::env::var("STORAGE_TEST_S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".into());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");

    let http = reqwest::Client::new();
    let fga = OpenFgaClient::connect(
        http,
        &OpenFgaConfig {
            base_url: openfga_url,
            store_name: format!("shiki-storage-naming-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);

    let s3 = S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key,
        secret_key,
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    store.ensure_bucket().await.expect("バケット準備");

    let service = StorageService::new(
        pool.clone(),
        store,
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    );
    Some(Ctx {
        service,
        pool,
        authz,
    })
}

fn make_ctx(org: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.into(),
        authz::TenantId::parse("default").unwrap(),
    )
}

async fn seed_org_member(authz: &Arc<dyn AuthzClient>, org: &str, uid: &str) {
    let ctx = make_ctx(org, uid);
    authz
        .write_tuple(
            &ctx.subject(),
            Relation::Member,
            &ctx.ns().organization(org),
        )
        .await
        .expect("member tuple seed");
    for _ in 0..200 {
        if authz
            .check(
                &ctx.subject(),
                Relation::Member,
                &ctx.ns().organization(org),
                Consistency::MinimizeLatency,
            )
            .await
            .expect("member check")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("seed した org member タプルが見えませんでした（org={org} uid={uid}）");
}

/// 冪等キー付きの書込（ワークフローの「ファイルを保存」と同じ入口）。
async fn save(
    s: &StorageService,
    ctx: &AuthContext,
    parent: Uuid,
    name: &str,
    on_taken: OnNameTaken,
    key: &str,
) -> Result<Value, StorageError> {
    s.write_file_internal_idempotent(
        ctx,
        Some(parent),
        name,
        b"# x",
        "text/markdown",
        on_taken,
        key,
        "digest-naming-it",
        None,
    )
    .await
}

/// フォルダーの中の生きているファイルの名前（名前順）。
async fn names_in(pool: &PgPool, parent: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT name FROM node WHERE parent_id = $1 AND deleted_at IS NULL ORDER BY name",
    )
    .bind(parent)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn journal_rows(pool: &PgPool, key: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM effect_journal WHERE idempotency_key = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 作ったファイルを消し、冪等キーの記録を外す（フォルダーは中身を消してから消す）。
async fn cleanup(c: &Ctx, ctx: &AuthContext, folders: &[Uuid], keys: &[String]) {
    for folder in folders {
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM node WHERE parent_id = $1 AND deleted_at IS NULL")
                .bind(folder)
                .fetch_all(&c.pool)
                .await
                .unwrap();
        for id in ids {
            let _ = c.service.soft_delete_file(ctx, id, None).await;
        }
        let _ = c.service.soft_delete_folder(ctx, *folder, None).await;
    }
    for key in keys {
        let _ = sqlx::query("DELETE FROM effect_journal WHERE idempotency_key = $1")
            .bind(key)
            .execute(&c.pool)
            .await;
    }
}

/// 番号の規則は入口によらず 1 つ: 失敗にする扱いは 1 回だけ試し、番号を付ける扱いは `名前 (2).md` から空いている
/// 名前で保存する。冪等キー付きの書込は、番号を付けても予約と記録は 1 つで、試し直しは前に付けた名前を返す。
#[tokio::test]
async fn every_entry_numbers_taken_names_by_one_rule() {
    let Some(c) = setup().await else { return };
    let org = format!("nameorg{}", Uuid::new_v4().simple());
    let uid = format!("nameuser{}", Uuid::new_v4().simple());
    let ctx = make_ctx(&org, &uid);
    seed_org_member(&c.authz, &org, &uid).await;
    let folder = c
        .service
        .create_folder(&ctx, None, "番号", None)
        .await
        .unwrap();
    let base = "週報.md";
    let keys: Vec<String> = (0..4)
        .map(|i| format!("wf:naming:{}:{i}", Uuid::new_v4().simple()))
        .collect();

    c.service
        .write_file_internal(&ctx, Some(folder.id), base, b"x", "text/markdown", None)
        .await
        .unwrap();

    // 失敗にする扱い: 名前の衝突で失敗し、予約も残さない（何も書かない）。
    let err = save(
        &c.service,
        &ctx,
        folder.id,
        base,
        OnNameTaken::Fail,
        &keys[0],
    )
    .await;
    assert!(matches!(err, Err(StorageError::Conflict)), "{err:?}");
    assert_eq!(journal_rows(&c.pool, &keys[0]).await, 0, "予約が残った");

    // 番号を付ける扱い: 空いている名前で保存し、記録には付けた名前が残る。
    let first = save(
        &c.service,
        &ctx,
        folder.id,
        base,
        OnNameTaken::AddNumber,
        &keys[1],
    )
    .await
    .unwrap();
    assert_eq!(first["name"], "週報 (2).md");
    assert_eq!(first["name"], numbered_name(base, 2).as_str());
    // 同じキーの試し直しは、扱いによらず前の結果を返し、保存し直さない。
    for policy in [OnNameTaken::AddNumber, OnNameTaken::Fail] {
        let again = save(&c.service, &ctx, folder.id, base, policy, &keys[1])
            .await
            .unwrap();
        assert_eq!(again, first, "{policy:?} の試し直しで結果が変わった");
    }

    // ドライブの新規作成・扱いを選ぶ新規作成も同じ規則で続きの番号を付ける。
    let drive = c
        .service
        .write_file_unique_internal(&ctx, Some(folder.id), base, b"x", "text/markdown", None)
        .await
        .unwrap();
    assert_eq!(drive.name, "週報 (3).md");
    let named = c
        .service
        .write_file_named(
            &ctx,
            Some(folder.id),
            base,
            b"x",
            "text/markdown",
            OnNameTaken::AddNumber,
            None,
        )
        .await
        .unwrap();
    assert_eq!(named.name, "週報 (4).md");
    let named_fail = c
        .service
        .write_file_named(
            &ctx,
            Some(folder.id),
            base,
            b"x",
            "text/markdown",
            OnNameTaken::Fail,
            None,
        )
        .await;
    assert!(
        matches!(named_fail, Err(StorageError::Conflict)),
        "{named_fail:?}"
    );

    // 拡張子の無い名前は末尾に付ける。
    for _ in 0..2 {
        c.service
            .write_file_unique_internal(&ctx, Some(folder.id), "メモ", b"x", "text/plain", None)
            .await
            .unwrap();
    }
    assert_eq!(
        names_in(&c.pool, folder.id).await,
        [
            "メモ",
            "メモ (2)",
            "週報 (2).md",
            "週報 (3).md",
            "週報 (4).md",
            "週報.md"
        ]
    );
    assert_eq!(journal_rows(&c.pool, &keys[1]).await, 1);

    cleanup(&c, &ctx, &[folder.id], &keys).await;
}

/// 上限（` (50)`）まで埋まっていれば、どの入口も名前の衝突で失敗し、予約を残さない。番号を付けた名前が長すぎる
/// ときは、名前の誤り（Invalid）にする（前の規則と同じ）。
#[tokio::test]
async fn numbering_stops_at_the_limit() {
    let Some(c) = setup().await else { return };
    let org = format!("nameorg{}", Uuid::new_v4().simple());
    let uid = format!("nameuser{}", Uuid::new_v4().simple());
    let ctx = make_ctx(&org, &uid);
    seed_org_member(&c.authz, &org, &uid).await;
    let folder = c
        .service
        .create_folder(&ctx, None, "上限", None)
        .await
        .unwrap();
    let keys = vec![format!("wf:naming:{}:full", Uuid::new_v4().simple())];

    for n in 1..=MAX_NUMBERED_NAME {
        let node = c
            .service
            .write_file_unique_internal(&ctx, Some(folder.id), "a.txt", b"x", "text/plain", None)
            .await
            .unwrap();
        assert_eq!(node.name, numbered_name("a.txt", n));
    }
    assert_eq!(numbered_name("a.txt", MAX_NUMBERED_NAME), "a (50).txt");

    let err = save(
        &c.service,
        &ctx,
        folder.id,
        "a.txt",
        OnNameTaken::AddNumber,
        &keys[0],
    )
    .await;
    assert!(matches!(err, Err(StorageError::Conflict)), "{err:?}");
    assert_eq!(journal_rows(&c.pool, &keys[0]).await, 0, "予約が残った");
    let err = c
        .service
        .write_file_unique_internal(&ctx, Some(folder.id), "a.txt", b"x", "text/plain", None)
        .await;
    assert!(matches!(err, Err(StorageError::Conflict)), "{err:?}");
    assert_eq!(
        names_in(&c.pool, folder.id).await.len(),
        MAX_NUMBERED_NAME as usize
    );

    // 255 文字ちょうどの名前に番号を付けると長すぎる。
    let long = format!("{}.md", "x".repeat(252));
    c.service
        .write_file_internal(&ctx, Some(folder.id), &long, b"x", "text/markdown", None)
        .await
        .unwrap();
    let err = c
        .service
        .write_file_unique_internal(&ctx, Some(folder.id), &long, b"x", "text/markdown", None)
        .await;
    assert!(matches!(err, Err(StorageError::Invalid(_))), "{err:?}");

    cleanup(&c, &ctx, &[folder.id], &keys).await;
}

/// 同じ名前へ同時に保存しても、番号を付ける扱いはそれぞれ別の空いている名前で保存する（名前の衝突だけを
/// 巻き戻して次の名前を試す）。同じ冪等キーの同時の書込は 1 つにまとまる。
#[tokio::test]
async fn concurrent_saves_get_distinct_numbers() {
    let Some(c) = setup().await else { return };
    let org = format!("nameorg{}", Uuid::new_v4().simple());
    let uid = format!("nameuser{}", Uuid::new_v4().simple());
    let ctx = make_ctx(&org, &uid);
    seed_org_member(&c.authz, &org, &uid).await;
    let folder = c
        .service
        .create_folder(&ctx, None, "同時", None)
        .await
        .unwrap();
    let keys: Vec<String> = (0..3)
        .map(|i| format!("wf:naming:{}:{i}", Uuid::new_v4().simple()))
        .collect();

    let (a, b) = tokio::join!(
        save(
            &c.service,
            &ctx,
            folder.id,
            "日報.md",
            OnNameTaken::AddNumber,
            &keys[0]
        ),
        save(
            &c.service,
            &ctx,
            folder.id,
            "日報.md",
            OnNameTaken::AddNumber,
            &keys[1]
        ),
    );
    let mut got = vec![
        a.unwrap()["name"].as_str().unwrap().to_string(),
        b.unwrap()["name"].as_str().unwrap().to_string(),
    ];
    got.sort();
    assert_eq!(got, ["日報 (2).md", "日報.md"]);

    let (a, b) = tokio::join!(
        save(
            &c.service,
            &ctx,
            folder.id,
            "日報.md",
            OnNameTaken::AddNumber,
            &keys[2]
        ),
        save(
            &c.service,
            &ctx,
            folder.id,
            "日報.md",
            OnNameTaken::AddNumber,
            &keys[2]
        ),
    );
    assert_eq!(
        a.unwrap(),
        b.unwrap(),
        "同じキーの同時の書込が 2 つになった"
    );
    assert_eq!(
        names_in(&c.pool, folder.id).await,
        ["日報 (2).md", "日報 (3).md", "日報.md"]
    );

    cleanup(&c, &ctx, &[folder.id], &keys).await;
}
