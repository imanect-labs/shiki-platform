//! ルーティング・セッション認証のレベルの統合テスト（外部依存なし）。
//!
//! BFF + セッション Cookie 方式（#55）の不変条件を検証する:
//! - 公開ルート（/healthz）は認証不要で 200。
//! - /me はセッション Cookie 無しで 401、有効セッションで 200。
//! - セッション失効（ストア削除）で次リクエストが 401。
//! - access token 期限切れでも refresh で継続（401 にならない）。
//! - 状態変更（logout）は double-submit CSRF が無いと 403。レスポンスにトークンを出さない。
//! - セッションキーが tenant_id でスコープされ、他テナントからは引けない。

use std::{sync::Arc, time::Duration};

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionRecord, SessionStore},
    state::AppState,
};
use async_trait::async_trait;
use authz::{AuthzClient, AuthzError, FgaObject, Principal, Relation, Subject};
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
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }

    async fn write_tuple(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<(), AuthzError> {
        Ok(())
    }

    async fn delete_tuple(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<(), AuthzError> {
        Ok(())
    }
}

/// ストレージのバイト層スタブ（/me・認証フローのテストでは呼ばれない）。
struct FakeStore;

#[async_trait]
impl storage::object_store::ObjectStore for FakeStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(
        &self,
        _key: &str,
        _ttl: Duration,
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
    async fn exists(&self, _key: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _src: &str, _dst: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<(), storage::ObjectStoreError> {
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
        },
        vector: VectorConfig {
            backend: VectorStoreBackend::Qdrant,
        },
        llm: LlmConfig {
            backend: LlmBackend::Vllm,
        },
    }
}

/// 指定のセッションストアと（任意の）OIDC 内部 base で AppState を組み立てる。
fn state_with(sessions: Arc<dyn SessionStore>, internal_base_url: Option<String>) -> AppState {
    let mut config = base_config();
    config.auth.internal_base_url = internal_base_url;
    let db = PgPoolOptions::new()
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
    ));
    AppState {
        config: Arc::new(config),
        db,
        authz: Arc::new(AllowAll),
        jwks,
        sessions,
        http: reqwest::Client::new(),
        storage,
    }
}

fn test_principal() -> Principal {
    Principal {
        id: "00000000-0000-0000-0000-000000000001".into(),
        email: Some("alice@acme.example".into()),
        groups: vec!["/acme".into()],
        dept: Some("engineering".into()),
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
                .header(COOKIE, "shiki_session=sid-valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn revoked_session_is_immediately_unauthorized() {
    // ストアに存在しない session id を指す Cookie は即 401（セッション削除＝即時失効）。
    let app = build_router(state_with(Arc::new(MemorySessionStore::new()), None));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header(COOKIE, "shiki_session=already-deleted")
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
                .header(COOKIE, "shiki_session=sid-exp")
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
                .header(COOKIE, "shiki_session=sid-tr")
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
                .header(COOKIE_H, "shiki_session=sid-dead")
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
                .header(COOKIE, "shiki_session=sid-dead")
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
                .header(COOKIE, "shiki_session=sid-lo; shiki_csrf=csrf-tok")
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
                .header(COOKIE, "shiki_session=sid-lo2; shiki_csrf=csrf-tok")
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
