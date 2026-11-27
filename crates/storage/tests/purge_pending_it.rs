//! 完全削除の片付け待ち（`storage_purge_pending`・GAP-108）の掃除と、blob のロック（`bump_blob`）の結合テスト。
//! 実 Postgres・RustFS・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する（未設定ならスキップ・
//! `SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! 片付け待ちの行は、完全削除（後続）の代わりに SQL で入れる。掃除はテナントを横断するので、別のテストの掃除が
//! 自分の行を先に片付けることがある。確かめは「自分の行が無くなるまで掃除を回す」の後に、結果（本体・タプル）で行う。
//! 書き込みと掃除の順は、行ロックを握った別の接続で書き込みを止めて作る（時間に頼らない）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Consistency, FgaObject, Principal, Relation};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::content_address::{blob_object_key, sha256_hex};
use storage::object_store::{ObjectStoreError, S3Config};
use storage::{ObjectStore, S3ObjectStore, StorageService};
use uuid::Uuid;

const TENANT: &str = "default";

/// 本物の S3 互換ストアへ渡しつつ、キーごとの `exists` の呼び出しを数える（bump_blob が行を新しく作ったとき
/// だけ本体を確かめ直すことを、外から見える形で確かめる）。
struct Counting {
    inner: Arc<dyn ObjectStore>,
    exists_calls: Mutex<HashMap<String, usize>>,
}

impl Counting {
    fn exists_count(&self, key: &str) -> usize {
        self.exists_calls
            .lock()
            .unwrap()
            .get(key)
            .copied()
            .unwrap_or(0)
    }
}

#[async_trait]
impl ObjectStore for Counting {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        self.inner.ensure_bucket().await
    }
    async fn presign_put(
        &self,
        key: &str,
        ttl: Duration,
        content_length: i64,
    ) -> Result<String, ObjectStoreError> {
        self.inner.presign_put(key, ttl, content_length).await
    }
    async fn presign_get(
        &self,
        key: &str,
        ttl: Duration,
        filename: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        self.inner
            .presign_get(key, ttl, filename, content_type)
            .await
    }
    async fn presign_get_internal(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<String, ObjectStoreError> {
        self.inner.presign_get_internal(key, ttl).await
    }
    async fn read_and_hash(&self, key: &str) -> Result<(String, u64), ObjectStoreError> {
        self.inner.read_and_hash(key).await
    }
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<(), ObjectStoreError> {
        self.inner.put_object(key, bytes, content_type).await
    }
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, ObjectStoreError> {
        self.inner.get_object(key).await
    }
    async fn exists(&self, key: &str) -> Result<bool, ObjectStoreError> {
        *self
            .exists_calls
            .lock()
            .unwrap()
            .entry(key.to_string())
            .or_default() += 1;
        self.inner.exists(key).await
    }
    async fn copy(&self, src_key: &str, dst_key: &str) -> Result<(), ObjectStoreError> {
        self.inner.copy(src_key, dst_key).await
    }
    async fn delete(&self, key: &str) -> Result<(), ObjectStoreError> {
        self.inner.delete(key).await
    }
    async fn list_prefix(
        &self,
        prefix: &str,
        continuation: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        self.inner.list_prefix(prefix, continuation).await
    }
    async fn delete_batch(&self, keys: &[String]) -> Result<(), ObjectStoreError> {
        self.inner.delete_batch(keys).await
    }
}

struct Ctx {
    service: Arc<StorageService>,
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    /// 数えない本物（確かめ・seed に使う）。
    raw_store: Arc<dyn ObjectStore>,
    counting: Arc<Counting>,
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
            store_name: format!("shiki-purge-pending-it-{}", Uuid::new_v4().simple()),
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
    let raw_store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    raw_store.ensure_bucket().await.expect("バケット準備");
    let counting = Arc::new(Counting {
        inner: raw_store.clone(),
        exists_calls: Mutex::new(HashMap::new()),
    });

