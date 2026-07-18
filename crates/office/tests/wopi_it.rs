//! WOPI ホストの結合テスト（Task 11.6・実 Postgres が必要）。
//!
//! 検証（受け入れ条件）:
//! - CheckFileInfo/GetFile/PutFile の正常系（PutFile→node.version+1・outbox・監査）
//! - 共有解除の即時反映（トークンが生きていても次の呼び出しで 404）
//! - テナント境界（他テナントのファイルへトークンを流用できない）
//! - ロック（LOCK→他者 lock_id の PutFile 409→UNLOCK→成功・期限切れは無視）
//! - viewer のみ（GetFile 可・PutFile 403）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::Router;
use office::{build_wopi_router, OfficeTokenKey, WopiState};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{Node, StorageService};
use tower::ServiceExt;
use uuid::Uuid;

/// (subject, relation) 付与集合を持つ authz モック（revoke 可能）。
struct RoleAuthz {
    grants: Mutex<HashSet<(String, Relation)>>,
}
impl RoleAuthz {
    fn new() -> Self {
        RoleAuthz {
            grants: Mutex::new(HashSet::new()),
        }
    }
    fn grant(&self, subject: &Subject, relation: Relation) {
        self.grants
            .lock()
            .unwrap()
            .insert((subject.as_str().to_string(), relation));
    }
    /// 共有解除（relation 剥奪）を模す。
    fn revoke(&self, subject: &Subject, relation: Relation) {
        self.grants
            .lock()
            .unwrap()
            .remove(&(subject.as_str().to_string(), relation));
    }
}
#[async_trait]
impl AuthzClient for RoleAuthz {
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(self
            .grants
            .lock()
            .unwrap()
            .contains(&(subject.as_str().to_string(), relation)))
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// バイト保持の in-memory ObjectStore。
#[derive(Default)]
struct MemStore {
    objects: Mutex<std::collections::HashMap<String, Vec<u8>>>,
}
#[async_trait]
impl storage::object_store::ObjectStore for MemStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _: &str,
        _: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn presign_put(
        &self,
        _: &str,
        _: Duration,
        _: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn read_and_hash(&self, _: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("mem".into()))
    }
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        _: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        self.objects.lock().unwrap().insert(key.into(), bytes);
        Ok(())
    }
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| storage::ObjectStoreError::NotFound(key.into()))
    }
    async fn exists(&self, key: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(self.objects.lock().unwrap().contains_key(key))
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

fn ctx_for(user: &str, tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec!["/acme".into()],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

struct Env {
    storage: Arc<StorageService>,
    authz: Arc<RoleAuthz>,
    pool: PgPool,
    key: OfficeTokenKey,
    router: Router,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let authz = Arc::new(RoleAuthz::new());
    let authz_dyn: Arc<dyn AuthzClient> = authz.clone();
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        Arc::new(MemStore::default()),
        authz_dyn.clone(),
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));
    let key = OfficeTokenKey::random();
    let router = build_wopi_router(WopiState {
        storage: storage.clone(),
        authz: authz_dyn,
        pool: pool.clone(),
        token_key: key.clone(),
        web_origin: Some("http://localhost:3000".into()),
        max_body_bytes: 64 * 1024 * 1024,
    });
    Some(Env {
        storage,
        authz,
        pool,
        key,
        router,
    })
}

