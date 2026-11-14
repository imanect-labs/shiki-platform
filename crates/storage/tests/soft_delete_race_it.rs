//! フォルダの論理削除と、その下への作成の競合（GAP-108 の下ごしらえ）の結合テスト。実 Postgres・RustFS・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する（未設定ならスキップ・
//! `SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! 「論理削除したフォルダの配下はすべて deleted_at を持つ」が、並行の作成で崩れないことを確かめる。
//! 作成は Tx の先頭で親（と祖先）を `FOR SHARE` で押さえ、論理削除は配下を `FOR NO KEY UPDATE` で押さえて
//! から印を付ける。順は advisory lock を別の接続で握って片方を止めて作り、止まった点は `pg_locks`・
//! `pg_blocking_pids` で確かめる（時間に頼らない）。
//!
//! - 作成が先に親を押さえていれば、論理削除は作成の commit を待ち、作ったファイルにも印を付ける。
//! - 論理削除が先に押さえていれば、作成は親の確かめ直しで NotFound になる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::sync::Arc;
use std::time::Duration;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Consistency, FgaObject, Principal, Relation};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::content_address::sha256_hex;
use storage::{object_store::S3Config, ObjectStore, S3ObjectStore, StorageError, StorageService};
use uuid::Uuid;

const TENANT: &str = "default";

struct Ctx {
    service: Arc<StorageService>,
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
        .max_connections(10)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");

    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: openfga_url,
            store_name: format!("shiki-soft-delete-race-it-{}", Uuid::new_v4().simple()),
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