    let service = Arc::new(StorageService::new(
        pool.clone(),
        counting.clone(),
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    Some(Ctx {
        service,
        pool,
        authz,
        raw_store,
        counting,
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

/// org のメンバーを seed し、見えるまで待つ（作成の認可は member@org）。
async fn new_member(c: &Ctx) -> AuthContext {
    let org = format!("pporg{}", Uuid::new_v4().simple());
    let ctx = make_ctx(&org, &format!("ppuser{}", Uuid::new_v4().simple()));
    let org_obj = ctx.ns().organization(&org);
    c.authz
        .write_tuple(&ctx.subject(), Relation::Member, &org_obj)
        .await
        .expect("member tuple seed");
    wait_check(c, &ctx, Relation::Member, &org_obj).await;
    ctx
}

/// 書いたタプルが check に見えるまで待つ（作成の後の書き込みの認可に使う）。
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

/// 他と重ならない内容（新しい blob になる）。
fn fresh_bytes(tag: &str) -> Vec<u8> {
    format!("purge-pending-it {tag} {}", Uuid::new_v4()).into_bytes()
}

/// `object` の片付け待ちを 1 行入れる（完全削除の代わり）。
async fn enqueue_object(c: &Ctx, tenant: &str, org: &str, sha: &str, key: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO storage_purge_pending (tenant_id, org, kind, sha256, object_key) \
         VALUES ($1, $2, 'object', $3, $4) RETURNING id",
    )
    .bind(tenant)
    .bind(org)
    .bind(sha)
    .bind(key)
    .fetch_one(&c.pool)
    .await
    .expect("片付け待ち（object）を入れる")
}

/// `node_tuples` の片付け待ちを 1 行入れる（完全削除の代わり）。
async fn enqueue_node_tuples(
    c: &Ctx,
    tenant: &str,
    org: &str,
    node_id: Uuid,
    node_kind: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO storage_purge_pending (tenant_id, org, kind, node_id, node_kind) \
         VALUES ($1, $2, 'node_tuples', $3, $4) RETURNING id",
    )
    .bind(tenant)
    .bind(org)
    .bind(node_id)
    .bind(node_kind)
    .fetch_one(&c.pool)
    .await
    .expect("片付け待ち（node_tuples）を入れる")
}

async fn pending_exists(c: &Ctx, tenant: &str, id: Uuid) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM storage_purge_pending WHERE tenant_id = $1 AND id = $2)",
    )
    .bind(tenant)
    .bind(id)
    .fetch_one(&c.pool)
    .await
    .expect("片付け待ちの行を引く")
}

/// 自分の行が片付くまで掃除を回す（別のテストの掃除が先に取っていれば、それが終わるのを待つ）。
///
/// 1 回の掃除には上限の時間を置く。書き込みが blob のロックを取ってから node の行を待つ（ロックの順が
/// node → blob でない）と、テストが押さえた行の後ろで掃除が blob のロックを待ち続けて止まるので、止まらずに落とす。
async fn sweep_until_gone(c: &Ctx, tenant: &str, id: Uuid) {
    for _ in 0..200 {
        tokio::time::timeout(Duration::from_secs(60), c.service.sweep_purge_pending())
            .await
            .expect("掃除が blob のロックで止まった（書き込みのロックの順が node → blob になっていない）")
            .expect("掃除が DB に届くこと");
        if !pending_exists(c, tenant, id).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("片付け待ちの行が片付きませんでした");
}

/// `holder_pid` の接続を待っている接続が現れるまで待つ（行ロックで止めた点を確かめる同期点）。
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
    panic!("行ロックを待つ接続が現れませんでした");
}

async fn blob_refcount(c: &Ctx, ctx: &AuthContext, sha: &str) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT refcount FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(sha)
    .fetch_optional(&c.pool)
    .await
    .expect("blob の行を引く")
}

/// blob の行が無い本体は、掃除が消す（片付けた行も消える）。
#[tokio::test]
async fn sweep_deletes_body_without_blob_row() {
    let Some(c) = setup().await else { return };
    let ctx = new_member(&c).await;
    let bytes = fresh_bytes("orphan");
    let sha = sha256_hex(&bytes);
    let key = blob_object_key(ctx.tenant_id(), ctx.org(), &sha);
    c.raw_store
        .put_object(&key, bytes, "text/plain")
        .await
        .expect("本体を置く");
    assert_eq!(blob_refcount(&c, &ctx, &sha).await, None, "blob の行は無い");

    let id = enqueue_object(&c, ctx.tenant_id(), ctx.org(), &sha, &key).await;
    sweep_until_gone(&c, ctx.tenant_id(), id).await;

    assert!(
        !c.raw_store.exists(&key).await.unwrap(),
        "blob の行が無い本体は消える"
    );
}

/// blob の行がある（同じ内容が生きている）本体は消さない。片付け待ちの行は片付く。
#[tokio::test]
async fn sweep_keeps_body_while_blob_row_exists() {
    let Some(c) = setup().await else { return };
    let ctx = new_member(&c).await;
    let bytes = fresh_bytes("live");
    let sha = sha256_hex(&bytes);
    let key = blob_object_key(ctx.tenant_id(), ctx.org(), &sha);
    let node = c
        .service
        .write_file_internal(&ctx, None, "live.txt", &bytes, "text/plain", None)
        .await
        .expect("書き込み");

    let id = enqueue_object(&c, ctx.tenant_id(), ctx.org(), &sha, &key).await;
    sweep_until_gone(&c, ctx.tenant_id(), id).await;

    assert!(c.raw_store.exists(&key).await.unwrap(), "本体は残る");
    let (_, read) = c
        .service
        .read_file_internal(&ctx, node.id, None)
        .await
        .expect("読める");
    assert_eq!(read, bytes);
    assert_eq!(blob_refcount(&c, &ctx, &sha).await, Some(1));
}

