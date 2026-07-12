//! OIDC code フロー（login → callback）と JWKS 検証の統合テスト（外部依存なし）。
//!
//! モックの OIDC token エンドポイントと JWKS エンドポイントを in-process で立て、
//! `build_router` 経由で BFF callback の token 交換・access token 検証・セッション発行を
//! 一気通貫で検証する。これにより login.rs / callback.rs / oidc.rs / middleware/jwks.rs /
//! server.rs（CORS・observe）の実経路をカバーする。
//!
//! 署名検証のため、テスト専用の RSA 鍵ペアを固定値で持つ（公開鍵を JWKS として配り、
//! 秘密鍵でトークンを RS256 署名する）。本番鍵とは無関係。

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
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionRecord, SessionStore},
    state::AppState,
};
use async_trait::async_trait;
use authz::{
    AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, ReadTupleKey, Relation, Subject,
};
use axum::{
    body::Body,
    http::{header::COOKIE, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
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

/// iss/aud/exp/sub を満たす有効な access token。
fn valid_access_token() -> String {
    sign_token(
        KID,
        serde_json::json!({
            "sub": "00000000-0000-0000-0000-000000000001",
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": now() + 3600,
            "groups": ["/acme"],
            "email": "alice@acme.example",
        }),
    )
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

/// token / JWKS を返す in-process モック IdP を立て、その base URL を返す。
/// `token_status` で token エンドポイントの応答ステータスを差し替えできる。
async fn spawn_idp(
    token_status: StatusCode,
    token_body: serde_json::Value,
    jwks_kid: &str,
) -> String {
    use axum::{routing::get, routing::post, Json, Router};
    let jwks = jwks_body(jwks_kid);
    let app = Router::new()
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
    format!("http://{addr}/realms/shiki")
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

fn state_with(config: AppConfig) -> AppState {
    state_with_store(config, Arc::new(MemorySessionStore::new()))
}

/// テストが検査できる外部セッションストアを渡して AppState を組み立てる。
fn state_with_store(config: AppConfig, store: Arc<dyn api::session::SessionStore>) -> AppState {
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
    let ui_validator = Arc::new(gui::SpecValidator::new(Arc::clone(&artifacts), db.clone()));
    let ui_specs = Arc::new(gui::UiSpecStore::new(Arc::clone(&artifacts), ui_validator));
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
    let workflow_registration = Arc::new(workflow_engine::RegistrationService::new(
        db.clone(),
        workflow_engine::DelegationStore::new(db.clone(), Arc::new(AllowAll)),
    ));
    let audit_rec = Arc::new(storage::audit::AuditRecorder::new(db.clone()));
    let workflow_summaries = Arc::new(workflow_engine::WorkflowSummaryStore::new(db.clone()));
    let workflow_layout = Arc::new(workflow_engine::EditorLayoutStore::new(db.clone()));
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
        db: api::state::ReadinessProbe::new(db),
        authz: Arc::new(AllowAll),
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

#[tokio::test]
async fn login_redirects_to_authorize_with_pkce() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    // PKCE S256 と code フローのパラメータが付与される。
    assert!(location.contains("response_type=code"));
    assert!(location.contains("code_challenge="));
    assert!(location.contains("code_challenge_method=S256"));
    assert!(set_cookies(&resp)
        .iter()
        .any(|c| c.starts_with("shiki_oidc_flow=")));
}

#[tokio::test]
async fn callback_exchanges_code_and_issues_session() {
    let token_body = serde_json::json!({
        "access_token": valid_access_token(),
        "refresh_token": "rt",
        "expires_in": 3600,
        "id_token": "id.tok.sig",
    });
    let idp = spawn_idp(StatusCode::OK, token_body, KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));

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
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "callback 成功はトップへリダイレクト"
    );
    assert_eq!(
        resp.headers().get(axum::http::header::LOCATION).unwrap(),
        "/"
    );
    let cookies = set_cookies(&resp);
    assert!(
        cookie_value(&cookies, "shiki_session").is_some(),
        "セッション Cookie が発行される"
    );
    assert!(
        cookie_value(&cookies, "shiki_csrf").is_some(),
        "CSRF Cookie が発行される"
    );
    // 相関 Cookie は破棄される（Max-Age 0）。
    assert!(cookies.iter().any(|c| c.starts_with("shiki_oidc_flow=")
        && (c.contains("Max-Age=0") || c.contains("max-age=0"))));
}

#[tokio::test]
async fn callback_with_idp_error_is_unauthorized() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/callback?error=access_denied")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_without_code_is_unauthorized() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/callback?state=x")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_with_state_mismatch_is_unauthorized() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let (_state, flow_cookie) = do_login(app.clone()).await;
    // flow cookie はあるが state クエリが一致しない → CSRF/リプレイ拒否。
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/callback?code=abc&state=wrong-state")
                .header(COOKIE, flow_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_without_flow_cookie_is_unauthorized() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/callback?code=abc&state=s")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_with_token_endpoint_4xx_is_unauthorized() {
    // token エンドポイントが invalid_grant 等 4xx → ApiError::Unauthorized。
    let idp = spawn_idp(
        StatusCode::BAD_REQUEST,
        serde_json::json!({ "error": "invalid_grant" }),
        KID,
    )
    .await;
    let app = build_router(state_with(config_with(&idp, vec![])));
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
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_with_unknown_kid_token_is_unauthorized() {
    // JWKS に存在しない kid で署名されたトークン → 検証失敗で 401。
    let token_body = serde_json::json!({
        "access_token": sign_token("unknown-kid", serde_json::json!({
            "sub": "u", "iss": ISSUER, "aud": AUDIENCE, "exp": now() + 3600,
        })),
        "expires_in": 3600,
    });
    let idp = spawn_idp(StatusCode::OK, token_body, KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
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
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_with_wrong_audience_is_unauthorized() {
    // aud がサーバの期待値と異なるトークン → audience 境界で拒否。
    let token_body = serde_json::json!({
        "access_token": sign_token(KID, serde_json::json!({
            "sub": "u", "iss": ISSUER, "aud": "other-client", "exp": now() + 3600,
        })),
        "expires_in": 3600,
    });
    let idp = spawn_idp(StatusCode::OK, token_body, KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
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
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_status_true_after_login_callback() {
    // callback で確立したセッション Cookie で /auth/session が authenticated=true を返す。
    let token_body = serde_json::json!({
        "access_token": valid_access_token(),
        "refresh_token": "rt",
        "expires_in": 3600,
    });
    let idp = spawn_idp(StatusCode::OK, token_body, KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));

    let (state, flow_cookie) = do_login(app.clone()).await;
    let cb = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback?code=abc&state={state}"))
                .header(COOKIE, flow_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let session_cookie =
        cookie_value(&set_cookies(&cb), "shiki_session").expect("セッション Cookie");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/session")
                .header(COOKIE, format!("shiki_session={session_cookie}"))
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
        Some(&serde_json::Value::Bool(true))
    );
}

#[tokio::test]
async fn cors_preflight_is_allowed_for_configured_origin() {
    // cors_allowed_origins 設定時、許可オリジンの preflight に CORS ヘッダを返す（cors_layer Some 経路）。
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(
        &idp,
        vec!["http://localhost:3000".into()],
    )));
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/healthz")
                .header(axum::http::header::ORIGIN, "http://localhost:3000")
                .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:3000")
    );
}

// --- admin プレーン（SAAS.2 テナント・プロビジョニング）の認可境界 ---

/// provisioner service account 相当のトークン（azp 指定・aud/iss/exp 有効）。
fn provisioner_token(azp: &str) -> String {
    sign_token(
        KID,
        serde_json::json!({
            "sub": "service-account-shiki-provisioner",
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": now() + 3600,
            "azp": azp,
        }),
    )
}

/// provisioner 設定が無ければ /admin/* はルート自体が存在しない（fail-closed で 404/405）。
#[tokio::test]
async fn admin_routes_absent_without_provisioner_config() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/tenants")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "config 未設定なら admin ルートは組み込まれない"
    );
}