/// docx 相当のファイルを作成し node を返す（内容にノンスを混ぜ blob dedup を避ける）。
async fn create_docx(env: &Env, owner: &AuthContext, body: &str) -> Node {
    env.authz.grant(&owner.subject(), Relation::Member);
    env.authz.grant(&owner.subject(), Relation::Editor);
    env.authz.grant(&owner.subject(), Relation::Viewer);
    let name = format!("doc-{}.docx", Uuid::new_v4());
    let bytes = format!("{body}:nonce:{}", Uuid::new_v4());
    env.storage
        .write_file_internal(
            owner,
            None,
            &name,
            bytes.as_bytes(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            None,
        )
        .await
        .expect("create docx")
}

fn token_for(env: &Env, ctx: &AuthContext, file_id: Uuid) -> String {
    office::wopi::token::issue(&env.key, ctx, file_id).expect("issue token")
}

async fn send(env: &Env, req: Request<Body>) -> Response<Body> {
    env.router.clone().oneshot(req).await.expect("oneshot")
}

async fn body_json(res: Response<Body>) -> serde_json::Value {
    use http_body_util::BodyExt;
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn body_bytes(res: Response<Body>) -> Vec<u8> {
    use http_body_util::BodyExt;
    res.into_body().collect().await.unwrap().to_bytes().to_vec()
}

fn get_req(file_id: Uuid, token: &str, contents: bool) -> Request<Body> {
    let suffix = if contents { "/contents" } else { "" };
    Request::builder()
        .method("GET")
        .uri(format!(
            "/wopi/files/{file_id}{suffix}?access_token={token}"
        ))
        .body(Body::empty())
        .unwrap()
}

fn put_req(file_id: Uuid, token: &str, body: &str, lock: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(format!(
        "/wopi/files/{file_id}/contents?access_token={token}"
    ));
    if let Some(lock) = lock {
        builder = builder.header("X-WOPI-Lock", lock);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn lock_req(file_id: Uuid, token: &str, operation: &str, lock: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/wopi/files/{file_id}?access_token={token}"))
        .header("X-WOPI-Override", operation);
    if let Some(lock) = lock {
        builder = builder.header("X-WOPI-Lock", lock);
    }
    builder.body(Body::empty()).unwrap()
}

fn header<'a>(res: &'a Response<Body>, name: &str) -> Option<&'a str> {
    res.headers().get(name).and_then(|v| v.to_str().ok())
}

/// 受け入れ条件: CheckFileInfo/GetFile/PutFile の正常系。
/// PutFile で node.version が進み、outbox に update イベント・監査記録が残る。
#[tokio::test]
async fn checkfileinfo_getfile_putfile_happy_path() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice, "v1-content").await;
    let token = token_for(&env, &alice, node.id);

    // CheckFileInfo（editor なので UserCanWrite=true・PostMessageOrigin は設定値）。
    let res = send(&env, get_req(node.id, &token, false)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let info = body_json(res).await;
    assert_eq!(info["BaseFileName"], node.name.as_str());
    assert_eq!(info["Version"], "1");
    assert_eq!(info["UserCanWrite"], true);
    assert_eq!(info["SupportsLocks"], true);
    assert_eq!(info["SupportsUpdate"], true);
    assert_eq!(info["PostMessageOrigin"], "http://localhost:3000");
    assert!(info["Size"].as_i64().unwrap() > 0);

    // GetFile（StorageService 経由・X-WOPI-ItemVersion 付き）。
    let res = send(&env, get_req(node.id, &token, true)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(header(&res, "x-wopi-itemversion"), Some("1"));
    let bytes = body_bytes(res).await;
    assert!(String::from_utf8_lossy(&bytes).contains("v1-content"));

    // PutFile → 新バージョン。
    let new_body = format!("v2-content:nonce:{}", Uuid::new_v4());
    let res = send(&env, put_req(node.id, &token, &new_body, None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(header(&res, "x-wopi-itemversion"), Some("2"));

    // node.version が進み、読み戻しが新内容になる。
    let (meta, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("read back");
    assert_eq!(meta.version, 2);
    assert_eq!(String::from_utf8_lossy(&bytes), new_body);

    // 書込イベント outbox（RAG 再索引の入口）と監査記録が同一 txn で残っている。
    let (outbox_count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM storage_event_outbox WHERE node_id = $1 AND op = 'update'",
    )
    .bind(node.id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(outbox_count, 1, "update イベントが outbox に載る");
    let (audit_count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_log \
         WHERE object_id = $1 AND action = 'file.write.content' AND decision = 'allow'",
    )
    .bind(node.id.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(audit_count, 1, "書込の監査記録が残る");
}

/// 受け入れ条件: 共有解除が次の WOPI 呼び出しで即時反映される
/// （トークンが有効期限内でも relation 剥奪で 404）。
#[tokio::test]
async fn revocation_takes_effect_on_next_call() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice, "shared").await;

    // bob に editor/viewer を付与 → トークン発行 → アクセスできる。
    let bob = ctx_for("bob", "default");
    env.authz.grant(&bob.subject(), Relation::Editor);
    env.authz.grant(&bob.subject(), Relation::Viewer);
    let token = token_for(&env, &bob, node.id);
    let res = send(&env, get_req(node.id, &token, true)).await;
    assert_eq!(res.status(), StatusCode::OK);

    // 共有解除（editor/viewer 剥奪）→ 同じトークンでも次の呼び出しから拒否。
    env.authz.revoke(&bob.subject(), Relation::Editor);
    env.authz.revoke(&bob.subject(), Relation::Viewer);
    let res = send(&env, get_req(node.id, &token, true)).await;
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "GetFile が存在秘匿の 404"
    );
    let res = send(&env, put_req(node.id, &token, "should-not-write", None)).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "PutFile も 404");
    // 内容が書き換わっていない。
    let (meta, _) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .unwrap();
    assert_eq!(meta.version, 1);
}

/// 受け入れ条件: 他テナントのファイルにトークンを流用できない。
#[tokio::test]
async fn tenant_boundary_is_enforced() {
    let Some(env) = setup().await else { return };
    // tenant B にファイルを作る。
    let bob_b = ctx_for("bob", "tenant-b");
    let node_b = create_docx(&env, &bob_b, "tenant-b-secret").await;
    // tenant A の alice（自分のファイルには正当にアクセスできる主体）。
    let alice_a = ctx_for("alice", "tenant-a");
    let node_a = create_docx(&env, &alice_a, "tenant-a-doc").await;

    // ① 正当な alice のトークン（file_id=A）で B のファイル ID を指定 →
    //    クレームの file_id 固定により検証段階で 401（構造的に不可）。
    let token_a = token_for(&env, &alice_a, node_a.id);
    let res = send(&env, get_req(node_b.id, &token_a, true)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // ② 仮に tenant A の主体が B のファイル ID を主張するトークンを得ても、
    //    テナント焼き込み（subject が user:tenant-a|alice になる）により ReBAC で
    //    不許可 → 存在秘匿の 404。
    let forged = office::wopi::token::issue(&env.key, &alice_a, node_b.id).unwrap();
    let res = send(&env, get_req(node_b.id, &forged, true)).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = send(
        &env,
        put_req(node_b.id, &forged, "cross-tenant-write", None),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // ③ でたらめなトークンは 401。
    let res = send(&env, get_req(node_b.id, "garbage.token", true)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// ロック: LOCK→他者 lock_id の PutFile 409→UNLOCK→PutFile 成功。期限切れは無視。
#[tokio::test]
async fn lock_protocol_and_lazy_expiry() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice, "lockable").await;
    let token = token_for(&env, &alice, node.id);

    // LOCK L1。
    let res = send(&env, lock_req(node.id, &token, "LOCK", Some("L1"))).await;
    assert_eq!(res.status(), StatusCode::OK);
    // GET_LOCK で L1 が見える。
    let res = send(&env, lock_req(node.id, &token, "GET_LOCK", None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(header(&res, "x-wopi-lock"), Some("L1"));

    // 他者 lock_id での PutFile は 409＋X-WOPI-Lock に現 lock_id（WOPI 準拠）。
    let res = send(&env, put_req(node.id, &token, "conflict-write", Some("L2"))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(header(&res, "x-wopi-lock"), Some("L1"));
    // lock_id 無しの PutFile もロック中は 409。
    let res = send(&env, put_req(node.id, &token, "conflict-write", None)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 保持者（L1）の PutFile は成功。
    let body = format!("locked-write:{}", Uuid::new_v4());
    let res = send(&env, put_req(node.id, &token, &body, Some("L1"))).await;
    assert_eq!(res.status(), StatusCode::OK);

    // REFRESH_LOCK は一致時のみ成功。
    let res = send(&env, lock_req(node.id, &token, "REFRESH_LOCK", Some("L2"))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let res = send(&env, lock_req(node.id, &token, "REFRESH_LOCK", Some("L1"))).await;
    assert_eq!(res.status(), StatusCode::OK);

    // UNLOCK: 不一致は 409（現 lock_id 付き）→ 一致で解除 → 以後はロック無しで書ける。
    let res = send(&env, lock_req(node.id, &token, "UNLOCK", Some("L2"))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(header(&res, "x-wopi-lock"), Some("L1"));
    let res = send(&env, lock_req(node.id, &token, "UNLOCK", Some("L1"))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = format!("unlocked-write:{}", Uuid::new_v4());
    let res = send(&env, put_req(node.id, &token, &body, None)).await;
    assert_eq!(res.status(), StatusCode::OK);

    // 期限切れロックは lazy 解放（次アクセスで無視・削除）。
    sqlx::query(
        "INSERT INTO office_lock (file_id, lock_id, locked_by, tenant_id, expires_at) \
         VALUES ($1, 'stale', 'user:default|alice', 'default', now() - interval '1 minute')",
    )
    .bind(node.id)
    .execute(&env.pool)
    .await
    .unwrap();
    let res = send(&env, lock_req(node.id, &token, "GET_LOCK", None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        header(&res, "x-wopi-lock"),
        Some(""),
        "期限切れは無いものとして扱う"
    );
    let body = format!("after-stale:{}", Uuid::new_v4());
    let res = send(&env, put_req(node.id, &token, &body, None)).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "期限切れロックは書込を妨げない"
    );
}

/// viewer のみの主体: GetFile 可・PutFile/ロック操作は 403。
#[tokio::test]
async fn viewer_can_read_but_not_write() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice, "readonly").await;

    let carol = ctx_for("carol", "default");
    env.authz.grant(&carol.subject(), Relation::Viewer);
    let token = token_for(&env, &carol, node.id);

    // CheckFileInfo: UserCanWrite=false。
    let res = send(&env, get_req(node.id, &token, false)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let info = body_json(res).await;
    assert_eq!(info["UserCanWrite"], false);

    // GetFile は読める。
    let res = send(&env, get_req(node.id, &token, true)).await;
    assert_eq!(res.status(), StatusCode::OK);

    // PutFile・ロック操作は 403（読める主体には存在を隠す意味が無い）。
    let res = send(&env, put_req(node.id, &token, "viewer-write", None)).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = send(&env, lock_req(node.id, &token, "LOCK", Some("V1"))).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 版が進んでいない。
    let (meta, _) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .unwrap();
    assert_eq!(meta.version, 1);
}

/// current_lock（Task 11.8 の判定入口）: ロック中のみ Some。
#[tokio::test]
async fn current_lock_reflects_session() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice, "session-probe").await;
    let token = token_for(&env, &alice, node.id);

    assert!(office::current_lock(&env.pool, "default", node.id)
        .await
        .unwrap()
        .is_none());
    let res = send(&env, lock_req(node.id, &token, "LOCK", Some("S1"))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let lock = office::current_lock(&env.pool, "default", node.id)
        .await
        .unwrap()
        .expect("ロック中は Some");
    assert_eq!(lock.lock_id, "S1");
    assert_eq!(lock.locked_by, alice.subject().as_str());
    // 他テナントからは見えない（tenant スコープ強制）。
    assert!(office::current_lock(&env.pool, "tenant-b", node.id)
        .await
        .unwrap()
        .is_none());
}
