//! collab WebSocket の結合テスト（実 axum サーバ＋実 WS クライアント＋実 Postgres）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行し、未設定ならスキップする。
//! 受け入れ条件（Task 11P.1）を y-websocket 互換ワイヤの実接続で検証する:
//! - 2 クライアントの並行編集が収束する（オフライン→再接続含む）
//! - viewer は読めるが書けない
//! - 共有解除（relation 剥奪）で接続が切断される
//! - relation が無い主体は HTTP（存在秘匿の 404）で拒否される

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
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
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionRecord, SessionStore},
    state::AppState,
};
use async_trait::async_trait;
use authz::{
    AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal, ReadTupleKey, Relation,
    Subject,
};
use futures::{SinkExt, StreamExt};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as TgMessage;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, Text, Transact, Update};

/// (subject, relation) の付与集合を実行中に増減できる authz モック（剥奪切断の検証用）。
struct RoleAuthz {
    grants: RwLock<HashSet<(String, Relation)>>,
}

impl RoleAuthz {
    fn new() -> Self {
        RoleAuthz {
            grants: RwLock::new(HashSet::new()),
        }
    }

    fn grant(&self, subject: &Subject, relation: Relation) {
        self.grants
            .write()
            .unwrap()
            .insert((subject.as_str().to_string(), relation));
    }

    fn revoke_all(&self, subject: &Subject) {
        self.grants
            .write()
            .unwrap()
            .retain(|(s, _)| s != subject.as_str());
    }
}

#[async_trait]
impl AuthzClient for RoleAuthz {
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        _object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(self
            .grants
            .read()
            .unwrap()
            .contains(&(subject.as_str().to_string(), relation)))
    }

    async fn write_tuple(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }

    async fn delete_tuple(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }

    async fn read_tuples(
        &self,
        _object: &FgaObject,
        _relation: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }

    async fn list_objects(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }

    async fn delete_object_tuples(&self, _object: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }

    async fn read_subject_objects(
        &self,
        _subject: &Subject,
        _object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// ストレージのバイト層スタブ（collab はメタデータしか読まない）。
struct FakeStore;

#[async_trait]
impl storage::object_store::ObjectStore for FakeStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _key: &str,
        _ttl: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://fake/internal".into())
    }
    async fn presign_put(
        &self,
        _key: &str,
        _ttl: Duration,
        _content_length: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://test/put".into())
    }
    async fn presign_get(
        &self,
        _key: &str,
        _ttl: Duration,
        _filename: Option<&str>,
        _content_type: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://test/get".into())
    }
    async fn read_and_hash(&self, _key: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("test".into()))
    }
    async fn put_object(
        &self,
        _key: &str,
        _bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn get_object(&self, _key: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("test".into()))
    }
    async fn exists(&self, _key: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _src: &str, _dst: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _prefix: &str,
        _continuation: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _keys: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

fn base_config(db_url: &str) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            cors_allowed_origins: vec![],
        },
        database: DatabaseConfig {
            url: db_url.into(),
            max_connections: 5,
        },
        auth: AuthConfig {
            issuer: "http://localhost/realms/shiki".into(),
            internal_base_url: None,
            jwks_uri: None,
            audience: "shiki-api".into(),
            jwks_ttl_secs: 300,
            client_id: "shiki-web".into(),
            client_secret: None,
            redirect_uri: "http://localhost:3000/auth/callback".into(),
            post_logout_redirect_uri: "http://localhost:3000/".into(),
            scopes: "openid profile".into(),
            tenancy: Tenancy::Single,
            tenant_id: Some("default".into()),
            provisioner_client_id: None,
            provisioner_client_secret: None,
            admin_base_url: None,
        },
        authz: AuthzConfig {
            base_url: "http://localhost:8080".into(),
            store_name: "shiki".into(),
        },
        session: SessionConfig {
            redis_url: "redis://localhost:6379".into(),
            ttl_secs: 86400,
            secure: false,
            refresh_leeway_secs: 60,
        },
        telemetry: TelemetryConfig {
            otlp_endpoint: None,
            service_name: "test".into(),
            log_format: LogFormat::Json,
        },
        storage: StorageConfig {
            backend: ObjectStoreBackend::Minio,
            s3: None,
            max_upload_size_bytes: 5 * 1024 * 1024 * 1024,
        },
        vector: VectorConfig {
            backend: VectorStoreBackend::Qdrant,
        },
        rag: rag::RagConfig::default(),
        llm: LlmConfig {
            backend: LlmBackend::Vllm,
            base_url: None,
            api_key: None,
            default_model: None,
            models: vec![],
            langfuse: None,
        },
        chat: api::config::ChatConfig::default(),
        websearch: api::config::WebSearchConfig::default(),
        secrets: api::config::SecretsConfig::default(),
        workflow: api::workflow_runtime::WorkflowConfig::default(),
        gateway: api::config::GatewayConfig::default(),
        tabular: api::config::TabularConfig::default(),
    }
}

