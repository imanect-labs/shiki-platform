//! 構造化データ API（テーブル＋レコード）の HTTP 統合テスト（Task 9.2/9.3/9.5）。
//!
//! セッション Cookie 認証で table 作成 → record CRUD → 一覧/件数/リビジョン/削除を
//! 実 Postgres 経由で一気通貫に検証する（`STORAGE_TEST_DATABASE_URL`・authz は AllowAll）。

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
use sqlx::PgPool;
use tower::ServiceExt;

struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _o: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

struct FakeStore;

#[async_trait]
impl storage::object_store::ObjectStore for FakeStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _k: &str,
        _t: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://fake/internal".into())
    }
    async fn presign_put(
        &self,
        _k: &str,
        _t: Duration,
        _l: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://test/put".into())
    }
    async fn presign_get(
        &self,
        _k: &str,
        _t: Duration,
        _f: Option<&str>,
        _c: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://test/get".into())
    }
    async fn read_and_hash(&self, _k: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("test".into()))
    }
    async fn put_object(
        &self,
        _k: &str,
        _b: Vec<u8>,
        _c: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn get_object(&self, _k: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("test".into()))
    }
    async fn exists(&self, _k: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _s: &str, _d: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _k: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _p: &str,
        _c: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _k: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

async fn setup() -> Option<PgPool> {
    let url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
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
    }
}

fn state_with(pool: PgPool, sessions: Arc<dyn SessionStore>) -> AppState {
    let config = base_config();
    let db = pool;
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
    AppState {
        config: Arc::new(config),
        db: api::state::ReadinessProbe::new(db),
        authz: Arc::new(AllowAll),
        jwks,
        sessions,
        http: reqwest::Client::new(),
        storage,
        artifacts,
        data: data_store,
        data_views,
        fsms,
        ui_specs,
        ui_actions,
        skills,
        mini_apps,
        secrets: None,
        workflows,
        workflow_launcher: None,
        workflow_runs: None,
        directory,
        tenants,
        search: None,
        chat: None,
        rag_admin,
    }
}

fn session_record(csrf: &str) -> SessionRecord {
    SessionRecord {
        principal: Principal {
            kind: authz::PrincipalKind::User,
            id: "00000000-0000-0000-0000-000000000001".into(),
            email: Some("alice@acme.example".into()),
            groups: vec!["/acme".into()],
            roles: vec!["engineering".into()],
            tenant_id: None,
        },
        tenant_id: "default".into(),
        access_token: "access".into(),
        refresh_token: None,
        id_token: None,
        access_expires_at: chrono::Utc::now().timestamp() + 3600,
        csrf_token: csrf.into(),
        keycloak_sid: None,
    }
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

/// table 作成 → record 作成/取得/更新/一覧/件数/リビジョン/削除を HTTP 経由で通す。
#[tokio::test]
async fn table_and_record_crud_over_http() {
    let Some(pool) = setup().await else { return };
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-data",
            &session_record("csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(pool, sessions));
    let cookie = "shiki_session=sid-data.default; shiki_csrf=csrf";

    // table 作成（title=text/indexed・amount=number/indexed）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/data/tables")
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "name": "expense",
                        "schema": {
                            "fields": [
                                {"name": "title", "type": "text", "indexed": true},
                                {"name": "amount", "type": "number", "indexed": true},
                            ]
                        },
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let table_id = body_json(resp).await["id"].as_str().unwrap().to_string();

    // table 取得。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/data/tables/{table_id}"))
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // record 作成。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/data/tables/{table_id}/records"))
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "data": {"title": "office-supplies", "amount": 100},
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let rec = body_json(resp).await;
    let rec_id = rec["id"].as_str().unwrap().to_string();
    assert_eq!(rec["rev"], 1);

    // record 取得。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/data/tables/{table_id}/records/{rec_id}"))
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // record 更新（merge patch・楽観ロック）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/data/tables/{table_id}/records/{rec_id}"))
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "patch": {"amount": 250},
                        "expected_rev": 1,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["rev"], 2);

    // 一覧（title 完全一致フィルタ）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/data/tables/{table_id}/records?filter_field=title&filter_value=office-supplies"
                ))
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // 件数（行述語適用済み）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/data/tables/{table_id}/records/count"))
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["count"], 1);

    // リビジョン履歴（作成＋更新の 2 世代）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/data/tables/{table_id}/records/{rec_id}/revisions"
                ))
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!body_json(resp).await["items"]
        .as_array()
        .unwrap()
        .is_empty());

    // 削除（楽観ロック・rev=2）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/data/tables/{table_id}/records/{rec_id}?expected_rev=2"
                ))
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 削除後は 404（不可視/不在は同形状）。
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/data/tables/{table_id}/records/{rec_id}"))
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