    let service = Arc::new(StorageService::new(
        pool.clone(),
        store,
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
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
        authz::TenantId::parse(TENANT).unwrap(),
    )
}

/// 書いたタプルが check に見えるまで待つ。
async fn wait_check(c: &Ctx, ctx: &AuthContext, relation: Relation, obj: &FgaObject) {
    for _ in 0..200 {
        if c.authz
            .check(&ctx.subject(), relation, obj, Consistency::MinimizeLatency)
            .await
            .expect("check")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("seed したタプルが見えませんでした");
}

/// org のメンバーを seed する（ルートへの作成の認可は member@org）。
async fn new_member(c: &Ctx) -> AuthContext {
    let org = format!("sdorg{}", Uuid::new_v4().simple());
    let ctx = make_ctx(&org, &format!("sduser{}", Uuid::new_v4().simple()));
    let org_obj = ctx.ns().organization(&org);
    c.authz
        .write_tuple(&ctx.subject(), Relation::Member, &org_obj)
        .await
        .expect("member tuple seed");
    wait_check(c, &ctx, Relation::Member, &org_obj).await;
    ctx
}

/// フォルダを作り、その editor が check に見えるまで待つ。
async fn new_folder(c: &Ctx, ctx: &AuthContext, parent: Option<Uuid>, name: &str) -> Uuid {
    let folder = c
        .service
        .create_folder(ctx, parent, name, None)
        .await
        .expect("フォルダ");
    wait_check(
        c,
        ctx,
        Relation::Editor,
        &ctx.ns().folder(&folder.id.to_string()),
    )
    .await;
    folder.id
}

/// advisory lock を別の接続で握る（`input` は `hashtextextended('<prefix>' || input, 0)` の入力）。
async fn hold_advisory(
    pool: &PgPool,
    prefix: &str,
    input: &str,
) -> (sqlx::Transaction<'static, sqlx::Postgres>, i64) {
    let key: i64 = sqlx::query_scalar("SELECT hashtextextended($1::text || $2::text, 0)")
        .bind(prefix)
        .bind(input)
        .fetch_one(pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *tx)
        .await
        .unwrap();
    (tx, key)
}

/// 指定キーの advisory lock を待っている接続が現れるまで待ち、その pid を返す。
///
/// `classid = key >> 32` / `objid = key & 0xFFFFFFFF` / `objsubid = 1` は `pg_advisory_xact_lock(bigint)` の
/// `pg_locks` 表現（storage_it の await_advisory_wait と同じ）。キーで絞るので他のテストと並んでも取り違えない。
async fn await_advisory_waiter(pool: &PgPool, key: i64) -> i32 {
    for _ in 0..600 {
        let pid: Option<i32> = sqlx::query_scalar(
            "SELECT pid FROM pg_locks \
             WHERE locktype = 'advisory' AND NOT granted AND objsubid = 1 \
               AND classid = (($1::bigint >> 32) & 4294967295)::oid \
               AND objid = ($1::bigint & 4294967295)::oid \
             LIMIT 1",
        )
        .bind(key)
        .fetch_optional(pool)
        .await
        .expect("pg_locks");
        if let Some(pid) = pid {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("advisory lock を待つ接続が現れませんでした");
}

/// `holder_pid` の接続を待っている接続が現れるまで待つ（行ロックで止まった点を確かめる）。
async fn await_blocked_by(pool: &PgPool, holder_pid: i32) {
    for _ in 0..600 {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
        )
        .bind(holder_pid)
        .fetch_one(pool)
        .await
        .expect("pg_blocking_pids");
        if blocked {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("行ロックを待つ接続が現れませんでした（作成と論理削除が待ち合っていない）");
}

async fn is_deleted(c: &Ctx, ctx: &AuthContext, id: Uuid) -> bool {
    sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM node WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(ctx.tenant_id())
        .fetch_one(&c.pool)
        .await
        .expect("ノードを引く")
}

/// 作成が親を押さえている間に、その親（または祖先）の論理削除を始める。作成の commit の後、作ったファイルにも
/// deleted_at が付くこと（`target` を論理削除し、`parent` にファイルを作る）。
async fn create_holds_parent_then_soft_delete(
    c: &Ctx,
    ctx: &AuthContext,
    target: Uuid,
    parent: Uuid,
) {
    // 作成を blob のロック（bump_blob・`blob:<tenant>|<org>|<sha>`）で止める。作成は Tx の先頭で親を押さえた後に
    // blob のロックを取りに行くので、ここで止まった作成は親（と祖先）を押さえている。
    let bytes = format!("soft-delete-race {}", Uuid::new_v4()).into_bytes();
    let sha = sha256_hex(&bytes);
    let (holder, key) = hold_advisory(
        &c.pool,
        "blob:",
        &format!("{}|{}|{sha}", ctx.tenant_id(), ctx.org()),
    )
    .await;

    let create = {
        let svc = c.service.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            svc.write_file_internal(&ctx, Some(parent), "late.txt", &bytes, "text/plain", None)
                .await
        })
    };
    let creator_pid = await_advisory_waiter(&c.pool, key).await;

    let delete = {
        let svc = c.service.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { svc.soft_delete_folder(&ctx, target, None).await })
    };
    // 論理削除が、作成の押さえている行を待っている。
    await_blocked_by(&c.pool, creator_pid).await;

    holder.rollback().await.unwrap();
    let file = create
        .await
        .unwrap()
        .expect("作成は親が生きているうちに押さえたので通る");
    delete.await.unwrap().expect("論理削除");

    assert!(is_deleted(c, ctx, target).await, "論理削除したフォルダ");
    assert!(
        is_deleted(c, ctx, file.id).await,
        "論理削除が作成の commit を待ち、作ったファイルにも印を付ける"
    );
}

/// (1) フォルダへのファイルの作成が親の行を押さえている間に、そのフォルダの論理削除を始めると、作ったファイルにも
/// deleted_at が付く。
#[tokio::test]
async fn soft_delete_waits_for_create_holding_the_folder() {
    let Some(c) = setup().await else { return };
    let ctx = new_member(&c).await;
    let folder = new_folder(&c, &ctx, None, "f").await;
    create_holds_parent_then_soft_delete(&c, &ctx, folder, folder).await;
}

/// (3) 孫のフォルダ（論理削除するフォルダの子のフォルダ）への作成でも (1) が成り立つ。
#[tokio::test]
async fn soft_delete_waits_for_create_in_grandchild_folder() {
    let Some(c) = setup().await else { return };
    let ctx = new_member(&c).await;
    let folder = new_folder(&c, &ctx, None, "f").await;
    let child = new_folder(&c, &ctx, Some(folder), "g").await;
    create_holds_parent_then_soft_delete(&c, &ctx, folder, child).await;
    assert!(is_deleted(&c, &ctx, child).await, "子のフォルダ");
}

/// (2) 論理削除が先に配下を押さえていれば、作成は親の確かめ直しで NotFound になり、削除したフォルダの下に生きた
/// ファイルを残さない。
#[tokio::test]
async fn create_after_soft_delete_holds_the_folder_is_not_found() {
    let Some(c) = setup().await else { return };
    let ctx = new_member(&c).await;
    let folder = new_folder(&c, &ctx, None, "f").await;

    // 論理削除を監査のチェーンのロック（`audit_chain:<tenant>|<org>`）で止める。論理削除は配下を押さえて印を
    // 付けた後にチェーンのロックを取りに行くので、ここで止まった論理削除は配下を押さえている。
    let (holder, key) = hold_advisory(
        &c.pool,
        "audit_chain:",
        &format!("{}|{}", ctx.tenant_id(), ctx.org()),
    )
    .await;
    let delete = {
        let svc = c.service.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { svc.soft_delete_folder(&ctx, folder, None).await })
    };
    let deleter_pid = await_advisory_waiter(&c.pool, key).await;

    // 作成は Tx の外の確かめ（commit 済みの行では親は生きている）を通り、Tx の先頭の親の確かめ直しで待つ。
    let create = {
        let svc = c.service.clone();
        let ctx = ctx.clone();
        let bytes = format!("soft-delete-race {}", Uuid::new_v4()).into_bytes();
        tokio::spawn(async move {
            svc.write_file_internal(&ctx, Some(folder), "late.txt", &bytes, "text/plain", None)
                .await
        })
    };
    await_blocked_by(&c.pool, deleter_pid).await;

    holder.rollback().await.unwrap();
    delete.await.unwrap().expect("論理削除");
    let created = create.await.unwrap();
    assert!(
        matches!(created, Err(StorageError::NotFound)),
        "論理削除の commit の後の確かめ直しで NotFound: {created:?}"
    );
    let live_children: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM node WHERE parent_id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
    )
    .bind(folder)
    .bind(ctx.tenant_id())
    .fetch_one(&c.pool)
    .await
    .unwrap();
    assert_eq!(live_children, 0, "削除したフォルダの下に生きたノードが無い");
}