/// /admin/* は Bearer JWT（iss/aud/exp 検証）＋ azp==provisioner を要求する。
#[tokio::test]
async fn admin_requires_provisioner_azp() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let mut config = config_with(&idp, vec![]);
    config.auth.provisioner_client_id = Some("shiki-provisioner".into());
    config.auth.provisioner_client_secret = Some("dev-secret".into());
    let app = build_router(state_with(config));

    // 1) トークン無し → 401。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/tenants")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"tenant_id":"t1","display_name":"T1","admin_email":"a@t1.example"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "無トークンは 401");

    // 2) 有効 JWT だが azp が別 client → 401（confused-deputy 防御）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/tenants")
                .header(
                    "authorization",
                    format!("Bearer {}", provisioner_token("shiki-web")),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"tenant_id":"t1","display_name":"T1","admin_email":"a@t1.example"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "azp 不一致は 401");

    // 3) 正しい azp → middleware 通過（tenant_id の禁止文字で 400 = ハンドラ到達の証跡。
    //    DB/Keycloak には到達しない）。
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/tenants")
                .header(
                    "authorization",
                    format!("Bearer {}", provisioner_token("shiki-provisioner")),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"tenant_id":"bad|id","display_name":"T","admin_email":"a@t.example"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "azp 一致で middleware を通過し、tenant_id 検証の 400 に到達する"
    );
}

