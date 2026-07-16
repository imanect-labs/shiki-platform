//! CSV クエリ/パッチ API のルートレベル結合テスト（Task 11P.7・実 Postgres＋MinIO＋隔離ランナー）。
//!
//! `STORAGE_TEST_DATABASE_URL`＋`STORAGE_TEST_S3_ENDPOINT`＋隔離ランナーバイナリが揃う時のみ
//! 実行する（未設定はスキップ）。HTTP ルータ経由で schema/rows/query/patch/save を通し、
//! 楽観ロック 409・SQL 拒否 400 を検証する（routes/tabular.rs のカバレッジ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionStore},
    state::AppState,
};
use async_trait::async_trait;
use authz::{
    AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal, ReadTupleKey, Relation,
    Subject,
};
use axum::body::Body;
use axum::http::{header::COOKIE, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;

struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
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

/// no-op ObjectStore（BundleStore の AppState 充足用・本テストは bundle 面を叩かない）。
struct NoopStore;

#[async_trait]
impl storage::object_store::ObjectStore for NoopStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _k: &str,
        _t: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://noop".into())
    }
    async fn presign_put(
        &self,
        _k: &str,
        _t: Duration,
        _l: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://noop".into())
    }
    async fn presign_get(
        &self,
        _k: &str,
        _t: Duration,
        _f: Option<&str>,
        _c: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://noop".into())
    }
    async fn read_and_hash(&self, _k: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("noop".into()))
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
        Err(storage::ObjectStoreError::NotFound("noop".into()))
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

fn runner_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SHIKI_TABULAR_RUNNER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    // ワークスペースルートから見た除外クレートの既定ビルド先。
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    for profile in ["release", "debug"] {
        let p = root
            .join("crates/tabular/runner/target")
            .join(profile)
            .join("shiki-tabular-runner");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn config(db_url: &str) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "0.0.0.0".into(),
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
            backend: LlmBackend::Stub,
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
        office: api::config::OfficeConfig::default(),
    }
}

struct Env {
    app: axum::Router,
    storage: Arc<storage::StorageService>,
    ctx: authz::AuthContext,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Some(runner) = runner_path() else {
        eprintln!("隔離ランナー未ビルドのためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let access_key =
        std::env::var("STORAGE_TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let secret_key =
        std::env::var("STORAGE_TEST_S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".into());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");

    let authz: Arc<dyn AuthzClient> = Arc::new(AllowAll);
    let s3 = storage::object_store::S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: format!("tabtest-{}", Uuid::new_v4().simple()),
        access_key,
        secret_key,
        region: "us-east-1".into(),
        presign_get_ttl_secs: 900,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let object_store = Arc::new(storage::S3ObjectStore::new(&s3)) as Arc<dyn storage::ObjectStore>;
    object_store.ensure_bucket().await.expect("bucket");
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        object_store,
        Arc::clone(&authz),
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));
    let tabular = Arc::new(tabular::TabularService::new(
        Arc::clone(&storage),
        tabular::RunnerConfig::new(
            runner.to_string_lossy().to_string(),
            Duration::from_secs(20),
        ),
        tabular::Quotas::default(),
    ));

    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-tab",
            &api::session::SessionRecord {
                principal: principal(),
                tenant_id: "default".into(),
                access_token: "a".into(),
                refresh_token: None,
                id_token: None,
                access_expires_at: chrono::Utc::now().timestamp() + 3600,
                csrf_token: "csrf-tab".into(),
                keycloak_sid: None,
            },
            Duration::from_secs(3600),
        )
        .await
        .unwrap();

    let app = build_app(
        &pool,
        config(&db_url),
        storage.clone(),
        tabular,
        sessions,
        &authz,
    );
    Some(Env {
        app,
        storage,
        ctx: authz::AuthContext::new(principal(), "acme".into(), "default".into()),
    })
}

fn principal() -> Principal {
    Principal {
        kind: authz::PrincipalKind::User,
        id: "alice".into(),
        email: None,
        groups: vec!["/acme".into()],
        roles: vec![],
        tenant_id: None,
    }
}

