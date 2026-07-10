//! ワークフロー API のルートレベル結合テスト（実 Postgres・セッション認証・Task 10.14）。
//!
//! 保存 → 一覧 → layout → run 起動 → 履歴（詳細/step/イベント）→ cancel / retry(resume/new) →
//! SSE（terminal リプレイ）までを **HTTP ルータ経由**で通す（ハンドラ層の回帰と coverage）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::sync::Arc;
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
use axum::body::Body;
use axum::http::{header::COOKIE, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;
use workflow_engine::{
    NodeContext, NodeExecutor, NodeResult, RunStore, WorkerConfig, WorkflowWorker,
};

/// 全許可 authz（ルート層の配線検証用・実 FGA は registration IT が担う）。
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
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// ノード id `boom` だけ permanent 失敗する pass-through executor。
struct PassExecutor;

#[async_trait]
impl NodeExecutor for PassExecutor {
    async fn execute(&self, _t: &str, _p: &Value, ctx: &NodeContext) -> NodeResult {
        if ctx.step_path == "boom" {
            NodeResult::fail("boom", "意図的失敗", false)
        } else {
            NodeResult::ok(json!({ "from": ctx.step_path }))
        }
    }
}

fn test_principal() -> Principal {
    Principal {
        kind: authz::PrincipalKind::User,
        id: "00000000-0000-0000-0000-000000000001".into(),
        email: Some("alice@acme.example".into()),
        groups: vec!["/acme".into()],
        roles: vec![],
        tenant_id: None,
    }
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
    }
}

struct Env {
    app: axum::Router,
    pool: sqlx::PgPool,
    cookie: &'static str,
    csrf: &'static str,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");

    // セッション（tenant=default・csrf 供給）。
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-wf",
            &SessionRecord {
                principal: test_principal(),
                tenant_id: "default".into(),
                access_token: "access".into(),
                refresh_token: None,
                id_token: None,
                access_expires_at: chrono::Utc::now().timestamp() + 3600,
                csrf_token: "csrf-wf".into(),
                keycloak_sid: None,
            },
            Duration::from_secs(3600),
        )
        .await
        .unwrap();

    let authz: Arc<dyn AuthzClient> = Arc::new(AllowAll);
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz),
    ));
    let workflows = Arc::new(workflow_engine::WorkflowStore::new(Arc::clone(&artifacts)));
    let runs = RunStore::new(pool.clone());
    let delegation = workflow_engine::DelegationStore::new(pool.clone(), Arc::clone(&authz));
    let launcher = Arc::new(workflow_engine::WorkflowRunLauncher::new(
        delegation.clone(),
        (*workflows).clone(),
        runs.clone(),
    ));
    let ui_validator = Arc::new(gui::SpecValidator::new(
        Arc::clone(&artifacts),
        pool.clone(),
    ));
    // 構造化データ（Phase 9）: workflow ルートは触らないが AppState 構築に必要。
    let data_directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    let data_storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        Arc::new(NoopStore),
        Arc::clone(&authz),
        Duration::from_secs(120),
        Duration::from_secs(900),
        1024,
    ));
    let data_store = Arc::new(data::DataStore::new(
        pool.clone(),
        Arc::clone(&authz),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::clone(&data_directory),
            storage: Arc::clone(&data_storage),
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
    let state = AppState {
        config: Arc::new(config(&db_url)),
        db: api::state::ReadinessProbe::new(pool.clone()),
        authz: Arc::clone(&authz),
        jwks: Arc::new(api::middleware::JwksCache::new(
            reqwest::Client::new(),
            "http://localhost/jwks".into(),
            Duration::from_secs(300),
        )),
        sessions,
        http: reqwest::Client::new(),
        storage: Arc::new(storage::StorageService::new(
            pool.clone(),
            Arc::new(NoopStore),
            Arc::clone(&authz),
            Duration::from_secs(120),
            Duration::from_secs(900),
            1024,
        )),
        artifacts: Arc::clone(&artifacts),
        ui_specs: Arc::new(gui::UiSpecStore::new(Arc::clone(&artifacts), ui_validator)),
        ui_actions: Arc::new(gui::ActionDispatcher::new(
            storage::audit::AuditRecorder::new(pool.clone()),
        )),
        skills: Arc::new(gui::SkillStore::new(Arc::clone(&artifacts))),
        mini_apps: Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), pool.clone())),
        data: data_store,
        data_views,
        fsms,
        mini_app_code,
        secrets: None,
        workflows,
        workflow_launcher: Some(launcher),
        workflow_registration: Arc::new(workflow_engine::RegistrationService::new(
            pool.clone(),
            delegation,
        )),
        workflow_summaries: Arc::new(workflow_engine::WorkflowSummaryStore::new(pool.clone())),
        workflow_layout: Arc::new(workflow_engine::EditorLayoutStore::new(pool.clone())),
        audit: Arc::new(storage::audit::AuditRecorder::new(pool.clone())),
        workflow_runs: Some(Arc::new(runs)),
        directory: Arc::new(storage::DirectoryStore::new(pool.clone())),
        tenants: Arc::new(storage::TenantStore::new(pool.clone())),
        search: None,
        chat: None,
        rag_admin: Arc::new(rag::RagAdmin::new(pool.clone(), None, None)),
    };
    Some(Env {
        app: build_router(state),
        pool,
        cookie: "shiki_session=sid-wf.default; shiki_csrf=csrf-wf",
        csrf: "csrf-wf",
    })
}

/// ObjectStore の no-op（workflow ルートはストレージ本体を触らない）。
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

