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
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let delegation = workflow_engine::DelegationStore::new(pool.clone(), Arc::clone(&authz));
    let launcher = Arc::new(workflow_engine::WorkflowRunLauncher::new(
        delegation.clone(),
        (*workflows).clone(),
        runs.clone(),
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
    let collab_storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        Arc::new(NoopStore),
        Arc::clone(&authz),
        Duration::from_secs(120),
        Duration::from_secs(900),
        1024,
    ));
    let collab_hub = Arc::new(collab::CollabHub::new(
        pool.clone(),
        Arc::clone(&authz),
        collab_storage,
    ));
    let tabular_svc = Arc::new(tabular::TabularService::new(
        Arc::new(storage::StorageService::new(
            pool.clone(),
            Arc::new(NoopStore),
            Arc::clone(&authz),
            Duration::from_secs(120),
            Duration::from_secs(900),
            1024,
        )),
        tabular::RunnerConfig::new("shiki-tabular-runner", Duration::from_secs(5)),
        tabular::Quotas::default(),
    ));
    let installs = Arc::new(app_platform::InstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        Arc::clone(&mini_app_code),
        Arc::clone(&data_store),
        Arc::new(AllowAll),
        None,
        vec![],
    ));
    let skill_installs = Arc::new(app_platform::SkillInstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        app_platform::TrustedKeyStore::new(pool.clone()),
        Arc::clone(&artifacts),
        Arc::new(AllowAll),
    ));
    let bundles = Arc::new(app_platform::BundleStore::new(
        Arc::new(NoopStore),
        Arc::new(AllowAll),
        storage::audit::AuditRecorder::new(pool.clone()),
    ));
    let app_usage = Arc::new(app_platform::AppUsageStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let state = AppState {
        config: Arc::new(config(&db_url)),
        insights: std::sync::Arc::new(api::insights::InsightsService::new(pool.clone())),
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
        collab: collab_hub,
        tabular: tabular_svc,
        artifacts: Arc::clone(&artifacts),
        ui_actions: Arc::new(gui::ActionDispatcher::new(
            storage::audit::AuditRecorder::new(pool.clone()),
        )),
        skills: Arc::new(gui::SkillStore::new(Arc::clone(&artifacts))),
        mini_apps: Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), pool.clone())),
        data: data_store,
        data_views,
        fsms,
        mini_app_code,
        installs,
        skill_installs,
        bundles,
        app_usage,
        secrets: None,
        workflows,
        workflow_launcher: Some(launcher),
        workflow_registration: Arc::new(workflow_engine::RegistrationService::new(
            pool.clone(),
            delegation,
        )),
        workflow_summaries: Arc::new(workflow_engine::WorkflowSummaryStore::new(pool.clone())),
        workflow_layout: Arc::new(workflow_engine::EditorLayoutStore::new(pool.clone())),
        workflow_approvals: Arc::new(workflow_engine::ApprovalStore::new(pool.clone())),
        audit: Arc::new(storage::audit::AuditRecorder::new(pool.clone())),
        workflow_runs: Some(Arc::new(runs)),
        directory: Arc::new(storage::DirectoryStore::new(pool.clone())),
        tenants: Arc::new(storage::TenantStore::new(pool.clone())),
        search: None,
        chat: None,
        llm: None,
        rag_admin: Arc::new(rag::RagAdmin::new(pool.clone(), None, None)),
        office: None,
        docx_composer: std::sync::Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
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
        RunStore::new(env.pool.clone(), workflow_engine::DEFAULT_LEASE_SECS),
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