/// 最小 AppState を組んでルータを返す（tabular ルートのみ叩く）。
fn build_app(
    pool: &sqlx::PgPool,
    config: AppConfig,
    storage: Arc<storage::StorageService>,
    tabular: Arc<tabular::TabularService>,
    sessions: Arc<MemorySessionStore>,
    authz: &Arc<dyn AuthzClient>,
) -> axum::Router {
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(authz),
    ));
    let directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    let data_store = Arc::new(data::DataStore::new(
        pool.clone(),
        Arc::clone(authz),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::clone(&directory),
            storage: Arc::clone(&storage),
        }),
    ));
    let collab = Arc::new(collab::CollabHub::new(
        pool.clone(),
        Arc::clone(authz),
        Arc::clone(&storage),
    ));
    let ui_validator = Arc::new(gui::SpecValidator::new(
        Arc::clone(&artifacts),
        pool.clone(),
    ));
    let mini_app_code = Arc::new(app_platform::MiniAppCodeStore::new(
        Arc::clone(&artifacts),
        app_platform::Registry::new(pool.clone()),
    ));
    let installs = Arc::new(app_platform::InstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        Arc::clone(&mini_app_code),
        Arc::clone(&data_store),
        Arc::clone(authz),
        None,
        vec![],
    ));
    let bundles = Arc::new(app_platform::BundleStore::new(
        Arc::new(NoopStore),
        Arc::clone(authz),
        storage::audit::AuditRecorder::new(pool.clone()),
    ));
    let app_usage = Arc::new(app_platform::AppUsageStore::new(
        pool.clone(),
        Arc::clone(authz),
    ));
    let state = AppState {
        config: Arc::new(config),
        db: api::state::ReadinessProbe::new(pool.clone()),
        authz: Arc::clone(authz),
        jwks: Arc::new(api::middleware::JwksCache::new(
            reqwest::Client::new(),
            "http://localhost/jwks".into(),
            Duration::from_secs(300),
        )),
        sessions,
        http: reqwest::Client::new(),
        storage: Arc::clone(&storage),
        collab,
        tabular,
        artifacts: Arc::clone(&artifacts),
        data: Arc::clone(&data_store),
        data_views: Arc::new(data::DataViewStore::new(
            Arc::clone(&artifacts),
            (*data_store).clone(),
        )),
        fsms: Arc::new(data::FsmStore::new(
            Arc::clone(&artifacts),
            (*data_store).clone(),
        )),
        mini_app_code: Arc::clone(&mini_app_code),
        installs,
        bundles,
        app_usage,
        ui_specs: Arc::new(gui::UiSpecStore::new(Arc::clone(&artifacts), ui_validator)),
        ui_actions: Arc::new(gui::ActionDispatcher::new(
            storage::audit::AuditRecorder::new(pool.clone()),
        )),
        skills: Arc::new(gui::SkillStore::new(Arc::clone(&artifacts))),
        mini_apps: Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), pool.clone())),
        secrets: None,
        workflows: Arc::new(workflow_engine::WorkflowStore::new(Arc::clone(&artifacts))),
        workflow_launcher: None,
        workflow_runs: None,
        workflow_registration: Arc::new(workflow_engine::RegistrationService::new(
            pool.clone(),
            workflow_engine::DelegationStore::new(pool.clone(), Arc::clone(authz)),
        )),
        workflow_summaries: Arc::new(workflow_engine::WorkflowSummaryStore::new(pool.clone())),
        workflow_layout: Arc::new(workflow_engine::EditorLayoutStore::new(pool.clone())),
        audit: Arc::new(storage::audit::AuditRecorder::new(pool.clone())),
        directory,
        tenants: Arc::new(storage::TenantStore::new(pool.clone())),
        search: None,
        chat: None,
        rag_admin: Arc::new(rag::RagAdmin::new(pool.clone(), None, None)),
        office: None,
    };
    build_router(state)
}

fn get(app: &axum::Router, path: &str) -> Request<Body> {
    let _ = app;
    Request::builder()
        .method("GET")
        .uri(path)
        .header(COOKIE, "shiki_session=sid-tab.default")
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(COOKIE, "shiki_session=sid-tab.default; shiki_csrf=csrf-tab")
        .header("content-type", "application/json")
        .header("x-csrf-token", "csrf-tab")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

#[tokio::test]
async fn tabular_schema_rows_query_patch_flow() {
    let Some(env) = setup().await else { return };
    // CSV を作成（storage 経由）。
    let name = format!("data-{}.csv", Uuid::new_v4());
    let node = env
        .storage
        .write_file_internal(
            &env.ctx,
            None,
            &name,
            b"id,score\n1,10\n2,30\n3,20\n",
            "text/csv",
            None,
        )
        .await
        .expect("create csv");
    let id = node.id;

    // schema。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(get(&env.app, &format!("/files/{id}/tabular/schema")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["columns"], json!(["id", "score"]));
    assert_eq!(body["total_rows"], json!(3));

    // rows。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(get(&env.app, &format!("/files/{id}/tabular/rows?offset=0")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["rows"].as_array().unwrap().len(), 3);

    // query（RO SELECT）。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(post(
                &format!("/files/{id}/tabular/query"),
                json!({"sql": "SELECT id FROM data WHERE CAST(score AS INT) >= 20 ORDER BY id"}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["rows"].as_array().unwrap().len(), 2);

    // query（DML は 400 拒否）。
    let (st, _b) = json_body(
        env.app
            .clone()
            .oneshot(post(
                &format!("/files/{id}/tabular/query"),
                json!({"sql": "DROP TABLE data"}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // patch（cell 更新・base_rev=現行）。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(post(
                &format!("/files/{id}/tabular/patch"),
                json!({"base_rev": node.version, "ops": [
                    {"op": "cell_update", "row": 0, "col": 1, "value": "999"}
                ]}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let new_ver = body["version"].as_i64().unwrap();
    assert_eq!(new_ver, node.version + 1);

    // patch（古い base_rev は 409）。
    let (st, _b) = json_body(
        env.app
            .clone()
            .oneshot(post(
                &format!("/files/{id}/tabular/patch"),
                json!({"base_rev": node.version, "ops": [
                    {"op": "cell_update", "row": 0, "col": 1, "value": "x"}
                ]}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // save（新規 CSV）。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(post(
                "/tabular/save",
                json!({"parent_id": null, "name": format!("saved-{}", Uuid::new_v4()), "csv": "a,b\n1,2\n"}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert!(body["node_id"].is_string());
}
