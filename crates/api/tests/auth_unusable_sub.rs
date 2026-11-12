//! sub が利用者の id に使えない人（FGA の区切りの文字を含む・D-205・GAP-77）を、ログインとセッションの更新の入口で
//! 断ることの統合テスト（外部依存なし）。
//!
//! - ログイン（`/auth/callback`）: JSON の 401 を見せずに `/login?error=account_unusable` へ戻す。セッションの Cookie を
//!   出さず、OpenFGA（組織の member・ロール）にも触れない。受け取った refresh token で Keycloak の SSO セッションを
//!   サーバ側から終える（終えられなくてもログイン画面へは戻す）。
//! - セッションの更新: refresh で受け取ったトークンの sub が使えなければ、セッションを捨てて 401 にする（この変更より
//!   前にできたセッションの人を、古い principal のまま続けさせない）。
//!
//! モックの IdP（token・JWKS）と鍵・設定・AppState の組み立ては `auth_flow.rs` と同じもの（テスト専用の RSA 鍵）。

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

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use api::{
    build_router,
    config::*,
    session::{encode_session_cookie, MemorySessionStore, SessionRecord, SessionStore},
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
use jsonwebtoken::{EncodingKey, Header};
use tower::ServiceExt;

const KID: &str = "test-key-1";
const ISSUER: &str = "http://idp.test/realms/shiki";
const AUDIENCE: &str = "shiki-api";

/// テスト専用 RSA 秘密鍵（PKCS#8 PEM・2048bit）。公開鍵成分は [`JWK_N`]/`e=AQAB`。
const TEST_RSA_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDZHuilPKTVyZGZ
5AvC8vIL1M3dZdeaufFSwrqAcybhfnNTjGSo2Wn9T2O5Q+FScS42Gqj9MU6rHscy
VFfOxFMTCJ8WHANaOKCfjaHlQ8Dt5yT5Z5KtacEcYK2tjgfYTiA1kzdOxV7mAaWo
RoeuHbwC48F8fQH99ZkPGL5Yq06BWXuCJStzcWBFbo4GYJb6LFmOoYsemCZI15TE
uG3P4Vgs40fVcfbhB2eczxKuV1nzpRpp40/JYW0l4fE/t9VKKklXMPyxHqAOTqDB
NckcgttixNro0fXrt5BpFu75lyaNeL59p6TurOxJyBw8ooXADmpHolt0VrJ06jvP
65aIo9nrAgMBAAECggEABVysjSwevALi3CSUH8flL2KdhOq3dORDr/IMLhDp9Gat
jXIpqDvaUL2trk0dWu5doEtvQfV+Rl6Xt8f6dSpHDPDJEQA3GvrMCLS0O1e9g4KS
WLB0oGC8uqkukNsxmWdwhzIwCvt32QFQaIP5ZNTqKD4csKjbrDxs/8DyLnlokmwq
KfpHYkoZ9BtBWjAkpIhRA5FvqGTa6vXZou95d+4Vjx6V4TmvvfXuiJHcD977PG00
lkdbrw8PPnodbao5GzjUz//uku8hymYWjfckHZmK8SgnsmxM0ZzOWmp9qrz1DKEp
4W/9iuLKEQVILeZzJUHYt4tTqczt7s2jkbhLWGsCjQKBgQDt/cPya+AJ996gYsPs
bKliFtFb5Q/UzRv1JwTjIq/8U15dOXCxJL4Lim6K7LiMZbuq9T2zsuujqrOaLDCC
qQj/MKI4aftUmjgGYVW8CovghOVCgvKb6f6FeOGMrn2B0lGyying6dhVzrL7PLDq
+iWUuumN1vgzfhD9Cts5MdRcbwKBgQDpjNnZpD0SkqZlJXihRaEXR1d20vTafR0z
JP/LnvL2RYb5CGu6DA5xG7ngS7JmNDzylQBn54YcHqxWovdD2V6nk9C2YUhZrxr1
yfoi8FQOm0ScVwE9tTox5HMOviZo7bvN1xmlgutfOuOW93YQ0KqZEQZusSA3/NkH
MehCBWAQRQKBgHujbSvA7ThghFDwXnayEOE7l3JVMv9Lu22F4t0ZRTIiIZDu6WOu
Aek+9qTHzCxsIa30ECUOG6sAYKQEtwL6TAk/O9dw/7f5EogGAyNYm0h94hjGrMFh
M/AlV4/diqhqGjV3H4CQG+qgIo2w/vxkDigRXopolrMxmCPNgwxYncmTAoGBALMb
xeZXQk8AEIP5XK2xjH0hxT3nQshcswwKD/HEkGe1onFRt+wSWvD7Zm1RIBupbCRN
iOYmdH8UNu6qRB7QkPrLLYDw0l+VHoPoxeANlyksgk2zm8wLM/oXTPW9dg96YlDV
6WE5KfD6ZJfeZ7k1jd+dYuV5CVBmpLoT2B7pqGZRAoGATm1reSlO1CNa50Wpyki0
ZhyOnFyW40/6HAm9TPQTd5Ckee+tkRpTpOU3sdNNGkLMRSg1qUKy+TJgHXpvygGl
d8yx2gsYwCxeUAvpdbVQN+kwRY44bZiXhgMD8sK0UNW6p9197txgymvcwVwzCgBb
dQGoas84lLLCX8s898HkuXc=
-----END PRIVATE KEY-----";

/// [`TEST_RSA_PEM`] の公開鍵 modulus（base64url・パディング無し）。
const JWK_N: &str = "2R7opTyk1cmRmeQLwvLyC9TN3WXXmrnxUsK6gHMm4X5zU4xkqNlp_U9juUPhUnEuNhqo_TFOqx7HMlRXzsRTEwifFhwDWjign42h5UPA7eck-WeSrWnBHGCtrY4H2E4gNZM3TsVe5gGlqEaHrh28AuPBfH0B_fWZDxi-WKtOgVl7giUrc3FgRW6OBmCW-ixZjqGLHpgmSNeUxLhtz-FYLONH1XH24QdnnM8SrldZ86UaaeNPyWFtJeHxP7fVSipJVzD8sR6gDk6gwTXJHILbYsTa6NH167eQaRbu-ZcmjXi-faek7qzsScgcPKKFwA5qR6JbdFaydOo7z-uWiKPZ6w";

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

/// 呼ばれた数を数える認可（断った人で OpenFGA に触れないことを見る）。
#[derive(Default)]
struct Counting(AtomicUsize);

impl Counting {
    fn calls(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
    fn bump<T>(&self, value: T) -> Result<T, AuthzError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(value)
    }
}

#[async_trait]
impl AuthzClient for Counting {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        self.bump(true)
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.bump(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.bump(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        self.bump(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.bump(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        self.bump(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.bump(vec![])
    }
}

/// ストレージのバイト層スタブ（認証フローのテストでは呼ばれない）。
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

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// 指定 kid で RS256 署名した access token を作る。`kid` を変えると未知 kid 経路を試せる。
fn sign_token(kid: &str, claims: serde_json::Value) -> String {
    let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let key = EncodingKey::from_rsa_pem(TEST_RSA_PEM.as_bytes()).expect("テスト鍵の読み込み");
    jsonwebtoken::encode(&header, &claims, &key).expect("署名")
}

fn jwks_body(kid: &str) -> serde_json::Value {
    serde_json::json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": kid,
            "n": JWK_N,
            "e": "AQAB",
        }]
    })
}

/// モック IdP の logout エンドポイントが受けた form の本文。
type LogoutCalls = Arc<Mutex<Vec<String>>>;

/// token / JWKS / logout を返す in-process モック IdP を立て、その base URL と logout が受けた本文を返す。
/// `token_status` で token エンドポイントの、`logout_status` で logout エンドポイントの応答ステータスを差し替えできる。
async fn spawn_idp(
    token_status: StatusCode,
    token_body: serde_json::Value,
    jwks_kid: &str,
    logout_status: StatusCode,
) -> (String, LogoutCalls) {
    use axum::{routing::get, routing::post, Json, Router};
    let jwks = jwks_body(jwks_kid);
    let calls = LogoutCalls::default();
    let seen = Arc::clone(&calls);
    let app = Router::new()
        .route(
            "/realms/shiki/protocol/openid-connect/logout",
            post(move |body: String| {
                let seen = Arc::clone(&seen);
                async move {
                    seen.lock().unwrap().push(body);
                    logout_status
                }
            }),
        )
        .route(
            "/realms/shiki/protocol/openid-connect/token",
            post(move || {
                let body = token_body.clone();
                async move { (token_status, Json(body)) }
            }),
        )
        .route(
            "/realms/shiki/protocol/openid-connect/certs",
            get(move || {
                let jwks = jwks.clone();
                async move { Json(jwks) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/realms/shiki"), calls)
}

fn config_with(idp_base: &str, cors: Vec<String>) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 0,
            cors_allowed_origins: cors,
        },
        database: DatabaseConfig {
            url: "postgres://localhost/none".into(),
            migrate_url: None,
            max_connections: 1,
        },
        auth: AuthConfig {
            // ブラウザ向け authorize/end-session は公開 issuer 由来。
            issuer: ISSUER.into(),
            // token 交換・JWKS はモック IdP（内部 base）へ向ける。
            internal_base_url: Some(idp_base.to_string()),
            jwks_uri: Some(format!("{idp_base}/protocol/openid-connect/certs")),
            audience: AUDIENCE.into(),
            jwks_ttl_secs: 300,
            client_id: "shiki-web".into(),
            client_secret: Some("secret".into()),
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
            audit_retention: api::config::AuditRetentionConfig::default(),
        },
        vector: VectorConfig {
            backend: VectorStoreBackend::Qdrant,
        },
        rag: rag::RagConfig::default(),
        help: rag::help::HelpConfig::default(),
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

/// テストが検査できる外部セッションストアを渡して AppState を組み立てる。
fn state_with(
    config: AppConfig,
    store: Arc<dyn api::session::SessionStore>,
    authz: Arc<dyn AuthzClient>,
) -> AppState {
    let db = sqlx::postgres::PgPoolOptions::new()
        // 認証系テストは DB 不要（到達不能 URL）。lazy pool の取得待ちで 30s 掛からないよう短く。
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy(&config.database.url)
        .unwrap();
    let jwks = Arc::new(api::middleware::JwksCache::new(
        reqwest::Client::new(),
        config.auth.effective_jwks_uri(),
        Duration::from_secs(config.auth.jwks_ttl_secs),
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
    let rag_admin = std::sync::Arc::new(rag::RagAdmin::new(db.clone(), None, None));
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
        authz,
        jwks,
        sessions: store,
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
        egress_proxy: None,
        docx_composer: std::sync::Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
    }
}

/// Set-Cookie 群から指定名の Cookie 値を取り出す。
fn cookie_value(resp_cookies: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    resp_cookies.iter().find_map(|c| {
        c.strip_prefix(&prefix)
            .map(|rest| rest.split(';').next().unwrap_or("").to_string())
    })
}

fn set_cookies(resp: &axum::http::Response<Body>) -> Vec<String> {
    resp.headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

/// login → flow cookie 取り出し。state と flow cookie 文字列（"name=value"）を返す。
async fn do_login(app: axum::Router) -> (String, String) {
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "login は 303/302 リダイレクト"
    );
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.starts_with(&format!("{ISSUER}/protocol/openid-connect/auth")),
        "authorize へ。location={location}"
    );
    let cookies = set_cookies(&resp);
    let flow_val = cookie_value(&cookies, "shiki_oidc_flow").expect("flow cookie が発行される");
    // location の state クエリを取り出す。
    let url = reqwest::Url::parse(&location).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .unwrap();
    (state, format!("shiki_oidc_flow={flow_val}"))
}

/// 署名つきの access token（sub を差し替える）。
fn access_token_for(sub: &str) -> String {
    sign_token(
        KID,
        serde_json::json!({
            "sub": sub,
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": now() + 3600,
            "groups": ["/acme"],
            "email": "bob@acme.example",
        }),
    )
}

#[tokio::test]
async fn login_with_an_unusable_sub_goes_back_to_the_login_page_without_a_session() {
    // フェデレーションで Import を切った人の sub（`f:<provider>:<username>`）と、OpenFGA が受け付けてしまう `|`。
    // Keycloak の logout が誤りを返しても、ログイン画面へは同じように戻す。
    for (sub, logout_status) in [
        ("f:ldap:bob", StatusCode::NO_CONTENT),
        ("a|b", StatusCode::NO_CONTENT),
        ("f:ldap:carol", StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let token_body = serde_json::json!({
            "access_token": access_token_for(sub),
            "refresh_token": "rt",
            "expires_in": 3600,
        });
        let (idp, logout_calls) = spawn_idp(StatusCode::OK, token_body, KID, logout_status).await;
        let authz = Arc::new(Counting::default());
        let store = Arc::new(MemorySessionStore::new());
        let app = build_router(state_with(
            config_with(&idp, vec![]),
            store.clone(),
            authz.clone(),
        ));

        let (state, flow_cookie) = do_login(app.clone()).await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/auth/callback?code=abc&state={state}"))
                    .header(COOKIE, flow_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // JSON の 401 ではなく、ログイン画面へ戻す（画面が「このアカウントは使えません」を出す）。
        assert_eq!(resp.status(), StatusCode::SEE_OTHER, "{sub:?}");
        assert_eq!(
            resp.headers().get(axum::http::header::LOCATION).unwrap(),
            "/login?error=account_unusable",
            "{sub:?}"
        );
        // セッションも CSRF も出さず、相関 Cookie は捨てる。
        let cookies = set_cookies(&resp);
        assert!(
            cookie_value(&cookies, "shiki_session").is_none(),
            "{cookies:?}"
        );
        assert!(
            cookie_value(&cookies, "shiki_csrf").is_none(),
            "{cookies:?}"
        );
        assert!(cookies.iter().any(|c| c.starts_with("shiki_oidc_flow=")
            && (c.contains("Max-Age=0") || c.contains("max-age=0"))));
        // 組織の member もロールも書かない（裏の同期も走らせない）。
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(authz.calls(), 0, "{sub:?}: OpenFGA に触れない");
        // Keycloak の SSO セッションを、受け取った refresh token でサーバ側から終える（開き直したログイン画面で
        // Keycloak が同じ人を黙って通さない）。
        let calls = logout_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{sub:?}: {calls:?}");
        let form: Vec<(String, String)> = url_form(&calls[0]);
        for pair in [
            ("client_id", "shiki-web"),
            ("client_secret", "secret"),
            ("refresh_token", "rt"),
        ] {
            assert!(
                form.iter().any(|(k, v)| (k.as_str(), v.as_str()) == pair),
                "{sub:?}: {form:?}"
            );
        }
    }
}

/// `application/x-www-form-urlencoded` の本文を組に分ける。
fn url_form(body: &str) -> Vec<(String, String)> {
    reqwest::Url::parse(&format!("http://x/?{body}"))
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// 署名なしの JWT 形状トークン（refresh の backchannel 応答を模す。claims を載せる）。
fn fake_jwt(claims: serde_json::Value) -> String {
    use base64::Engine;
    let enc = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = enc(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = enc(serde_json::to_vec(&claims).unwrap().as_slice());
    format!("{header}.{payload}.sig")
}

#[tokio::test]
async fn refresh_with_an_unusable_sub_drops_the_session() {
    // この変更より前にログインした `|` を含む人のセッション。access が切れて refresh すると、新しいトークンの sub で
    // 断られ、セッションが捨てられる。
    let token_body = serde_json::json!({
        "access_token": fake_jwt(serde_json::json!({ "sub": "a|b", "groups": ["/acme"] })),
        "refresh_token": "rt2",
        "expires_in": 3600,
    });
    let (idp, _) = spawn_idp(StatusCode::OK, token_body, KID, StatusCode::NO_CONTENT).await;
    let authz = Arc::new(Counting::default());
    let store = Arc::new(MemorySessionStore::new());
    let record = SessionRecord {
        principal: Principal {
            kind: authz::PrincipalKind::User,
            id: "a|b".into(),
            email: None,
            groups: vec!["/acme".into()],
            roles: vec![],
            tenant_id: None,
        },
        tenant_id: "default".into(),
        access_token: "access".into(),
        refresh_token: Some("rt".into()),
        id_token: None,
        access_expires_at: now() - 10,
        csrf_token: "csrf".into(),
        keycloak_sid: None,
    };
    store
        .put("default", "sid-old", &record, Duration::from_secs(3600))
        .await
        .unwrap();
    let app = build_router(state_with(
        config_with(&idp, vec![]),
        store.clone(),
        authz.clone(),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header(
                    COOKIE,
                    format!(
                        "shiki_session={}",
                        encode_session_cookie("sid-old", "default")
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        store.get("default", "sid-old").await.unwrap().is_none(),
        "セッションが捨てられる"
    );
    // 古い principal で組織の member・ロールをそろえ直さない。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(authz.calls(), 0, "OpenFGA に触れない");
}