/// 書き込みが本体を置いた後・blob の行を作る前に掃除が本体を消しても、書き込みは行を作るときにロックの中で
/// 本体を確かめ直して置き直すので、書いたファイルが読める。
///
/// 順は、配置先のフォルダの行を別の接続で押さえて作る。書き込みは Tx の外で本体を置いてから、Tx の先頭の
/// 親の確かめ直し（`FOR SHARE`）で止まる。その間に掃除を最後まで回して本体を消し、押さえを外す。
#[tokio::test]
async fn write_racing_sweep_restores_body_inside_blob_lock() {
    let Some(c) = setup().await else { return };
    let ctx = new_member(&c).await;
    let folder = c
        .service
        .create_folder(&ctx, None, "race", None)
        .await
        .expect("フォルダ");
    wait_check(
        &c,
        &ctx,
        Relation::Editor,
        &ctx.ns().folder(&folder.id.to_string()),
    )
    .await;
    let bytes = fresh_bytes("race");
    let sha = sha256_hex(&bytes);
    let key = blob_object_key(ctx.tenant_id(), ctx.org(), &sha);

    // 配置先のフォルダの行を押さえる。
    let mut holder = c.pool.begin().await.unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM node WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
        .bind(folder.id)
        .bind(ctx.tenant_id())
        .fetch_one(&mut *holder)
        .await
        .unwrap();

    let writer = {
        let svc = c.service.clone();
        let ctx = ctx.clone();
        let bytes = bytes.clone();
        tokio::spawn(async move {
            svc.write_file_internal(
                &ctx,
                Some(folder.id),
                "race.txt",
                &bytes,
                "text/plain",
                None,
            )
            .await
        })
    };
    // 書き込みが本体を置き、親の確かめ直しで止まった。
    await_blocked_by(&c.pool, holder_pid).await;
    assert!(
        c.raw_store.exists(&key).await.unwrap(),
        "書き込みは Tx の外で本体を置いている"
    );

    // 完全削除の片付けが、行の無い本体を消す（書き込みはまだ blob の行を作っていない）。
    let id = enqueue_object(&c, ctx.tenant_id(), ctx.org(), &sha, &key).await;
    sweep_until_gone(&c, ctx.tenant_id(), id).await;
    assert!(
        !c.raw_store.exists(&key).await.unwrap(),
        "掃除が本体を消した"
    );

    // 押さえを外すと、書き込みは blob の行を新しく作り、ロックの中で本体を置き直す。
    holder.rollback().await.unwrap();
    let node = writer.await.unwrap().expect("書き込み");
    let (_, read) = c
        .service
        .read_file_internal(&ctx, node.id, None)
        .await
        .expect("書いたファイルが読める");
    assert_eq!(read, bytes);
    assert_eq!(blob_refcount(&c, &ctx, &sha).await, Some(1));
}

/// bump_blob は blob の行を新しく作ったときだけ本体を確かめ直す（created = true）。既にある行への参照の追加
/// （created = false）では確かめない。本体の確かめ（`exists`）の回数で見る。
#[tokio::test]
async fn bump_blob_reports_created_only_for_new_rows() {
    let Some(c) = setup().await else { return };
    let ctx = new_member(&c).await;

    // 新規作成（internal_io）。
    let first = fresh_bytes("created");
    let first_key = blob_object_key(ctx.tenant_id(), ctx.org(), &sha256_hex(&first));
    let a = c
        .service
        .write_file_internal(&ctx, None, "a.txt", &first, "text/plain", None)
        .await
        .expect("a");
    assert_eq!(
        c.counting.exists_count(&first_key),
        1,
        "行を新しく作った（created）"
    );
    let b = c
        .service
        .write_file_internal(&ctx, None, "b.txt", &first, "text/plain", None)
        .await
        .expect("b");
    assert_eq!(
        c.counting.exists_count(&first_key),
        1,
        "既にある行に参照を足しただけ（created でない）"
    );
    assert_eq!(blob_refcount(&c, &ctx, &sha256_hex(&first)).await, Some(2));

    // 内容更新（content_update）。
    wait_check(
        &c,
        &ctx,
        Relation::Editor,
        &ctx.ns().file(&a.id.to_string()),
    )
    .await;
    wait_check(
        &c,
        &ctx,
        Relation::Editor,
        &ctx.ns().file(&b.id.to_string()),
    )
    .await;
    let second = fresh_bytes("updated");
    let second_key = blob_object_key(ctx.tenant_id(), ctx.org(), &sha256_hex(&second));
    c.service
        .update_file_content_internal(&ctx, a.id, &second, "text/plain", None)
        .await
        .expect("a の内容更新");
    assert_eq!(c.counting.exists_count(&second_key), 1, "created");
    c.service
        .update_file_content_internal(&ctx, b.id, &second, "text/plain", None)
        .await
        .expect("b の内容更新");
    assert_eq!(c.counting.exists_count(&second_key), 1, "created でない");
    assert_eq!(blob_refcount(&c, &ctx, &sha256_hex(&second)).await, Some(2));

    // 過去の版の復元は既にある行に参照を足すだけ（本体は確かめない）。
    c.service
        .restore_version(&ctx, a.id, 1, None)
        .await
        .expect("版の復元");
    assert_eq!(
        c.counting.exists_count(&first_key),
        1,
        "復元は created でない"
    );
    assert_eq!(blob_refcount(&c, &ctx, &sha256_hex(&first)).await, Some(3));
}