fn principal(id: &str) -> Principal {
    Principal {
        kind: authz::PrincipalKind::User,
        id: id.into(),
        email: None,
        groups: vec!["/acme".into()],
        roles: vec![],
        tenant_id: None,
    }
}

/// テスト全体の足場（サーバ・authz モック・セッション・播種済みノード）。
struct Harness {
    addr: SocketAddr,
    authz: Arc<RoleAuthz>,
    pool: PgPool,
    node_id: Uuid,
}

/// subject 文字列（`user:<tenant>|<id>` 形式）を AuthContext と同じ経路で作る。
fn subject_of(id: &str) -> Subject {
    let ctx = authz::AuthContext::new(principal(id), "acme".into(), "default".into());
    ctx.subject()
}

async fn seed_session(sessions: &MemorySessionStore, sid: &str, user_id: &str) {
    let record = SessionRecord {
        principal: principal(user_id),
        tenant_id: "default".into(),
        access_token: "access".into(),
        refresh_token: None,
        id_token: None,
        access_expires_at: chrono::Utc::now().timestamp() + 3600,
        csrf_token: "csrf".into(),
        keycloak_sid: None,
    };
    sessions
        .put("default", sid, &record, Duration::from_secs(3600))
        .await
        .unwrap();
}

async fn seed_file_node(pool: &PgPool) -> Uuid {
    let sha = format!("{:0>64}", hex::encode(Uuid::new_v4().as_bytes()));
    sqlx::query(
        "INSERT INTO blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount)
         VALUES ('default', 'acme', $1, 1, 'text/markdown', $2, 1)",
    )
    .bind(&sha)
    .bind(format!("default/acme/{sha}"))
    .execute(pool)
    .await
    .unwrap();
    let node_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO node (id, org, tenant_id, kind, name, blob_sha256, size_bytes, content_type, created_by, updated_by)
         VALUES ($1, 'acme', 'default', 'file', $1::text || '-doc.bin', $2, 1, 'application/octet-stream', 'tester', 'tester')",
    )
    .bind(node_id)
    .bind(&sha)
    .execute(pool)
    .await
    .unwrap();
    node_id
}

