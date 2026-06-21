//! ルーティング・認証ミドルウェアのレベルの統合テスト（外部依存なし）。
//!
//! - `/healthz` は認証不要で 200。
//! - `/me` は Authorization ヘッダ無しで 401（負例。正例の E2E は compose smoke）。

use std::sync::Arc;

use api::{build_router, config::*, state::AppState};
use async_trait::async_trait;
use authz::{AuthzClient, AuthzError, FgaObject, Relation, Subject};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

/// 常に allow を返すモック（/me の認可前に 401 になるため呼ばれない）。
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
}

fn test_state() -> AppState {
    let config = AppConfig {
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 0,
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
            cookie_name: "shiki_session".into(),
            csrf_cookie_name: "shiki_csrf".into(),
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
        },
        vector: VectorConfig {
            backend: VectorStoreBackend::Qdrant,
        },
        llm: LlmConfig {
            backend: LlmBackend::Vllm,
        },
    };
    // lazy 接続なので実際の Postgres は不要。
    let db = PgPoolOptions::new()
        .connect_lazy(&config.database.url)
        .unwrap();
    let jwks = Arc::new(api::middleware::JwksCache::new(
        reqwest::Client::new(),
        config.auth.effective_jwks_uri(),
        std::time::Duration::from_secs(300),
    ));
    AppState {
        config: Arc::new(config),
        db,
        authz: Arc::new(AllowAll),
        jwks,
        sessions: Arc::new(api::session::MemorySessionStore::new()),
        http: reqwest::Client::new(),
    }
}

#[tokio::test]
async fn healthz_is_public_and_ok() {
    let app = build_router(test_state());
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
async fn me_without_token_is_unauthorized() {
    let app = build_router(test_state());
    let resp = app
        .oneshot(Request::builder().uri("/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn openapi_json_is_served() {
    let app = build_router(test_state());
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