/// `node_tuples` の行は、そのノードに直接張られたタプルをすべて外す（ファイルとフォルダ）。
#[tokio::test]
async fn sweep_removes_node_tuples() {
    let Some(c) = setup().await else { return };
    let ctx = new_member(&c).await;
    let folder = c
        .service
        .create_folder(&ctx, None, "tuples", None)
        .await
        .expect("フォルダ");
    let folder_obj = ctx.ns().folder(&folder.id.to_string());
    wait_check(&c, &ctx, Relation::Editor, &folder_obj).await;
    let file = c
        .service
        .write_file_internal(
            &ctx,
            Some(folder.id),
            "t.txt",
            &fresh_bytes("tuples"),
            "text/plain",
            None,
        )
        .await
        .expect("ファイル");
    let file_obj = ctx.ns().file(&file.id.to_string());
    assert!(
        !c.authz
            .read_tuples(&file_obj, None)
            .await
            .unwrap()
            .is_empty(),
        "作成で owner と parent のタプルがある"
    );
    assert!(!c
        .authz
        .read_tuples(&folder_obj, None)
        .await
        .unwrap()
        .is_empty());

    let file_row = enqueue_node_tuples(&c, ctx.tenant_id(), ctx.org(), file.id, "file").await;
    let folder_row = enqueue_node_tuples(&c, ctx.tenant_id(), ctx.org(), folder.id, "folder").await;
    sweep_until_gone(&c, ctx.tenant_id(), file_row).await;
    sweep_until_gone(&c, ctx.tenant_id(), folder_row).await;

    assert!(
        c.authz
            .read_tuples(&file_obj, None)
            .await
            .unwrap()
            .is_empty(),
        "ファイルのタプルが外れた"
    );
    assert!(
        c.authz
            .read_tuples(&folder_obj, None)
            .await
            .unwrap()
            .is_empty(),
        "フォルダのタプルが外れた"
    );
}

/// 片付けに失敗した行は消さず、attempts と last_error を残して次の回に回す。
#[tokio::test]
async fn failed_row_keeps_attempts_and_last_error() {
    let Some(c) = setup().await else { return };
    // 文字ポリシーに反する tenant_id（`|` を含む）では名前空間を組まない（fail-closed）。
    let bad_tenant = format!("bad|{}", Uuid::new_v4().simple());
    let id = enqueue_node_tuples(&c, &bad_tenant, "org1", Uuid::new_v4(), "file").await;

    let mut kept = None;
    for _ in 0..200 {
        c.service
            .sweep_purge_pending()
            .await
            .expect("掃除が DB に届くこと");
        let row: (i32, Option<String>) = sqlx::query_as(
            "SELECT attempts, last_error FROM storage_purge_pending WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&bad_tenant)
        .bind(id)
        .fetch_one(&c.pool)
        .await
        .expect("失敗した行は残る");
        if row.0 >= 1 {
            kept = Some(row);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (attempts, last_error) = kept.expect("attempts が進む");
    assert!(attempts >= 1);
    assert!(
        last_error.is_some_and(|e| e.contains("tenant_id")),
        "last_error に理由が残る"
    );

    sqlx::query("DELETE FROM storage_purge_pending WHERE tenant_id = $1 AND id = $2")
        .bind(&bad_tenant)
        .bind(id)
        .execute(&c.pool)
        .await
        .unwrap();
}