async fn setup() -> Option<Harness> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();

    let authz_mock = Arc::new(RoleAuthz::new());
    let authz_dyn: Arc<dyn AuthzClient> = Arc::clone(&authz_mock) as Arc<dyn AuthzClient>;
    let sessions = Arc::new(MemorySessionStore::default());
    seed_session(&sessions, "sid-alice", "alice").await;
    seed_session(&sessions, "sid-bob", "bob").await;
    seed_session(&sessions, "sid-charlie", "charlie").await;

    let config = base_config(&db_url);
    let storage_svc = Arc::new(storage::StorageService::new(
        pool.clone(),
        Arc::new(FakeStore),
        Arc::clone(&authz_dyn),
        Duration::from_secs(120),
        Duration::from_secs(900),
        1024,
    ));
    let directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    let tenants = Arc::new(storage::TenantStore::new(pool.clone()));
    let rag_admin = Arc::new(rag::RagAdmin::new(pool.clone(), None, None));
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz_dyn),
    ));
    let workflows = Arc::new(workflow_engine::WorkflowStore::new(Arc::clone(&artifacts)));
    let ui_validator = Arc::new(gui::SpecValidator::new(
        Arc::clone(&artifacts),
        pool.clone(),
    ));
    let ui_specs = Arc::new(gui::UiSpecStore::new(Arc::clone(&artifacts), ui_validator));
    let ui_actions = Arc::new(gui::ActionDispatcher::new(
        storage::audit::AuditRecorder::new(pool.clone()),
    ));
    let skills = Arc::new(gui::SkillStore::new(Arc::clone(&artifacts)));
    let mini_apps = Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), pool.clone()));
    let data_store = Arc::new(data::DataStore::new(
        pool.clone(),
        Arc::clone(&authz_dyn),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::clone(&directory),
            storage: Arc::clone(&storage_svc),
        }),
    ));
    let data_views = Arc::new(data::DataViewStore::new(
        Arc::clone(&artifacts),
        (*data_store).clone(),
    ));
    let fsms = Arc::new(data::FsmStore::new(
        Arc::clone(&artifacts),
        (*data_store).clone(),
    ));
    let mini_app_code = Arc::new(app_platform::MiniAppCodeStore::new(
        Arc::clone(&artifacts),
        app_platform::Registry::new(pool.clone()),
    ));
    let workflow_registration = Arc::new(workflow_engine::RegistrationService::new(
        pool.clone(),
        workflow_engine::DelegationStore::new(pool.clone(), Arc::clone(&authz_dyn)),
    ));
    let audit_rec = Arc::new(storage::audit::AuditRecorder::new(pool.clone()));
    let workflow_summaries = Arc::new(workflow_engine::WorkflowSummaryStore::new(pool.clone()));
    let workflow_layout = Arc::new(workflow_engine::EditorLayoutStore::new(pool.clone()));
    // 剥奪切断を短時間で検証するため再チェック間隔を 200ms に縮める。
    let collab_hub = Arc::new(
        collab::CollabHub::new(
            pool.clone(),
            Arc::clone(&authz_dyn),
            Arc::clone(&storage_svc),
        )
        .with_recheck_interval(Duration::from_millis(200)),
    );
    let jwks = Arc::new(api::middleware::JwksCache::new(
        reqwest::Client::new(),
        config.auth.effective_jwks_uri(),
        Duration::from_secs(300),
    ));
    let installs = Arc::new(app_platform::InstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        Arc::clone(&mini_app_code),
        Arc::clone(&data_store),
        Arc::clone(&authz_dyn),
        None,
        vec![],
    ));
    let tabular_svc = std::sync::Arc::new(tabular::TabularService::new(
        std::sync::Arc::clone(&storage_svc),
        tabular::RunnerConfig::new("shiki-tabular-runner", std::time::Duration::from_secs(5)),
        tabular::Quotas::default(),
    ));
    let state = AppState {
        config: Arc::new(config),
        db: api::state::ReadinessProbe::new(pool.clone()),
        authz: authz_dyn,
        jwks,
        sessions,
        http: reqwest::Client::new(),
        storage: storage_svc,
        collab: collab_hub,
        tabular: tabular_svc,
        artifacts,
        data: data_store,
        data_views,
        fsms,
        mini_app_code,
        installs,
        ui_specs,
        ui_actions,
        skills,
        mini_apps,
        secrets: None,
        workflows,
        workflow_launcher: None,
        workflow_registration,
        workflow_summaries,
        workflow_layout,
        audit: audit_rec,
        workflow_runs: None,
        directory,
        tenants,
        search: None,
        chat: None,
        rag_admin,
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });

    let node_id = seed_file_node(&pool).await;
    Some(Harness {
        addr,
        authz: authz_mock,
        pool,
        node_id,
    })
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// セッション Cookie 付きで WS 接続する。
async fn connect_ws(
    harness: &Harness,
    sid: &str,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let url = format!("ws://{}/collab/docs/{}/ws", harness.addr, harness.node_id);
    let mut request = url.into_client_request()?;
    request.headers_mut().insert(
        "Cookie",
        format!("shiki_session={sid}.default").parse().unwrap(),
    );
    let (ws, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(ws)
}

/// サーバの初期ハンドシェイク（SyncStep1）を 1 通受け取り、サーバ側セッションの
/// `doc.subscribe()` 完了を待つ。サーバは購読の**後**に SyncStep1 を送るため、これを
/// 受け取れた時点で以降の broadcast を取りこぼさない。connect 直後に編集を送ると相手
/// セッションの購読が間に合わず最初の update を落とす競合を防ぐ。
async fn await_server_ready(ws: &mut Ws) {
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("サーバ初期ハンドシェイク待ちタイムアウト")
        .expect("接続が閉じた")
        .expect("ws 受信エラー");
    assert!(
        matches!(msg, TgMessage::Binary(_)),
        "初期メッセージは binary（SyncStep1）"
    );
}

/// サーバへ yrs sync メッセージを送る。
async fn send_msg(ws: &mut Ws, msg: yrs::sync::Message) {
    ws.send(TgMessage::Binary(msg.encode_v1().into()))
        .await
        .unwrap();
}

/// 受信メッセージを doc に適用し続け、`pred` が真になったら true を返す（タイムアウトで false）。
async fn pump_until(ws: &mut Ws, doc: &Doc, pred: impl Fn(&Doc) -> bool) -> bool {
    if pred(doc) {
        return true;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let Ok(Some(Ok(msg))) = tokio::time::timeout(remaining, ws.next()).await else {
            return false;
        };
        if let TgMessage::Binary(bytes) = msg {
            apply_server_message(doc, &bytes);
            if pred(doc) {
                return true;
            }
        }
    }
}

/// サーバからの 1 メッセージをクライアント doc に適用する（sync step1 には応答しない
/// 簡易クライアント。テストは明示的に SyncStep1 を送って diff を取りに行く）。
fn apply_server_message(doc: &Doc, bytes: &[u8]) {
    let Ok(msg) = yrs::sync::Message::decode_v1(bytes) else {
        return;
    };
    if let yrs::sync::Message::Sync(
        yrs::sync::SyncMessage::SyncStep2(update) | yrs::sync::SyncMessage::Update(update),
    ) = msg
    {
        if let Ok(u) = Update::decode_v1(&update) {
            let _ = doc.transact_mut().apply_update(u);
        }
    }
}

/// クライアントの現在状態との差分をサーバへ要求する（sync step1）。
async fn request_sync(ws: &mut Ws, doc: &Doc) {
    let sv = doc.transact().state_vector();
    send_msg(
        ws,
        yrs::sync::Message::Sync(yrs::sync::SyncMessage::SyncStep1(sv)),
    )
    .await;
}

/// ローカル編集を行いサーバへ update を送る。
async fn edit_and_send(ws: &mut Ws, doc: &Doc, index: u32, chunk: &str) {
    let text = doc.get_or_insert_text("t");
    let before = doc.transact().state_vector();
    text.insert(&mut doc.transact_mut(), index, chunk);
    let update = doc.transact().encode_state_as_update_v1(&before);
    send_msg(
        ws,
        yrs::sync::Message::Sync(yrs::sync::SyncMessage::Update(update)),
    )
    .await;
}

fn text_of(doc: &Doc) -> String {
    let text = doc.get_or_insert_text("t");
    text.get_string(&doc.transact())
}

/// 受け入れ条件: 2 クライアントの並行編集が収束する（再接続の全量同期含む）。
#[tokio::test]
async fn two_clients_converge_and_reconnect_syncs() {
    let Some(h) = setup().await else { return };
    let alice = subject_of("alice");
    h.authz.grant(&alice, Relation::Editor);
    h.authz.grant(&alice, Relation::Viewer);

    let mut ws1 = connect_ws(&h, "sid-alice").await.unwrap();
    let mut ws2 = connect_ws(&h, "sid-alice").await.unwrap();
    // 双方のサーバセッションが購読を確立するまで待つ（初回 update の取りこぼし防止）。
    await_server_ready(&mut ws1).await;
    await_server_ready(&mut ws2).await;
    let doc1 = Doc::new();
    let doc2 = Doc::new();

    edit_and_send(&mut ws1, &doc1, 0, "hello ").await;
    edit_and_send(&mut ws2, &doc2, 0, "world").await;

    // 互いの編集がブロードキャストで届き、両者が同一文書に収束する。
    let ok1 = pump_until(&mut ws1, &doc1, |d| {
        let t = text_of(d);
        t.contains("hello") && t.contains("world")
    })
    .await;
    let ok2 = pump_until(&mut ws2, &doc2, |d| {
        let t = text_of(d);
        t.contains("hello") && t.contains("world")
    })
    .await;
    assert!(ok1 && ok2, "両クライアントに全編集が届くこと");
    assert_eq!(text_of(&doc1), text_of(&doc2), "収束すること");

    // 再接続（オフライン復帰相当）: まっさらなクライアントが step1 で全量を取得する。
    let expected = text_of(&doc1);
    let mut ws3 = connect_ws(&h, "sid-alice").await.unwrap();
    let doc3 = Doc::new();
    request_sync(&mut ws3, &doc3).await;
    let ok3 = pump_until(&mut ws3, &doc3, |d| text_of(d) == expected).await;
    assert!(ok3, "再接続クライアントが全量同期で収束すること");
}

/// 受け入れ条件: viewer は読めるが書けない。
#[tokio::test]
async fn viewer_reads_but_cannot_write() {
    let Some(h) = setup().await else { return };
    let alice = subject_of("alice");
    let bob = subject_of("bob");
    h.authz.grant(&alice, Relation::Editor);
    h.authz.grant(&alice, Relation::Viewer);
    h.authz.grant(&bob, Relation::Viewer);

    // alice が編集し、bob（viewer）は読み取れる。
    let mut ws_alice = connect_ws(&h, "sid-alice").await.unwrap();
    let doc_alice = Doc::new();
    edit_and_send(&mut ws_alice, &doc_alice, 0, "readable").await;

    let mut ws_bob = connect_ws(&h, "sid-bob").await.unwrap();
    let doc_bob = Doc::new();
    request_sync(&mut ws_bob, &doc_bob).await;
    let ok = pump_until(&mut ws_bob, &doc_bob, |d| text_of(d).contains("readable")).await;
    assert!(ok, "viewer が読めること");

    // bob が書き込みを試みる → サーバは適用しない。
    edit_and_send(&mut ws_bob, &doc_bob, 0, "EVIL").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // まっさらな alice セッションで全量を取り直し、EVIL が入っていないことを確認。
    let mut ws_check = connect_ws(&h, "sid-alice").await.unwrap();
    let doc_check = Doc::new();
    request_sync(&mut ws_check, &doc_check).await;
    let ok = pump_until(&mut ws_check, &doc_check, |d| {
        text_of(d).contains("readable")
    })
    .await;
    assert!(ok, "全量同期が返ること");
    assert!(
        !text_of(&doc_check).contains("EVIL"),
        "viewer の書込がサーバ状態へ反映されていないこと"
    );
}

/// 受け入れ条件: 共有解除で接続が切断される（定期再チェック・fail-closed）。
#[tokio::test]
async fn revocation_disconnects_session() {
    let Some(h) = setup().await else { return };
    let alice = subject_of("alice");
    h.authz.grant(&alice, Relation::Editor);
    h.authz.grant(&alice, Relation::Viewer);

    let mut ws = connect_ws(&h, "sid-alice").await.unwrap();
    let doc = Doc::new();
    edit_and_send(&mut ws, &doc, 0, "before revoke").await;

    // 剥奪 → 再チェック（200ms 間隔）で Close が届き、ストリームが終端すること。
    h.authz.revoke_all(&alice);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut closed = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TgMessage::Close(_)))) | Ok(Some(Err(_))) | Ok(None) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(_))) => {}
            Err(_) => break,
        }
    }
    assert!(closed, "剥奪後にサーバから切断されること");
}

/// relation が無い主体は WS ハンドシェイク前に HTTP で拒否される。
///
/// StorageService の読取認可は存在秘匿のため 404 を返す（`require_read` の設計・
/// 権限が無いことと存在しないことを区別させない）。403 ではなく 404 が正。
#[tokio::test]
async fn no_relation_is_rejected_before_upgrade() {
    let Some(h) = setup().await else { return };
    let err = connect_ws(&h, "sid-charlie").await;
    match err {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), 404, "存在秘匿の 404 で拒否されること");
        }
        other => panic!("HTTP 404 拒否を期待したが {other:?}"),
    }
    // 存在しないノードは 404（editor 持ちでも実在チェックが先）。
    let alice = subject_of("alice");
    h.authz.grant(&alice, Relation::Editor);
    h.authz.grant(&alice, Relation::Viewer);
    let url = format!("ws://{}/collab/docs/{}/ws", h.addr, Uuid::new_v4());
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Cookie", "shiki_session=sid-alice.default".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), 404, "実在しないノードは 404");
        }
        other => panic!("HTTP 404 拒否を期待したが {other:?}"),
    }
    let _ = &h.pool;
}