async fn req(env: &Env, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(COOKIE, env.cookie)
        .header("x-csrf-token", env.csrf);
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = env
        .app
        .clone()
        .oneshot(b.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn simple_ir(name: &str, second_node: &str) -> Value {
    json!({
        "ir_version": 1,
        "name": name,
        "display_name": "テストフロー",
        "declared_scopes": ["storage.read"],
        "triggers": [{ "kind": "interactive" }],
        "nodes": [
            { "id": "a", "type": "storage.read",
              "params": { "file": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" } },
            { "id": second_node, "type": "storage.read",
              "params": { "file": { "$from": "nodes.a.output", "path": "/from" } } }
        ],
        "edges": [{ "from": "a", "to": second_node }]
    })
}

#[tokio::test]
async fn workflow_http_end_to_end() {
    let Some(env) = setup().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let name_ok = format!("http-ok-{}", &suffix[..12]);
    let name_fail = format!("http-fail-{}", &suffix[..12]);
    let worker = WorkflowWorker::new(
        RunStore::new(env.pool.clone()),
        Arc::new(PassExecutor),
        WorkerConfig::default(),
    )
    .scoped_to_tenant("default");

    // --- 保存（V1〜V7 を通る IR）。
    let (st, body) = req(
        &env,
        "POST",
        "/workflows",
        Some(json!({ "ir": simple_ir(&name_ok, "b") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let wf_ok = body["id"].as_str().unwrap().to_string();

    let (st, body) = req(
        &env,
        "POST",
        "/workflows",
        Some(json!({ "ir": simple_ir(&name_fail, "boom") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let wf_fail = body["id"].as_str().unwrap().to_string();

    // --- 一覧（要約射影）。
    let (st, body) = req(&env, "GET", "/workflows", None).await;
    assert_eq!(st, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["id"] == json!(wf_ok)
        && i["display_name"] == json!("テストフロー")
        && i["enabled_status"] == json!("none")
        && i["trigger_kinds"] == json!(["interactive"])));

    // --- layout roundtrip。
    let layout = json!({ "positions": { "a": { "x": 100.0, "y": 50.0 } } });
    let (st, _) = req(
        &env,
        "PUT",
        &format!("/workflows/{wf_ok}/layout"),
        Some(json!({ "layout": layout })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, body) = req(&env, "GET", &format!("/workflows/{wf_ok}/layout"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["layout"], layout);

    // --- 対話起動 → worker 完走 → 詳細/step/イベント/一覧。
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{wf_ok}/runs"),
        Some(json!({ "input": { "hello": 1 } })),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    let run_ok: Uuid = body["run_id"].as_str().unwrap().parse().unwrap();
    while worker.claim_and_run_once("w1").await.unwrap() {}

    let (st, body) = req(
        &env,
        "GET",
        &format!("/workflows/{wf_ok}/runs/{run_ok}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], json!("succeeded"));
    assert_eq!(body["input"], json!({ "hello": 1 }));
    assert!(body["trace_id"].is_string(), "OTel 相関 id が露出する");
    let steps = body["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert!(steps.iter().all(|s| s["has_output"] == json!(true)));

    let (st, body) = req(
        &env,
        "GET",
        &format!("/workflows/{wf_ok}/runs/{run_ok}/steps?path=a"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["output"], json!({ "from": "a" }));

    let (st, body) = req(
        &env,
        "GET",
        &format!("/workflows/{wf_ok}/runs/{run_ok}/events"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(body["items"].as_array().unwrap().len() >= 3);

    let (st, body) = req(
        &env,
        "GET",
        &format!("/workflows/{wf_ok}/runs?status=succeeded"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);

    // --- 存在秘匿: 別 workflow の run_id は 404。
    let (st, _) = req(
        &env,
        "GET",
        &format!("/workflows/{wf_fail}/runs/{run_ok}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // --- SSE（terminal run のリプレイ→run.terminal→close）。
    let resp = env
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/workflows/{wf_ok}/runs/{run_ok}/events/stream"))
                .header(COOKIE, env.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let sse =
        String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert!(sse.contains("event: run_event"), "{sse}");
    assert!(sse.contains("run.terminal"), "{sse}");

    // --- 失敗 run → retry(resume) → 再失敗 → retry(new)。
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{wf_fail}/runs"),
        Some(json!({ "input": {} })),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let run_fail: Uuid = body["run_id"].as_str().unwrap().parse().unwrap();
    while worker.claim_and_run_once("w1").await.unwrap() {}
    let (_, body) = req(
        &env,
        "GET",
        &format!("/workflows/{wf_fail}/runs/{run_fail}"),
        None,
    )
    .await;
    assert_eq!(body["status"], json!("failed"));

    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{wf_fail}/runs/{run_fail}/retry"),
        Some(json!({ "mode": "resume" })),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["mode"], json!("resume"));

    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{wf_fail}/runs/{run_fail}/retry"),
        Some(json!({ "mode": "new" })),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    assert!(body["run_id"].is_string(), "新規 run が作られる: {body}");

    // --- cancel（worker 未処理の run を即キャンセル）。
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{wf_ok}/runs"),
        Some(json!({ "input": {} })),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let run_cancel: Uuid = body["run_id"].as_str().unwrap().parse().unwrap();
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{wf_ok}/runs/{run_cancel}/cancel"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], json!("requested"));
    let (_, body) = req(
        &env,
        "GET",
        &format!("/workflows/{wf_ok}/runs/{run_cancel}"),
        None,
    )
    .await;
    assert_eq!(body["status"], json!("cancelled"));
    // 二重キャンセルは already_terminal。
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{wf_ok}/runs/{run_cancel}/cancel"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["outcome"], json!("already_terminal"));
}