/// data ノード・ストレージノードを置いたワークフローは、`declared_scopes` を書かなくても保存できる
/// （ノードが要するスコープを保存の入口で補完する・検証 C-P1-2）。保存・検証の応答に補完後のスコープが載り、
/// 保存した IR にも残る。実効スコープは「宣言 ∩ 委譲」のまま（有効化の委譲の検査は変わらない）。
#[tokio::test]
async fn saving_completes_declared_scopes_from_nodes() {
    let Some(env) = setup().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let name = format!("http-scopes-{}", &suffix[..12]);
    let ir = json!({
        "ir_version": 1,
        "name": name,
        "triggers": [{ "kind": "interactive" }],
        "nodes": [
            { "id": "a", "type": "storage.read",
              "params": { "file": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" } },
            { "id": "q", "type": "data.query",
              "params": { "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a2" } }
        ],
        "edges": [{ "from": "a", "to": "q" }]
    });
    let (st, body) = req(
        &env,
        "POST",
        "/workflows/validate",
        Some(json!({ "ir": ir })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["errors"], json!([]), "{body}");
    assert_eq!(
        body["declared_scopes"],
        json!(["data.read", "storage.read"])
    );
    let (st, body) = req(&env, "POST", "/workflows", Some(json!({ "ir": ir }))).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["declared_scopes"],
        json!(["data.read", "storage.read"])
    );
    let id = body["id"].as_str().unwrap().to_string();
    let (st, body) = req(&env, "GET", &format!("/workflows/{id}"), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["ir"]["declared_scopes"],
        json!(["data.read", "storage.read"])
    );
}

/// D-40・D-43: 必須の文字の欄が空の IR と、schedule で保存先フォルダを省略した IR は保存できる（警告つき）が、
/// 同意計画が理由を返し、有効化は 409 `not_ready`、対話の起動は 400 で止まる。直せば有効化できる。
#[tokio::test]
async fn blank_required_text_and_missing_folder_save_but_block_enable_and_run() {
    let Some(env) = setup().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let name = format!("http-ready-{}", &suffix[..12]);
    let mut ir = json!({
        "ir_version": 1,
        "name": name,
        "triggers": [
            { "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" },
            { "kind": "interactive" }
        ],
        "nodes": [
            { "id": "ask", "type": "llm.invoke", "params": { "prompt": "" } },
            { "id": "save", "type": "storage.write", "params": {
                "name": "memo.txt", "content": { "$from": "nodes.ask.output", "path": "/text" } } }
        ],
        "edges": [{ "from": "ask", "to": "save" }]
    });
    let (st, body) = req(&env, "POST", "/workflows", Some(json!({ "ir": ir }))).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();
    let warnings = body["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "ir.required_text_empty"
                && w["node_id"] == "ask"
                && w["path"] == "/params/prompt"),
        "{body}"
    );
    // 保存先の省略は保存時の警告にしない（対話の実行は本人のルートに書ける）。有効化の理由にだけ出す。
    assert!(
        warnings
            .iter()
            .all(|w| w["code"] != "ir.write_folder_required"),
        "{body}"
    );

    let (st, body) = req(
        &env,
        "GET",
        &format!("/workflows/{id}/versions/1/consent-plan"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let codes: Vec<(&str, &str)> = body["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["code"].as_str().unwrap(), b["node_id"].as_str().unwrap()))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("ir.required_text_empty", "ask"),
            ("ir.write_folder_required", "save")
        ],
        "{body}"
    );

    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{id}/enable"),
        Some(json!({ "version": 1, "grants": [] })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "not_ready", "{body}");
    assert_eq!(body["errors"].as_array().unwrap().len(), 2, "{body}");
    assert_eq!(body["errors"][1]["path"], "/params/folder", "{body}");
    // 有効化は記録されない。
    let (_, reg) = req(&env, "GET", &format!("/workflows/{id}/registration"), None).await;
    assert_eq!(reg["status"], "none", "{reg}");

    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{id}/runs"),
        Some(json!({ "input": {} })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "not_ready", "{body}");
    assert_eq!(body["errors"][0]["path"], "/params/prompt", "{body}");
    assert!(
        body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("AI への指示"),
        "{body}"
    );

    // 直した版（指示を書き、保存先を決める）は理由が消え、有効化できる。
    ir["nodes"][0]["params"]["prompt"] = json!("今日の予定をまとめてください");
    ir["nodes"][1]["params"]["folder"] = json!("8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a3");
    let (st, body) = req(
        &env,
        "PUT",
        &format!("/workflows/{id}"),
        Some(json!({ "ir": ir, "expected_version": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st, body) = req(
        &env,
        "GET",
        &format!("/workflows/{id}/versions/2/consent-plan"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["blockers"], json!([]), "{body}");
}

/// 検証 API は各ノードの出口（`node_ports`）を、保存時検証 V2 と同じ関数で返す（エディタは導出を書き写さない・
/// H15）。期待値は workflow-engine の V2 の試験と同じ fixture（port_cases.json）。保存できない IR でも返し、
/// 出口を導けないノード（params の誤り）は載せない。値で振り分ける判定から out へ張ったエッジは
/// ir.unknown_port になる（FEAT-1）。
#[tokio::test]
async fn validate_returns_node_ports_from_the_shared_fixture() {
    let Some(env) = setup().await else { return };
    let fixture: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../workflow-engine/tests/fixtures/port_cases.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    let nodes: Vec<Value> = cases.iter().map(|c| c["node"].clone()).collect();
    let ir = json!({
        "ir_version": 1, "name": "http-node-ports", "triggers": [{ "kind": "interactive" }],
        "nodes": nodes, "edges": [{ "from": "decide_value", "from_port": "out", "to": "read" }]
    });
    let (st, body) = req(
        &env,
        "POST",
        "/workflows/validate",
        Some(json!({ "ir": ir })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let ports = body["node_ports"].as_object().unwrap();
    for c in cases {
        let id = c["node"]["id"].as_str().unwrap();
        let want = &c["ports"];
        if want.is_null() {
            assert!(!ports.contains_key(id), "{}: {body}", c["name"]);
        } else {
            assert_eq!(&ports[id], want, "{}", c["name"]);
        }
    }
    assert_eq!(
        ports.len(),
        cases.iter().filter(|c| !c["ports"].is_null()).count()
    );
    // 保存できない IR（承認のテーブルが無い・段階で振り分ける判定・発火しない出口）でも出口は返る。
    let errors = body["errors"].as_array().unwrap();
    assert!(
        errors.iter().any(|e| e["code"] == "ir.unknown_port"
            && e["node_id"] == "decide_value"
            && e["path"] == "/edges/0/from_port"),
        "{body}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e["node_id"] == "decide_bad" && e["path"] == "/params/route/question"),
        "{body}"
    );
    // 読めない IR では空。
    let (st, body) = req(
        &env,
        "POST",
        "/workflows/validate",
        Some(json!({ "ir": { "nodes": "x" } })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["node_ports"], json!({}));
}

/// BUG-107: 別のワークフローを実行の相手が無い名前なら、保存と検証で欄の下の誤り（`ir.unknown_workflow`・
/// `/params/name`）にする。在る名前は通り、空の名前は保存だけ通して実行の前に止める（D-83）。
#[tokio::test]
async fn workflow_start_target_is_checked_when_saving() {
    let Some(env) = setup().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let child = format!("http-child-{}", &suffix[..12]);
    let (st, body) = req(
        &env,
        "POST",
        "/workflows",
        Some(json!({ "ir": simple_ir(&child, "b") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let child_id = body["id"].as_str().unwrap().to_string();
    let parent = |target: &str| {
        json!({
            "ir_version": 1,
            "name": format!("http-parent-{}", &suffix[..12]),
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "start", "type": "workflow.start", "params": { "name": target } }],
            "edges": []
        })
    };
    let missing = format!("http-missing-{}", &suffix[..12]);

    let (st, body) = req(
        &env,
        "POST",
        "/workflows/validate",
        Some(json!({ "ir": parent(&missing) })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let errors = body["errors"].as_array().unwrap();
    assert!(
        errors.iter().any(|e| e["code"] == "ir.unknown_workflow"
            && e["node_id"] == "start"
            && e["path"] == "/params/name"
            && e["message"]
                == "実行するワークフローが見つかりません。削除されたか、見る権限がありません。"),
        "{body}"
    );
    let (st, body) = req(
        &env,
        "POST",
        "/workflows",
        Some(json!({ "ir": parent(&missing) })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");

    let (st, body) = req(
        &env,
        "POST",
        "/workflows/validate",
        Some(json!({ "ir": parent(&child) })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["errors"], json!([]), "{body}");

    let (st, body) = req(
        &env,
        "POST",
        "/workflows/validate",
        Some(json!({ "ir": parent("") })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["errors"], json!([]), "{body}");
    assert!(
        body["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "ir.required_text_empty" && w["path"] == "/params/name"),
        "{body}"
    );

    // 片付け（ワークフローは artifact として消す）。
    let (st, _) = req(&env, "DELETE", &format!("/artifacts/{child_id}"), None).await;
    assert!(st.is_success(), "{st}");
}

/// BUG-102・D-90: 同意の画面（`consent: true`）は grants が空でも同意し直し、使ってよい操作が増えた版へ
/// 切り替える。無効化の本文 `revoke_delegations: true` は委譲もすべて取り消し、監査に残す。本文の無い無効化は
/// 今までどおり委譲を残す。一覧の委譲には対象の種類と id が付く（画面が名前を引く）。
#[tokio::test]
async fn consent_and_revoke_on_disable() {
    let Some(env) = setup().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let file = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";

    // --- 対象の無いワークフロー: 増えた操作を consent で受ける。
    let mut ir = json!({
        "ir_version": 1,
        "name": format!("consent-{}", &suffix[..12]),
        "declared_scopes": [],
        "triggers": [{ "kind": "schedule", "cron": "0 9 * * 1", "tz": "Asia/Tokyo" }],
        "nodes": [{ "id": "calc", "type": "script.run",
                    "params": { "source": { "inline": "function main(i){ return {}; }" } } }],
        "edges": []
    });
    let (st, body) = req(&env, "POST", "/workflows", Some(json!({ "ir": ir }))).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let bare = body["id"].as_str().unwrap().to_string();
    let enable = |version: i64, consent: Option<bool>| {
        let mut b = json!({ "version": version, "grants": [] });
        if let Some(c) = consent {
            b["consent"] = json!(c);
        }
        b
    };
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{bare}/enable"),
        Some(enable(1, Some(true))),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    ir["declared_scopes"] = json!(["http.egress"]);
    let (st, body) = req(
        &env,
        "PUT",
        &format!("/workflows/{bare}"),
        Some(json!({ "ir": ir, "expected_version": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    // consent を付けない（API を直接呼ぶ）なら今までどおり 409。
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{bare}/enable"),
        Some(enable(2, None)),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "scope_expansion", "{body}");
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{bare}/enable"),
        Some(enable(2, Some(true))),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (_, reg) = req(
        &env,
        "GET",
        &format!("/workflows/{bare}/registration"),
        None,
    )
    .await;
    assert_eq!(reg["enabled_version"], 2, "{reg}");
    assert_eq!(reg["consented_scopes"], json!(["http.egress"]), "{reg}");

    // --- 対象のあるワークフロー: 本文の無い無効化は委譲を残し、取り消しを付けると消える。
    let (st, body) = req(
        &env,
        "POST",
        "/workflows",
        Some(json!({ "ir": simple_ir(&format!("revoke-{}", &suffix[..12]), "b") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{id}/enable"),
        Some(json!({ "version": 1, "consent": true, "grants": [
            { "scope": "storage.read", "object_type": "file", "object_id": file, "relation": "viewer" }
        ] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (_, reg) = req(&env, "GET", &format!("/workflows/{id}/registration"), None).await;
    assert_eq!(reg["delegations"][0]["object_kind"], "file", "{reg}");
    assert_eq!(reg["delegations"][0]["object_id"], file, "{reg}");
    assert_eq!(
        reg["delegations"][0]["delegator"], "00000000-0000-0000-0000-000000000001",
        "{reg}"
    );

    let (st, body) = req(&env, "POST", &format!("/workflows/{id}/disable"), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["revoked_delegations"], 0, "{body}");
    let (_, reg) = req(&env, "GET", &format!("/workflows/{id}/registration"), None).await;
    assert_eq!(reg["status"], "disabled", "{reg}");
    assert_eq!(reg["delegations"].as_array().unwrap().len(), 1, "{reg}");

    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{id}/disable"),
        Some(json!({ "revoke_delegations": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["revoked_delegations"], 1, "{body}");
    let (_, reg) = req(&env, "GET", &format!("/workflows/{id}/registration"), None).await;
    assert_eq!(reg["delegations"], json!([]), "{reg}");
    assert_eq!(reg["consented_scopes"], json!([]), "{reg}");
    let audit: Vec<Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_log WHERE action = 'workflow.delegation.revoke' AND object_id = $1",
    )
    .bind(&id)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0]["revoked"], 1, "{audit:?}");
    assert_eq!(
        audit[0]["delegations"][0]["object_ref"],
        format!("file:default|{file}"),
        "{audit:?}"
    );

    // 片付け。
    for wf in [&bare, &id] {
        sqlx::query("DELETE FROM workflow_delegation WHERE tenant_id = 'default' AND workflow_id = $1::uuid")
            .bind(wf)
            .execute(&env.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM workflow_registration WHERE tenant_id = 'default' AND workflow_id = $1::uuid")
            .bind(wf)
            .execute(&env.pool)
            .await
            .unwrap();
        let (st, _) = req(&env, "DELETE", &format!("/artifacts/{wf}"), None).await;
        assert!(st.is_success(), "{st}");
    }
}

/// 全面の再同意で同意から外した委譲も、「権限も取り消す」と同じ `workflow.delegation.revoke` に残す（理由
/// `reconsent`・委譲元・scope・対象・relation）。再同意は (scope, 対象, relation) の組で外すので、ほかの委譲元の行も
/// 外れうる（BUG-130）。誰の、どの委譲が、誰の操作で外れたかを後から追えるようにする。
#[tokio::test]
async fn reconsent_records_the_delegations_it_revokes() {
    let Some(env) = setup().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let file = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
    let (st, body) = req(
        &env,
        "POST",
        "/workflows",
        Some(json!({ "ir": simple_ir(&format!("reconsent-{}", &suffix[..12]), "b") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();
    let enable = |grants: Value| json!({ "version": 1, "consent": true, "grants": grants });
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{id}/enable"),
        Some(enable(json!([
            { "scope": "storage.read", "object_type": "file", "object_id": file, "relation": "editor" }
        ]))),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    // 同じ対象の relation を狭めて同意し直す（editor → viewer）。
    let (st, body) = req(
        &env,
        "POST",
        &format!("/workflows/{id}/enable"),
        Some(enable(json!([
            { "scope": "storage.read", "object_type": "file", "object_id": file, "relation": "viewer" }
        ]))),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let audit = |pool: sqlx::PgPool, id: String| async move {
        sqlx::query_scalar::<_, Value>(
            "SELECT metadata FROM audit_log WHERE action = 'workflow.delegation.revoke' AND object_id = $1 \
             ORDER BY created_at",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let rows = audit(env.pool.clone(), id.clone()).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["reason"], "reconsent", "{rows:?}");
    assert_eq!(rows[0]["revoked"], 1, "{rows:?}");
    assert_eq!(
        rows[0]["delegations"][0],
        json!({
            "delegator": "00000000-0000-0000-0000-000000000001",
            "scope": "storage.read",
            "object_ref": format!("file:default|{file}"),
            "relation": "editor",
        }),
        "{rows:?}"
    );
    // 対象を外して同意し直すと、残っていた委譲も残す。外すものが無い同意し直しは行を足さない。
    for _ in 0..2 {
        let (st, body) = req(
            &env,
            "POST",
            &format!("/workflows/{id}/enable"),
            Some(enable(json!([]))),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
    }
    let rows = audit(env.pool.clone(), id.clone()).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1]["delegations"][0]["relation"], "viewer", "{rows:?}");

    // 片付け（監査の行は残す: 書き込みの監査はハッシュの鎖でつながっていて、消すと鎖が切れる）。
    for table in ["workflow_delegation", "workflow_registration"] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE tenant_id = 'default' AND workflow_id = $1::uuid"
        ))
        .bind(&id)
        .execute(&env.pool)
        .await
        .unwrap();
    }
    let (st, _) = req(&env, "DELETE", &format!("/artifacts/{id}"), None).await;
    assert!(st.is_success(), "{st}");
}

/// 金額（1 以上の整数・必須）を受ける手動のワークフローを作る（実行の入力の試験・BUG-109・D-93）。
async fn create_amount_flow(env: &Env, prefix: &str) -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    let ir = json!({
        "ir_version": 1,
        "name": format!("{prefix}-{}", &suffix[..12]),
        "declared_scopes": [],
        "input_schema": { "type": "object", "required": ["amount"], "properties": {
            "amount": { "type": "integer", "minimum": 1, "title": "金額" } } },
        "triggers": [{ "kind": "interactive" }],
        "nodes": [{ "id": "echo", "type": "script.run",
                    "params": { "source": { "inline": "function main(i){ return {}; }" } } }],
        "edges": []
    });
    let (st, body) = req(env, "POST", "/workflows", Some(json!({ "ir": ir }))).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

/// 試験で作った run とワークフローを消す。
async fn delete_flow_and_runs(env: &Env, id: &str) {
    sqlx::query("DELETE FROM workflow_run WHERE tenant_id = 'default' AND workflow_id = $1::uuid")
        .bind(id)
        .execute(&env.pool)
        .await
        .unwrap();
    let (st, _) = req(env, "DELETE", &format!("/artifacts/{id}"), None).await;
    assert!(st.is_success(), "{st}");
}

/// BUG-109: 実行の入力の誤りは本文を捨てず、項目のキーと理由を返す（code invalid_input）。run は作らない。
#[tokio::test]
async fn manual_run_reports_input_errors_by_field() {
    let Some(env) = setup().await else { return };
    let id = create_amount_flow(&env, "input-errors").await;
    let start = |body: Value| {
        let env = &env;
        let id = id.clone();
        async move { req(env, "POST", &format!("/workflows/{id}/runs"), Some(body)).await }
    };
    let (st, body) = start(json!({ "input": { "amount": 0 } })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_input", "{body}");
    assert_eq!(
        body["errors"],
        json!([{ "key": "amount", "message": "1 以上にしてください" }]),
        "{body}"
    );
    assert!(body["message"].as_str().unwrap().contains("誤り"), "{body}");
    let (st, body) = start(json!({ "input": { "amount": "3" } })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["errors"],
        json!([{ "key": "amount", "message": "整数を入れてください" }]),
        "{body}"
    );
    let runs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_run WHERE tenant_id = 'default' AND workflow_id = $1::uuid",
    )
    .bind(&id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(runs, 0, "入力の誤りでは run を作らない");
    delete_flow_and_runs(&env, &id).await;
}

/// きっかけの内容と入力を、ブロックの結果としてそのまま返す（`$from trigger` の源を確かめる・D-93）。
struct ContextEcho;

#[async_trait]
impl NodeExecutor for ContextEcho {
    async fn execute(&self, _t: &str, _p: &Value, ctx: &NodeContext) -> NodeResult {
        NodeResult::ok(json!({ "input": ctx.input, "trigger": ctx.trigger }))
    }
}

/// D-93: 実行のダイアログは、手動の実行にきっかけの内容を入力と別に渡せる（`$from trigger` の源）。渡さなければ
/// 今までどおり、きっかけの内容は入力と同じ。項目のまとまりでないきっかけの内容は 400（field = trigger）。
/// 実行履歴の「新しく実行し直す」は、入力と一緒に試したきっかけの内容も引き継ぐ。
#[tokio::test]
async fn manual_run_takes_trial_trigger() {
    let Some(env) = setup().await else { return };
    let worker = WorkflowWorker::new(
        RunStore::new(env.pool.clone(), workflow_engine::DEFAULT_LEASE_SECS),
        Arc::new(ContextEcho),
        WorkerConfig::default(),
    )
    .scoped_to_tenant("default");
    let id = create_amount_flow(&env, "trial-trigger").await;
    let start = |body: Value| {
        let env = &env;
        let id = id.clone();
        async move { req(env, "POST", &format!("/workflows/{id}/runs"), Some(body)).await }
    };

    // --- きっかけの内容は項目のまとまりだけ。
    let (st, body) = start(json!({ "input": { "amount": 3 }, "trigger": [1] })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_input", "{body}");
    assert_eq!(body["field"], "trigger", "{body}");

    // --- 試すきっかけの内容は `$from trigger` の源になり、入力はそのまま。
    let trial = json!({ "record_id": "r-1", "to": "approved" });
    let (st, body) = start(json!({ "input": { "amount": 3 }, "trigger": trial })).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    let with_trial = body["run_id"].as_str().unwrap().to_string();
    // --- 渡さなければ、きっかけの内容は入力と同じ（今までどおり）。
    let (st, body) = start(json!({ "input": { "amount": 4 } })).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    let without = body["run_id"].as_str().unwrap().to_string();
    while worker.claim_and_run_once("w-trial").await.unwrap() {}
    let output = |run: String| {
        let env = &env;
        let id = id.clone();
        async move {
            let (st, body) = req(
                env,
                "GET",
                &format!("/workflows/{id}/runs/{run}/steps?path=echo"),
                None,
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{body}");
            body["output"].clone()
        }
    };
    assert_eq!(
        output(with_trial.clone()).await,
        json!({ "input": { "amount": 3 }, "trigger": trial })
    );
    assert_eq!(
        output(without.clone()).await,
        json!({ "input": { "amount": 4 }, "trigger": { "amount": 4 } })
    );

    // --- 「新しく実行し直す」は、元の run で試したきっかけの内容も引き継ぐ（入力だけを引き継ぐと、新しい run の
    //     きっかけの内容が入力の値になっていた）。試していない run は、いままでどおり入力と同じ。
    let retry_new = |run: String| {
        let env = &env;
        let id = id.clone();
        async move {
            let (st, body) = req(
                env,
                "POST",
                &format!("/workflows/{id}/runs/{run}/retry"),
                Some(json!({ "mode": "new" })),
            )
            .await;
            assert_eq!(st, StatusCode::ACCEPTED, "{body}");
            body["run_id"].as_str().unwrap().to_string()
        }
    };
    let again_trial = retry_new(with_trial.clone()).await;
    let again_without = retry_new(without).await;
    assert_ne!(again_trial, with_trial, "新しい run を作る");
    while worker.claim_and_run_once("w-trial").await.unwrap() {}
    assert_eq!(
        output(again_trial).await,
        json!({ "input": { "amount": 3 }, "trigger": trial })
    );
    assert_eq!(
        output(again_without).await,
        json!({ "input": { "amount": 4 }, "trigger": { "amount": 4 } })
    );
    delete_flow_and_runs(&env, &id).await;
}
