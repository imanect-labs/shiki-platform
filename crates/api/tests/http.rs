//! ルーティング・セッション認証のレベルの統合テスト（外部依存なし）。
//!
//! BFF + セッション Cookie 方式（#55）の不変条件を検証する:
//! - 公開ルート（/healthz）は認証不要で 200。
//! - /me はセッション Cookie 無しで 401、有効セッションで 200。
//! - セッション失効（ストア削除）で次リクエストが 401。
//! - access token 期限切れでも refresh で継続（401 にならない）。
//! - 状態変更（logout）は double-submit CSRF が無いと 403。レスポンスにトークンを出さない。
//! - セッションキーが tenant_id でスコープされ、他テナントからは引けない。

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

use std::{sync::Arc, time::Duration};

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
use axum::{
    body::Body,
    http::{header::COOKIE, Request, StatusCode},
};
use http_body_util::BodyExt;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

/// 常に allow を返す認可モック（/me の認可を通す）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
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

/// 組織管理者（`organization#admin`・#553）と監査閲覧者（`organization#audit_viewer`・#634）だけを
/// 切り替え、他は allow する認可モック。
struct AdminGate {
    admin: bool,
    auditor: bool,
}

#[async_trait]
impl AuthzClient for AdminGate {
    async fn check(
        &self,
        _subject: &Subject,
        relation: Relation,
        _object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(match relation {
            Relation::Admin => self.admin,
            Relation::AuditViewer => self.auditor,
            _ => true,
        })
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

/// `/me` のメンバー判定で問われた organization オブジェクトを記録する認可モック（#811）。
/// AuthContext の org がどう解決されたかを HTTP 越しに観測する（member だけ allow）。
#[derive(Default)]
struct OrgProbe {
    asked: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl AuthzClient for OrgProbe {
    async fn check(
        &self,
        _subject: &Subject,
        relation: Relation,
        object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        if relation == Relation::Member {
            self.asked.lock().unwrap().push(object.to_string());
        }
        Ok(relation == Relation::Member)
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

/// ストレージのバイト層スタブ（/me・認証フローのテストでは呼ばれない）。
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

fn base_config() -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 0,
            cors_allowed_origins: vec![],
        },
        database: DatabaseConfig {
            url: "postgres://localhost/none".into(),
            max_connections: 1,
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
            backend: ObjectStoreBackend::S3,
            s3: None,
            max_upload_size_bytes: 5 * 1024 * 1024 * 1024,
            upload_gc: api::config::UploadGcConfig::default(),
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
            context_window_tokens: None,
            langfuse: None,
            user_agent: None,
            session_header: None,
            ..Default::default()
        },
        chat: api::config::ChatConfig::default(),
        websearch: api::config::WebSearchConfig::default(),
        secrets: api::config::SecretsConfig::default(),
        data: api::config::DataConfig::default(),
        workflow: api::workflow_runtime::WorkflowConfig::default(),
        gateway: api::config::GatewayConfig::default(),
        tabular: api::config::TabularConfig::default(),
        office: api::config::OfficeConfig::default(),
        insights: api::insights::InsightsConfig::default(),
        membership_reconcile: api::membership_reconcile::MembershipReconcileConfig::default(),
        egress: Default::default(),
    }
}

/// 認可チョークポイント（`AppState::authz`）だけ差し替えた AppState。
fn state_with_authz(sessions: Arc<dyn SessionStore>, authz: Arc<dyn AuthzClient>) -> AppState {
    AppState {
        authz,
        ..state_with(sessions, None)
    }
}

/// 指定のセッションストアと（任意の）OIDC 内部 base で AppState を組み立てる。
fn state_with(sessions: Arc<dyn SessionStore>, internal_base_url: Option<String>) -> AppState {
    let mut config = base_config();
    config.auth.internal_base_url = internal_base_url;
    let db = PgPoolOptions::new()
        // 認証系テストは DB 不要（到達不能 URL）。lazy pool の取得待ちで 30s 掛からないよう短く。
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy(&config.database.url)
        .unwrap();
    let jwks = Arc::new(api::middleware::JwksCache::new(
        reqwest::Client::new(),
        config.auth.effective_jwks_uri(),
        Duration::from_secs(300),
    ));
    let storage = Arc::new(storage::StorageService::new(
        db.clone(),
        Arc::new(FakeStore),
        Arc::new(AllowAll),
        Duration::from_secs(120),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    let directory = Arc::new(storage::DirectoryStore::new(db.clone()));
    let tenants = Arc::new(storage::TenantStore::new(db.clone()));
    let rag_admin = Arc::new(rag::RagAdmin::new(db.clone(), None, None));
    let artifacts = Arc::new(artifact::ArtifactStore::new(db.clone(), Arc::new(AllowAll)));
    let workflows = Arc::new(workflow_engine::WorkflowStore::new(Arc::clone(&artifacts)));
    let ui_actions = Arc::new(gui::ActionDispatcher::new(
        storage::audit::AuditRecorder::new(db.clone()),
    ));
    let skills = Arc::new(gui::SkillStore::new(Arc::clone(&artifacts)));
    let mini_apps = Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), db.clone()));
    let data_store = Arc::new(data::DataStore::new(
        db.clone(),
        Arc::new(AllowAll),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::clone(&directory),
            storage: Arc::clone(&storage),
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
        app_platform::Registry::new(db.clone()),
    ));
    let installs = Arc::new(app_platform::InstallService::new(
        db.clone(),
        app_platform::Registry::new(db.clone()),
        Arc::clone(&mini_app_code),
        Arc::clone(&data_store),
        Arc::new(AllowAll),
        None,
        vec![],
    ));
    let skill_installs = Arc::new(app_platform::SkillInstallService::new(
        db.clone(),
        app_platform::Registry::new(db.clone()),
        app_platform::TrustedKeyStore::new(db.clone()),
        Arc::clone(&artifacts),
        Arc::new(AllowAll),
    ));
    let bundles = Arc::new(app_platform::BundleStore::new(
        Arc::new(FakeStore),
        Arc::new(AllowAll),
        storage::audit::AuditRecorder::new(db.clone()),
    ));
    let app_usage = Arc::new(app_platform::AppUsageStore::new(
        db.clone(),
        Arc::new(AllowAll),
    ));
    let workflow_registration = Arc::new(workflow_engine::RegistrationService::new(
        db.clone(),
        workflow_engine::DelegationStore::new(db.clone(), Arc::new(AllowAll)),
    ));
    let audit_rec = Arc::new(storage::audit::AuditRecorder::new(db.clone()));
    let workflow_summaries = Arc::new(workflow_engine::WorkflowSummaryStore::new(db.clone()));
    let workflow_layout = Arc::new(workflow_engine::EditorLayoutStore::new(db.clone()));
    let workflow_approvals = Arc::new(workflow_engine::ApprovalStore::new(db.clone()));
    let collab_hub = Arc::new(collab::CollabHub::new(
        db.clone(),
        Arc::new(AllowAll),
        Arc::clone(&storage),
    ));
    let tabular_svc = std::sync::Arc::new(tabular::TabularService::new(
        std::sync::Arc::clone(&storage),
        tabular::RunnerConfig::new("shiki-tabular-runner", std::time::Duration::from_secs(5)),
        tabular::Quotas::default(),
    ));
    AppState {
        config: Arc::new(config),
        insights: std::sync::Arc::new(api::insights::InsightsService::new(db.clone())),
        db: api::state::ReadinessProbe::new(db),
        authz: Arc::new(AllowAll),
        jwks,
        sessions,
        http: reqwest::Client::new(),
        storage,
        collab: collab_hub,
        tabular: tabular_svc,
        artifacts,
        data: data_store,
        data_views,
        fsms,
        mini_app_code,
        installs,
        skill_installs,
        bundles,
        app_usage,
        ui_actions,
        skills,
        mini_apps,
        secrets: None,
        workflows,
        workflow_launcher: None,
        workflow_registration,
        workflow_summaries,
        workflow_layout,
        workflow_approvals,
        audit: audit_rec,
        workflow_runs: None,
        directory,
        tenants,
        search: None,
        chat: None,
        llm: None,
        rag_admin,
        office: None,
        docx_composer: std::sync::Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
    }
}

fn test_principal() -> Principal {
    Principal {
        kind: authz::PrincipalKind::User,
        id: "00000000-0000-0000-0000-000000000001".into(),
        email: Some("alice@acme.example".into()),
        groups: vec!["/acme".into()],
        roles: vec!["engineering".into()],
        tenant_id: None,
    }
}

fn session_record(
    access_expires_at: i64,
    refresh_token: Option<&str>,
    csrf: &str,
) -> SessionRecord {
    SessionRecord {
        principal: test_principal(),
        tenant_id: "default".into(),
        access_token: "access".into(),
        refresh_token: refresh_token.map(str::to_string),
        id_token: None,
        access_expires_at,
        csrf_token: csrf.into(),
        keycloak_sid: None,
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 署名なしの JWT 形状トークン（refresh の backchannel 応答を模す。claims を載せる）。
fn fake_jwt(claims: serde_json::Value) -> String {
    use base64::Engine;
    let enc = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = enc(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = enc(serde_json::to_vec(&claims).unwrap().as_slice());
    format!("{header}.{payload}.sig")
}

/// 任意の status/body を返すモック OIDC token エンドポイントを立て、内部 base URL を返す。
async fn spawn_token_server(status: StatusCode, body: serde_json::Value) -> String {
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/realms/shiki/protocol/openid-connect/token",
        post(move || {
            let body = body.clone();
            async move { (status, Json(body)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/realms/shiki")
}

#[tokio::test]
async fn healthz_is_public_and_ok() {
    let app = build_router(state_with(Arc::new(MemorySessionStore::new()), None));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn me_without_session_is_unauthorized() {
    let app = build_router(state_with(Arc::new(MemorySessionStore::new()), None));
    let resp = app
        .oneshot(Request::builder().uri("/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_with_valid_session_is_ok() {
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sid-valid",
            &session_record(now() + 3600, None, "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(store, None));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header(COOKIE, "shiki_session=sid-valid.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 有効セッションを 1 つ入れたストア。
async fn store_with_valid_session() -> Arc<MemorySessionStore> {
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sid-valid",
            &session_record(now() + 3600, None, "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    store
}

async fn get_with_session(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(COOKIE, "shiki_session=sid-valid.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn org_admin_routes_reject_non_admin_members() {
    // 組織管理者でなければ /org-admin/* は 403（管理 API に入れない・#553）。
    let store = store_with_valid_session().await;
    let app = build_router(state_with_authz(
        store,
        Arc::new(AdminGate {
            admin: false,
            auditor: false,
        }),
    ));
    for uri in [
        "/org-admin/ai/providers",
        "/org-admin/ai/models",
        "/org-admin/ai/settings",
        "/org-admin/ai/usage",
        "/org-admin/insights/overview",
        "/org-admin/insights/adoption",
        "/org-admin/insights/people",
        "/org-admin/insights/use-cases",
        "/org-admin/insights/impact",
        "/org-admin/insights/departments",
        "/org-admin/insights/settings",
    ] {
        let (status, _) = get_with_session(app.clone(), uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let (status, me) = get_with_session(app, "/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["is_admin"], false);
}

#[tokio::test]
async fn org_admin_routes_pass_the_gate_for_admins() {
    // 管理者はゲートを通る（この構成は LLM 未構成なので 503＝ハンドラまで届いた）。
    let store = store_with_valid_session().await;
    let app = build_router(state_with_authz(
        store,
        Arc::new(AdminGate {
            admin: true,
            auditor: false,
        }),
    ));
    let (status, _) = get_with_session(app.clone(), "/org-admin/ai/models").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (_, me) = get_with_session(app, "/me").await;
    assert_eq!(me["is_admin"], true);
}

#[tokio::test]
async fn org_audit_routes_reject_non_auditors_even_if_admin() {
    // 監査閲覧者でなければ /org-audit/* は 403。admin は audit_viewer を含意しない（#634）。
    let store = store_with_valid_session().await;
    let app = build_router(state_with_authz(
        store,
        Arc::new(AdminGate {
            admin: true,
            auditor: false,
        }),
    ));
    for uri in ["/org-audit/events", "/org-audit/export"] {
        let (status, _) = get_with_session(app.clone(), uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let (_, me) = get_with_session(app, "/me").await;
    assert_eq!(me["can_view_audit"], false);
}

#[tokio::test]
async fn org_audit_routes_pass_the_gate_for_auditors_who_are_not_admins() {
    // 管理者でない監査閲覧者はゲートを通る（不正な条件の 400＝ハンドラまで届いた）。
    // 逆に管理 API（/org-admin/*）には入れない。
    let store = store_with_valid_session().await;
    let app = build_router(state_with_authz(
        store,
        Arc::new(AdminGate {
            admin: false,
            auditor: true,
        }),
    ));
    let (status, _) = get_with_session(app.clone(), "/org-audit/events?decision=maybe").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = get_with_session(app.clone(), "/org-audit/export?format=xlsx").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = get_with_session(app.clone(), "/org-admin/ai/models").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, me) = get_with_session(app, "/me").await;
    assert_eq!(me["can_view_audit"], true);
    assert_eq!(me["is_admin"], false);
}

#[tokio::test]
async fn org_admin_routes_require_a_session() {
    let app = build_router(state_with_authz(
        Arc::new(MemorySessionStore::new()),
        Arc::new(AdminGate {
            admin: true,
            auditor: false,
        }),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/org-admin/ai/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_session_is_immediately_unauthorized() {
    // ストアに存在しない session id を指す Cookie は即 401（セッション削除＝即時失効）。
    let app = build_router(state_with(Arc::new(MemorySessionStore::new()), None));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header(COOKIE, "shiki_session=already-deleted.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_access_token_is_refreshed_and_continues() {
    // access token 期限切れでも refresh が成功すれば downstream は 401 にならず、
    // principal は新トークンのクレームへ追従する。
    let new_access = fake_jwt(serde_json::json!({
        "sub": "00000000-0000-0000-0000-000000000001",
        "groups": ["/neworg"],
    }));
    let token_base = spawn_token_server(
        StatusCode::OK,
        serde_json::json!({ "access_token": new_access, "expires_in": 3600 }),
    )
    .await;
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sid-exp",
            &session_record(now() - 10, Some("refresh-token"), "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(store.clone(), Some(token_base)));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header(COOKIE, "shiki_session=sid-exp.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // セッションがローテーション更新され、principal が新クレームへ追従している。
    let updated = store.get("default", "sid-exp").await.unwrap().unwrap();
    assert!(updated.access_expires_at > now());
    assert_eq!(updated.principal.groups, vec!["/neworg".to_string()]);
}

/// 組織の member の直接タプルだけを持つ認可（セッションの更新での同期を見る・D-130）。ほかの check は通す。
#[derive(Default)]
struct OrgMembers(std::sync::Mutex<Vec<String>>);

#[async_trait]
impl AuthzClient for OrgMembers {
    async fn check(
        &self,
        _subject: &Subject,
        relation: Relation,
        object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        if relation == Relation::Member && object.as_str().starts_with("organization:") {
            return Ok(self.0.lock().unwrap().contains(&object.to_string()));
        }
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        if relation == Relation::Member && object.as_str().starts_with("organization:") {
            self.0.lock().unwrap().push(object.to_string());
        }
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        let mut rows = self.0.lock().unwrap();
        let before = rows.len();
        if relation == Relation::Member {
            rows.retain(|o| o != object.as_str());
        }
        Ok(rows.len() != before)
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
        object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(if object_type == ObjectType::Organization {
            self.0.lock().unwrap().clone()
        } else {
            vec![]
        })
    }
}

/// セッションの更新でも、所属グループから決まる組織の member にそろえる（D-130）。組織を移った人の前の組織の
/// member は外れる（組織を離れた人が、アプリのゲートウェイでその組織に入り続けない）。
#[tokio::test]
async fn refresh_moves_the_org_member_with_the_groups() {
    let new_access = fake_jwt(serde_json::json!({
        "sub": "00000000-0000-0000-0000-000000000001",
        "groups": ["/neworg"],
    }));
    let token_base = spawn_token_server(
        StatusCode::OK,
        serde_json::json!({ "access_token": new_access, "expires_in": 3600 }),
    )
    .await;
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sid-org",
            &session_record(now() - 10, Some("refresh-token"), "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    // 前のトークンの groups（/acme）で付けた member。
    let members = Arc::new(OrgMembers::default());
    members
        .0
        .lock()
        .unwrap()
        .push("organization:default|acme".into());
    let app = build_router(AppState {
        authz: members.clone(),
        ..state_with(store.clone(), Some(token_base))
    });
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header(COOKIE, "shiki_session=sid-org.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 同期は裏で走る（リクエストの待ち時間に載せない）。
    let mut rows = Vec::new();
    for _ in 0..50 {
        rows = members.0.lock().unwrap().clone();
        if rows == ["organization:default|neworg"] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(rows, ["organization:default|neworg"]);
}

#[tokio::test]
async fn transient_refresh_failure_keeps_session_when_access_still_valid() {
    // token endpoint が 5xx（一過性）でも、access がまだ有効ならセッションを破棄せず継続。
    let token_base = spawn_token_server(
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({ "error": "temporarily_unavailable" }),
    )
    .await;
    let store = Arc::new(MemorySessionStore::new());
    // leeway(60) 内だが未失効（残り 30 秒）。
    store
        .put(
            "default",
            "sid-tr",
            &session_record(now() + 30, Some("refresh-token"), "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(store.clone(), Some(token_base)));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header(COOKIE, "shiki_session=sid-tr.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 一過性障害ではセッションを破棄しない。
    assert!(store.get("default", "sid-tr").await.unwrap().is_some());
}

#[tokio::test]
async fn update_if_present_does_not_resurrect_deleted_session() {
    // logout 等で削除済みのセッションを refresh の書き戻しで復活させない（即時失効の保証）。
    let store = MemorySessionStore::new();
    let rec = session_record(now() + 3600, Some("rt"), "c");
    store
        .put("default", "sid", &rec, Duration::from_secs(3600))
        .await
        .unwrap();
    store.delete("default", "sid").await.unwrap();
    // 削除後の update_if_present は false（作成しない）。
    let updated = store
        .update_if_present("default", "sid", &rec, Duration::from_secs(3600))
        .await
        .unwrap();
    assert!(!updated);
    assert!(store.get("default", "sid").await.unwrap().is_none());
}

#[tokio::test]
async fn auth_session_reports_dead_session_as_unauthenticated() {
    use axum::http::header::COOKIE as COOKIE_H;
    let store = Arc::new(MemorySessionStore::new());
    // access 期限切れ＋refresh 無し＝死にセッション。
    store
        .put(
            "default",
            "sid-dead",
            &session_record(now() - 10, None, "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(store, None));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/session")
                .header(COOKIE_H, "shiki_session=sid-dead.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json.get("authenticated"),
        Some(&serde_json::Value::Bool(false)),
        "body={}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn expired_access_without_refresh_is_unauthorized() {
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sid-dead",
            &session_record(now() - 10, None, "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(store.clone(), None));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header(COOKIE, "shiki_session=sid-dead.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // 失効セッションは破棄されている。
    assert!(store.get("default", "sid-dead").await.unwrap().is_none());
}

#[tokio::test]
async fn logout_without_csrf_is_forbidden() {
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sid-lo",
            &session_record(now() + 3600, None, "csrf-tok"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(store, None));
    // CSRF ヘッダ無し（Cookie はある）→ 403。
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/logout")
                .header(COOKIE, "shiki_session=sid-lo.default; shiki_csrf=csrf-tok")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn logout_with_csrf_succeeds_and_leaks_no_token() {
    let store = Arc::new(MemorySessionStore::new());
    // id_token を保持していてもレスポンス（end-session URL）に出さないことを検証する。
    let mut record = session_record(now() + 3600, Some("refresh-token"), "csrf-tok");
    record.id_token = Some("idtokhdr.idtokpayload.idtoksig".into());
    store
        .put("default", "sid-lo2", &record, Duration::from_secs(3600))
        .await
        .unwrap();
    let app = build_router(state_with(store.clone(), None));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/logout")
                .header(COOKIE, "shiki_session=sid-lo2.default; shiki_csrf=csrf-tok")
                .header("x-csrf-token", "csrf-tok")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // 許可フィールド（end_session_url）のみ。トークン等の余計なフィールドを出さない。
    let obj = json.as_object().expect("logout 応答は JSON オブジェクト");
    assert_eq!(obj.len(), 1, "想定外のフィールド: {obj:?}");
    assert!(obj
        .get("end_session_url")
        .and_then(|v| v.as_str())
        .is_some());
    // セッションは削除されている。
    assert!(store.get("default", "sid-lo2").await.unwrap().is_none());
}

#[tokio::test]
async fn session_store_is_tenant_scoped() {
    // 別テナントのスコープからは同じ session id を引けない（共用プール論理分離）。
    let store = MemorySessionStore::new();
    store
        .put(
            "tenant-a",
            "sid",
            &session_record(now() + 3600, None, "c"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    assert!(store.get("tenant-b", "sid").await.unwrap().is_none());
    assert!(store.get("tenant-a", "sid").await.unwrap().is_some());
}

/// 版のブロック列（出典パネル・#508）: セッション必須、RAG 無効なら 503（DB に触れる前に止まる）。
#[tokio::test]
async fn version_blocks_requires_session_and_rag() {
    let path = format!(
        "/files/{}/versions/1/blocks?from=0&limit=10",
        uuid::Uuid::new_v4()
    );
    let app = build_router(state_with(Arc::new(MemorySessionStore::new()), None));
    let resp = app
        .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sid-blocks",
            &session_record(now() + 3600, None, "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(store, None));
    let resp = app
        .oneshot(
            Request::builder()
                .uri(&path)
                .header(COOKIE, "shiki_session=sid-blocks.default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn openapi_json_is_served() {
    let app = build_router(state_with(Arc::new(MemorySessionStore::new()), None));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api-docs/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// SQL 前段面（`POST /data/sql`）の部分集合外は 422 と構造化した理由を本文の最上位で返す
/// （拒否はパースの段で決まり、データベースへは届かない・Task 15.10）。
#[tokio::test]
async fn data_sql_out_of_subset_is_structured_422() {
    let app = build_router(state_with(store_with_valid_session().await, None));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/data/sql")
                .header(COOKIE, "shiki_session=sid-valid.default; shiki_csrf=csrf")
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"sql": "SELECT name FROM t\nUNION SELECT name FROM u"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], 422);
    assert_eq!(json["code"], "unsupported");
    assert!(json["message"].as_str().unwrap().contains("UNION"));
    assert_eq!(json["location"]["line"], 1);
}

/// multi テナンシーで、セッション middleware → OrgId → AuthContext の org が自テナント配下の
/// group だけから解決されることを HTTP 越しに確かめる（#811）。
#[tokio::test]
async fn multi_tenant_org_comes_only_from_own_tenant_groups() {
    let cases: &[(&[&str], &str)] = &[
        // 他テナント配下の group（名前が先でも）は拾わず、自テナントの org を採る。
        (
            &["/tenants/evil/aaa", "/tenants/t1/acme/eng"],
            "organization:t1|acme",
        ),
        // 他テナント配下だけ・テナント無修飾の旧 group だけなら既定 org。
        (&["/tenants/evil/aaa"], "organization:t1|default"),
        (&["/acme"], "organization:t1|default"),
        // tenant の比較はセグメント単位（`t10` は `t1` ではない）。
        (&["/tenants/t10/acme"], "organization:t1|default"),
    ];
    for (groups, expected) in cases {
        let store = Arc::new(MemorySessionStore::new());
        let mut record = session_record(now() + 3600, None, "csrf");
        record.tenant_id = "t1".into();
        record.principal.tenant_id = Some("t1".into());
        record.principal.groups = groups.iter().map(|g| g.to_string()).collect();
        store
            .put("t1", "sid-multi", &record, Duration::from_secs(3600))
            .await
            .unwrap();
        let probe = Arc::new(OrgProbe::default());
        let mut state = state_with_authz(store, probe.clone());
        let mut config = (*state.config).clone();
        config.auth.tenancy = Tenancy::Multi;
        state.config = Arc::new(config);
        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/me")
                    .header(COOKIE, "shiki_session=sid-multi.t1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{groups:?}");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let me: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(me["tenant_id"], "t1");
        assert_eq!(
            Some(me["org"].as_str().unwrap()),
            expected.strip_prefix("organization:t1|"),
            "{groups:?}"
        );
        let asked = probe.asked.lock().unwrap().clone();
        assert!(!asked.is_empty(), "{groups:?}");
        assert!(asked.iter().all(|o| o == expected), "{groups:?}: {asked:?}");
    }
}

// --- ChatGPT プランの接続（/me/chatgpt・#854） ---------------------------------

/// ChatGPT プランを有効にした gateway を載せる（資格情報は一時ディレクトリ・認証サーバへは出ない範囲だけ叩く）。
fn state_with_chatgpt(sessions: Arc<dyn SessionStore>, dir: &std::path::Path) -> AppState {
    let mut state = state_with(sessions, None);
    let mut config = (*state.config).clone();
    config.llm.chatgpt.enabled = true;
    config.llm.chatgpt.models = vec!["gpt-6.1-sol".into()];
    state.config = Arc::new(config);
    let account = llm_gateway::ChatGptAccount::open(
        reqwest::Client::new(),
        llm_gateway::ChatGptSettings::new(dir.join("chatgpt.json")),
    );
    let gateway_config = llm_gateway::GatewayConfig {
        extra_providers: vec![llm_gateway::ProviderConfig {
            id: llm_gateway::CHATGPT_PROVIDER_ID.into(),
            kind: llm_gateway::ProviderKind::ChatgptPlan,
            ..Default::default()
        }],
        catalog: llm_gateway::ModelCatalog {
            default_model: "m".into(),
            models: vec![llm_gateway::ModelEntry {
                id: "m".into(),
                ..Default::default()
            }],
        },
        chatgpt: Some(Arc::new(account)),
        ..Default::default()
    };
    let db = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy(&state.config.database.url)
        .unwrap();
    state.llm = Some(Arc::new(
        llm_gateway::LlmGateway::build(db, reqwest::Client::new(), gateway_config).unwrap(),
    ));
    state
}

/// セッション＋CSRF（二重送信）付きで状態変更系を叩く。
async fn send_with_session(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(COOKIE, "shiki_session=sid-valid.default; shiki_csrf=csrf")
        .header("x-csrf-token", "csrf");
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn chatgpt_routes_are_unavailable_unless_enabled() {
    let app = build_router(state_with(store_with_valid_session().await, None));
    let (status, body) = get_with_session(app.clone(), "/me/chatgpt").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "unavailable");
    for (method, uri) in [
        ("POST", "/me/chatgpt/login"),
        ("POST", "/me/chatgpt/callback"),
        ("DELETE", "/me/chatgpt"),
    ] {
        let body = (uri == "/me/chatgpt/callback")
            .then(|| serde_json::json!({ "callback_url": "http://127.0.0.1:1455/auth/callback" }));
        let (status, _) = send_with_session(app.clone(), method, uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
}

#[tokio::test]
async fn chatgpt_login_starts_and_rejects_wrong_callbacks_with_a_reason() {
    let dir = std::env::temp_dir().join(format!("shiki-chatgpt-http-{}", uuid::Uuid::new_v4()));
    let app = build_router(state_with_chatgpt(store_with_valid_session().await, &dir));

    let (status, body) = get_with_session(app.clone(), "/me/chatgpt").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "not_connected");

    // 認可 URL は OpenAI の認証サーバ（トークンは応答に含めない）。
    let (status, body) = send_with_session(app.clone(), "POST", "/me/chatgpt/login", None).await;
    assert_eq!(status, StatusCode::OK);
    let url = body["authorize_url"].as_str().unwrap();
    assert!(
        url.starts_with("https://auth.openai.com/api/accounts/authorize?"),
        "{url}"
    );

    // 貼り間違い・古い URL は交換せず、理由を detail で返す。
    for (pasted, reason) in [
        ("https://chatgpt.com/", "127.0.0.1"),
        (
            "http://127.0.0.1:1455/auth/callback?code=c&state=stale&client_id=oaiapp_x",
            "最新のログイン",
        ),
    ] {
        let (status, body) = send_with_session(
            app.clone(),
            "POST",
            "/me/chatgpt/callback",
            Some(serde_json::json!({ "callback_url": pasted })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{pasted}");
        assert!(
            body["detail"].as_str().unwrap().contains(reason),
            "{pasted}: {body}"
        );
    }

    // 未接続の切断は何もせず成功する（失効の送信は無い）。
    let (status, _) = send_with_session(app.clone(), "DELETE", "/me/chatgpt", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = get_with_session(app, "/me/chatgpt").await;
    assert_eq!(body["state"], "not_connected");
    let _ = std::fs::remove_dir_all(dir);
}

/// 検索の内訳（BUG-94）の試験用の認可: 組織の管理者かを切り替え、ファイルはどれも読めない（post-filter で
/// 落ちる）。ほかの問い（メンバーなど）は通す。
struct SearchGate {
    admin: bool,
}

#[async_trait]
impl AuthzClient for SearchGate {
    async fn check(
        &self,
        _subject: &Subject,
        relation: Relation,
        object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(match relation {
            Relation::Admin => self.admin,
            Relation::Viewer if object.as_str().starts_with("file:") => false,
            _ => true,
        })
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

/// 埋め込み・rerank は決まった値を返すだけ（検索の段の数を数える試験に中身は要らない）。
struct FixedEmbedder;

#[async_trait]
impl rag::embedding::EmbeddingProvider for FixedEmbedder {
    async fn embed(
        &self,
        _ctx: &authz::AuthContext,
        _input: rag::embedding::EmbedInput,
        texts: &[String],
    ) -> Result<rag::embedding::EmbedResponse, rag::RagError> {
        Ok(rag::embedding::EmbedResponse {
            vectors: texts.iter().map(|_| vec![1.0, 0.0]).collect(),
            model_version: "fixed".into(),
            dimension: 2,
        })
    }

    fn model_version(&self) -> &str {
        "fixed"
    }
}

struct ZeroReranker;

#[async_trait]
impl rag::rerank::Reranker for ZeroReranker {
    async fn rerank(
        &self,
        _ctx: &authz::AuthContext,
        _query: &str,
        passages: &[rag::rerank::RerankPassage],
    ) -> Result<Vec<rag::rerank::RerankScore>, rag::RagError> {
        Ok(passages
            .iter()
            .map(|p| rag::rerank::RerankScore {
                id: p.id.clone(),
                score: 0.0,
            })
            .collect())
    }
}

/// どの問いにも、同じ 1 件（読めないファイルの箇所）を返すベクトル索引。
struct OneHitVectors {
    hit: rag::vector_store::ScoredChunk,
}

#[async_trait]
impl rag::vector_store::VectorStore for OneHitVectors {
    async fn ensure_ready(&self, _dimension: usize) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn upsert(
        &self,
        _ctx: &authz::AuthContext,
        _points: &[rag::vector_store::ChunkPoint],
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn delete_node(
        &self,
        _ctx: &authz::AuthContext,
        _node_id: uuid::Uuid,
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn delete_stale_versions(
        &self,
        _ctx: &authz::AuthContext,
        _node_id: uuid::Uuid,
        _keep_version: i64,
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn set_authz_tags(
        &self,
        _ctx: &authz::AuthContext,
        _node_id: uuid::Uuid,
        _tags: &[String],
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn search(
        &self,
        _ctx: &authz::AuthContext,
        query: &rag::vector_store::VectorSearch<'_>,
    ) -> Result<Vec<rag::vector_store::ScoredChunk>, rag::RagError> {
        Ok(if query.exclude.contains(&self.hit.chunk_id) {
            vec![]
        } else {
            vec![self.hit.clone()]
        })
    }
    async fn purge_tenant(&self, _tenant_id: &str) -> Result<(), rag::RagError> {
        Ok(())
    }
}

/// 全文の索引は空。
struct NoFulltext;

impl rag::fulltext::FulltextIndex for NoFulltext {
    fn replace_node(
        &self,
        _ctx: &authz::AuthContext,
        _node_id: uuid::Uuid,
        _docs: &[rag::fulltext::FulltextDoc<'_>],
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    fn delete_node(
        &self,
        _ctx: &authz::AuthContext,
        _node_id: uuid::Uuid,
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    fn search(
        &self,
        _ctx: &authz::AuthContext,
        _query_text: &str,
        _limit: usize,
        _prefilter: &rag::vector_store::PreFilter,
        _scope_tags: &[String],
        _exclude: &[uuid::Uuid],
    ) -> Result<Vec<rag::vector_store::ScoredChunk>, rag::RagError> {
        Ok(vec![])
    }
    fn purge_tenant(&self, _tenant_id: &str) -> Result<(), rag::RagError> {
        Ok(())
    }
}

/// BUG-94・D-80: 文書検索の内訳（`debug`）は組織の管理者にだけ返す。管理者でない人が `debug: true` を
/// 付けても 200 で結果だけを返し、権限で除いた文書の数（`authz_denied_files`）を返さない（403 にしない）。
/// 検索の監査を書くので実 DB が要る（`STORAGE_TEST_DATABASE_URL` が無ければスキップ）。
#[tokio::test]
async fn search_debug_is_returned_only_to_org_admins() {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let hit = rag::vector_store::ScoredChunk {
        chunk_id: uuid::Uuid::new_v4(),
        node_id: uuid::Uuid::new_v4(),
        score: 0.9,
    };
    for admin in [false, true] {
        let authz: Arc<dyn AuthzClient> = Arc::new(SearchGate { admin });
        let search = rag::SearchService::new(
            pool.clone(),
            rag::RagConfig::default(),
            Arc::new(FixedEmbedder),
            Arc::new(ZeroReranker),
            Arc::new(OneHitVectors { hit: hit.clone() }),
            Arc::new(NoFulltext),
            Arc::clone(&authz),
            storage::audit::AuditRecorder::new(pool.clone()),
        );
        let state = AppState {
            search: Some(Arc::new(search)),
            ..state_with_authz(store_with_valid_session().await, authz)
        };
        let (status, body) = send_with_session(
            build_router(state),
            "POST",
            "/search",
            Some(serde_json::json!({ "query": "経費", "debug": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "admin={admin}: {body}");
        assert_eq!(
            body["results"],
            serde_json::json!([]),
            "admin={admin}: {body}"
        );
        if admin {
            assert_eq!(
                body["debug"]["authz_denied_files"], 1,
                "管理者には内訳を返す: {body}"
            );
        } else {
            assert!(
                body.get("debug").is_none(),
                "管理者でない人には内訳を返さない: {body}"
            );
            assert!(!body.to_string().contains("authz_denied_files"), "{body}");
        }
    }
    // 検索の監査（このテストの 2 回分）を片付ける。
    sqlx::query(
        "DELETE FROM audit_log WHERE tenant_id = 'default' AND action LIKE 'search%' \
         AND metadata::text LIKE $1",
    )
    .bind(format!("%{}%", hit.node_id))
    .execute(&pool)
    .await
    .ok();
}

/// 社内文書の検索が使えない環境では、取り込み直し（ドライブのメニュー）とまとめての取り込み（管理コンソール）を、
/// 画面にそのまま出せる文言で断る（D-126）。管理者の様子の取得は `enabled: false` を返す（画面が「この環境では
/// 使えません」と言う材料）。管理者でない人は、まとめての取り込みの API に入れない。
#[tokio::test]
async fn search_ingest_refuses_when_search_is_disabled() {
    let admin = build_router(state_with_authz(
        store_with_valid_session().await,
        Arc::new(AdminGate {
            admin: true,
            auditor: false,
        }),
    ));
    let disabled = "この環境では社内文書の検索が使えません。管理者に連絡してください。";
    let (status, body) = send_with_session(
        admin.clone(),
        "POST",
        &format!("/files/{}/reindex", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "search_disabled", "{body}");
    assert_eq!(body["message"], disabled, "{body}");
    let (status, body) = get_with_session(admin.clone(), "/org-admin/search/backfill").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false, "{body}");
    assert!(body["backfill"].is_null(), "{body}");
    let (status, body) = send_with_session(admin, "POST", "/org-admin/search/backfill", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "search_disabled", "{body}");
    assert_eq!(body["message"], disabled, "{body}");

    let member = build_router(state_with_authz(
        store_with_valid_session().await,
        Arc::new(AdminGate {
            admin: false,
            auditor: false,
        }),
    ));
    let (status, _) = get_with_session(member.clone(), "/org-admin/search/backfill").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send_with_session(member, "POST", "/org-admin/search/backfill", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