/// token/JWKS ＋ Keycloak admin REST（groups/users）を兼ねるモック IdP。
/// admin_tenant_lifecycle_end_to_end 用（KeycloakAdmin は internal_base から
/// token endpoint と admin base の両方を導出するため、1 サーバで両方を提供する）。
async fn spawn_idp_with_admin() -> String {
    use axum::routing::{delete, get, post};
    use axum::{extract::Path, extract::Query, Json, Router};
    use serde_json::{json, Value};
    use std::sync::Mutex;
    let jwks = jwks_body(KID);
    // 作成済みユーザーの payload（attributes 込み）。KeycloakAdmin の tenant 照合を通すため
    // POST された内容をそのまま username 検索で返す。
    let created: Arc<Mutex<Option<Value>>> = Arc::default();
    let app = Router::new()
        .route(
            "/realms/shiki/protocol/openid-connect/token",
            post(|| async {
                Json(json!({ "access_token": "mock-admin-token", "expires_in": 60 }))
            }),
        )
        .route(
            "/realms/shiki/protocol/openid-connect/certs",
            get(move || {
                let jwks = jwks.clone();
                async move { Json(jwks) }
            }),
        )
        .route(
            "/admin/realms/shiki/groups",
            post(|| async { StatusCode::CREATED }).get(|Query(q): Query<Value>| async move {
                let name = q.get("search").and_then(Value::as_str).unwrap_or("");
                Json(json!([{ "id": "group-1", "name": name }]))
            }),
        )
        .route(
            "/admin/realms/shiki/groups/{id}",
            delete(|Path(_id): Path<String>| async { StatusCode::NO_CONTENT }),
        )
        .route("/admin/realms/shiki/users", {
            let created_post = created.clone();
            let created_get = created.clone();
            post(move |Json(body): Json<Value>| {
                let created = created_post.clone();
                async move {
                    let mut guard = created.lock().unwrap();
                    if guard.is_some() {
                        StatusCode::CONFLICT
                    } else {
                        *guard = Some(body);
                        StatusCode::CREATED
                    }
                }
            })
            .get(move |Query(q): Query<Value>| {
                let created = created_get.clone();
                async move {
                    let stored = created.lock().unwrap().clone();
                    if q.get("username").is_some() {
                        // 作成済みならその attributes を返す（tenant 照合が通る）。
                        return match stored {
                            Some(u) => Json(json!([{
                                "id": "kc-admin-1",
                                "username": "tenant-admin",
                                "attributes": u.get("attributes").cloned().unwrap_or(json!({})),
                            }])),
                            None => Json(json!([])),
                        };
                    }
                    let first = q
                        .get("first")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    if first == 0 && stored.is_some() {
                        Json(json!([{ "id": "kc-admin-1", "username": "tenant-admin" }]))
                    } else {
                        Json(json!([]))
                    }
                }
            })
        })
        .route(
            "/admin/realms/shiki/users/{id}",
            delete(|Path(_id): Path<String>| async { StatusCode::NO_CONTENT }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/realms/shiki")
}

/// SAAS.2 テナント・ライフサイクル e2e（作成→冪等再作成→削除→tombstone 再利用拒否）。
/// 実 Postgres が必要（`STORAGE_TEST_DATABASE_URL` 設定時のみ・CI の coverage ジョブで実走）。
#[tokio::test]
async fn admin_tenant_lifecycle_end_to_end() {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    // migration を適用しておく（他テストと共存できる冪等適用）。
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("Postgres 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migration 適用");

    let idp = spawn_idp_with_admin().await;
    let mut config = config_with(&idp, vec![]);
    config.database.url = db_url;
    config.auth.provisioner_client_id = Some("shiki-provisioner".into());
    config.auth.provisioner_client_secret = Some("dev-secret".into());
    let app = build_router(state_with(config));
    let token = provisioner_token("shiki-provisioner");
    let tenant_id = format!("t{}", uuid::Uuid::new_v4().simple());

    // --- 作成: 201・一時パスワードが返る。 ---
    let body = serde_json::json!({
        "tenant_id": tenant_id,
        "display_name": "Test Corp",
        "admin_email": "admin@test.example",
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/tenants")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(created["tenant_id"], tenant_id.as_str());
    assert_eq!(created["status"], "active");
    assert_eq!(created["admin_user_id"], "kc-admin-1");

    // --- 冪等: 再作成も 201（tenant 行は active のまま）。 ---
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/tenants")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "再実行も成功（冪等）");

    // --- 削除: 204・tombstone 化。 ---
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/admin/tenants/{tenant_id}"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let status: String = sqlx::query_scalar("SELECT status FROM tenant WHERE tenant_id = $1")
        .bind(&tenant_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "deleted", "tombstone が残る");

    // --- tombstone の tenant_id 再利用は拒否（400）。 ---
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/tenants")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "tombstone 再利用は拒否"
    );
}

// --- OIDC Back-Channel Logout（#91: 無効化/削除ユーザーの即時失効） ---

const BCL_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";

/// テスト用セッションレコード（sub/sid 指定）。
fn bcl_record(sub: &str, sid: Option<&str>) -> SessionRecord {
    SessionRecord {
        principal: authz::Principal {
            kind: authz::PrincipalKind::User,
            id: sub.into(),
            email: None,
            groups: vec!["/acme".into()],
            roles: vec![],
            tenant_id: Some("default".into()),
        },
        tenant_id: "default".into(),
        access_token: "access".into(),
        refresh_token: None,
        id_token: None,
        access_expires_at: now() + 3600,
        csrf_token: "csrf".into(),
        keycloak_sid: sid.map(str::to_string),
    }
}

/// logout_token を form-encoded で /auth/backchannel-logout へ POST する。
async fn post_backchannel_logout(
    app: axum::Router,
    logout_token: &str,
) -> axum::http::Response<Body> {
    let body = format!("logout_token={logout_token}");
    app.oneshot(
        Request::builder()
            .method(Method::POST)
            .uri("/auth/backchannel-logout")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn backchannel_logout_by_sid_revokes_only_that_session() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let store = Arc::new(MemorySessionStore::new());
    // 同一ユーザーの 2 セッション（別 SSO セッション）。sid=A のみ失効させる。
    store
        .put(
            "default",
            "sess-a",
            &bcl_record("user-1", Some("sid-A")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    store
        .put(
            "default",
            "sess-b",
            &bcl_record("user-1", Some("sid-B")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with_store(config_with(&idp, vec![]), store.clone()));

    let logout_token = sign_token(
        KID,
        serde_json::json!({
            "iss": ISSUER,
            "aud": "shiki-web",
            "iat": now(),
            "jti": "jti-1",
            "sid": "sid-A",
            "events": { BCL_EVENT: {} },
        }),
    );
    let resp = post_backchannel_logout(app, &logout_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "検証済み logout_token は 200"
    );

    assert!(
        store.get("default", "sess-a").await.unwrap().is_none(),
        "sid-A のセッションは失効する"
    );
    assert!(
        store.get("default", "sess-b").await.unwrap().is_some(),
        "sid-B のセッションは残る（sid スコープ）"
    );
}

#[tokio::test]
async fn backchannel_logout_by_sub_revokes_all_user_sessions() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sess-a",
            &bcl_record("user-1", Some("sid-A")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    store
        .put(
            "default",
            "sess-b",
            &bcl_record("user-1", Some("sid-B")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    // 別ユーザーのセッションは巻き込まない。
    store
        .put(
            "default",
            "sess-c",
            &bcl_record("user-2", Some("sid-C")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with_store(config_with(&idp, vec![]), store.clone()));

    // sid 無し・sub のみ → 当該ユーザーの全セッションを失効（無効化/削除シナリオ）。
    let logout_token = sign_token(
        KID,
        serde_json::json!({
            "iss": ISSUER,
            "aud": "shiki-web",
            "iat": now(),
            "jti": "jti-sub-1",
            "sub": "user-1",
            "events": { BCL_EVENT: {} },
        }),
    );
    let resp = post_backchannel_logout(app, &logout_token).await;
    assert_eq!(resp.status(), StatusCode::OK);

    assert!(
        store.get("default", "sess-a").await.unwrap().is_none(),
        "user-1 sess-a 失効"
    );
    assert!(
        store.get("default", "sess-b").await.unwrap().is_none(),
        "user-1 sess-b 失効"
    );
    assert!(
        store.get("default", "sess-c").await.unwrap().is_some(),
        "別ユーザー user-2 のセッションは残る"
    );
}

#[tokio::test]
async fn backchannel_logout_rejects_token_without_logout_event() {
    // logout イベント宣言の無いトークン（通常の access token 等）では失効させない。
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sess-a",
            &bcl_record("user-1", Some("sid-A")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with_store(config_with(&idp, vec![]), store.clone()));

    let not_a_logout_token = sign_token(
        KID,
        serde_json::json!({
            "iss": ISSUER,
            "aud": "shiki-web",
            "iat": now(),
            "sid": "sid-A",
            // events 無し
        }),
    );
    let resp = post_backchannel_logout(app, &not_a_logout_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "events 宣言が無い token は 400"
    );
    assert!(
        store.get("default", "sess-a").await.unwrap().is_some(),
        "セッションは失効しない（誤用防御）"
    );
}

#[tokio::test]
async fn backchannel_logout_rejects_wrong_audience() {
    // aud が RP（client_id=shiki-web）でない logout_token は拒否（confused-deputy 防御）。
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sess-a",
            &bcl_record("user-1", Some("sid-A")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with_store(config_with(&idp, vec![]), store.clone()));

    let wrong_aud = sign_token(
        KID,
        serde_json::json!({
            "iss": ISSUER,
            "aud": "some-other-client",
            "iat": now(),
            "sid": "sid-A",
            "events": { BCL_EVENT: {} },
        }),
    );
    let resp = post_backchannel_logout(app, &wrong_aud).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "aud 不一致は 400");
    assert!(store.get("default", "sess-a").await.unwrap().is_some());
}

#[tokio::test]
async fn backchannel_logout_rejects_replayed_jti() {
    // 同一 jti の再送は「処理済み」として 200 を返しつつ、再度の失効処理を行わない
    // （リプレイ防止・OIDC BCL §2.6）。2 回目までに再作成したセッションが消えないことで確認する。
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sess-a",
            &bcl_record("user-1", Some("sid-A")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with_store(config_with(&idp, vec![]), store.clone()));

    let token = sign_token(
        KID,
        serde_json::json!({
            "iss": ISSUER,
            "aud": "shiki-web",
            "iat": now(),
            "jti": "jti-replay",
            "sid": "sid-A",
            "events": { BCL_EVENT: {} },
        }),
    );
    // 1 回目: 失効。
    let resp = post_backchannel_logout(app.clone(), &token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(store.get("default", "sess-a").await.unwrap().is_none());

    // 同一 sid で別セッションを再作成し、同じ jti を再送する。
    store
        .put(
            "default",
            "sess-a2",
            &bcl_record("user-1", Some("sid-A")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let resp = post_backchannel_logout(app, &token).await;
    assert_eq!(resp.status(), StatusCode::OK, "再送も 200（べき等）");
    assert!(
        store.get("default", "sess-a2").await.unwrap().is_some(),
        "リプレイは再処理されず、後続セッションは残る"
    );
}

#[tokio::test]
async fn backchannel_logout_rejects_stale_iat() {
    // 鮮度窓（120s + skew 60s）を超えた古い logout_token は拒否する。
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sess-a",
            &bcl_record("user-1", Some("sid-A")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with_store(config_with(&idp, vec![]), store.clone()));

    let stale = sign_token(
        KID,
        serde_json::json!({
            "iss": ISSUER,
            "aud": "shiki-web",
            "iat": now() - 3600,
            "jti": "jti-stale",
            "sid": "sid-A",
            "events": { BCL_EVENT: {} },
        }),
    );
    let resp = post_backchannel_logout(app, &stale).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "古い iat は 400");
    assert!(store.get("default", "sess-a").await.unwrap().is_some());
}

#[tokio::test]
async fn backchannel_logout_rejects_missing_jti() {
    // jti 欠落の logout_token は拒否する（OIDC BCL §2.4 必須）。
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let store = Arc::new(MemorySessionStore::new());
    store
        .put(
            "default",
            "sess-a",
            &bcl_record("user-1", Some("sid-A")),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with_store(config_with(&idp, vec![]), store.clone()));

    let no_jti = sign_token(
        KID,
        serde_json::json!({
            "iss": ISSUER,
            "aud": "shiki-web",
            "iat": now(),
            "sid": "sid-A",
            "events": { BCL_EVENT: {} },
        }),
    );
    let resp = post_backchannel_logout(app, &no_jti).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "jti 欠落は 400");
    assert!(store.get("default", "sess-a").await.unwrap().is_some());
}
