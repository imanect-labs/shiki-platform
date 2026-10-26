//! 承認待ち（human.approval・GAP-08）の結合テスト（実 Postgres ＋実 OpenFGA ＋ S3・HTTP ルータ経由）。
//!
//! 本番の relay（`api::workflow_runtime::relay_events`）・launcher・executor（`ProdNodePorts` の data 結線）で
//! 申請 → ワークフローの受付（遷移）→ 承認待ち → 受信箱（`/approvals`）→ 判断 → run の前進までを通す。
//!
//! - 直前の自分の遷移（停止時の rev 以下）では承認が起きない
//! - 受信箱・件数は担当者（本人・所属ロール）宛てで、承認者本人に見える行だけ（見えない行を数えない）。
//!   読めない列は出さない。担当でない人には 404
//! - 判断は承認者本人の FSM 遷移（担当者でも状態の遷移の規則で許されなければ 403）。コメント必須・rev 不一致
//! - 判断の直後に再取得しても出ない。判断の遷移で run が approved へ進み、コメントが出力に載る
//! - テーブル画面からの遷移（`FsmStore::transition`）でも同じ承認が決まる
//! - 期限を過ぎた承認は判断できず（409）、回収で timeout の出口へ進む
//!
//! relay はテナントを問わず未配送のイベントを掴むので、このファイルのテストは直列に流し、利用者 id は
//! テストごとに一意にする（テナントは single 構成の既定 `default`）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines
)]

use std::sync::Arc;
use std::time::Duration;

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionStore},
    state::AppState,
};
use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use axum::body::Body;
use axum::http::{header::COOKIE, Request, StatusCode};
use data::{
    CmpOp, DataStore, FieldDef, FieldPolicy, FieldType, FsmBody, FsmRef, FsmStore, FsmTransition,
    NewDataTable, PolicyExpr, PolicyOperand, RefResolver, RowPolicy, TableSchema,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;
use workflow_engine::{
    CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, GrantRequest, NodeExecutor,
    RegistrationService, RunStore, SchedulerStore, WorkerConfig, WorkflowRunLauncher,
    WorkflowStore,
};

const TENANT: &str = "default";

/// このファイルのテストを直列にする（上のモジュールの説明）。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 実在するユーザー（テストごとの接尾辞つき）。
struct Users;

#[async_trait]
impl RefResolver for Users {
    async fn user_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(["alice-", "bob-", "carol-", "dave-"]
            .iter()
            .any(|p| id.starts_with(p)))
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

struct NoopAudit;
impl workflow_engine::CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
}

/// no-op ObjectStore（BundleStore の AppState 充足用）。
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
            tenant_id: Some(TENANT.into()),
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
            backend: LlmBackend::Stub,
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

fn ctx(id: &str) -> AuthContext {
    AuthContext::new(principal(id), "acme".into(), TENANT.into())
}

struct Env {
    app: axum::Router,
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    storage: Arc<storage::StorageService>,
    data: Arc<DataStore>,
    fsms: Arc<FsmStore>,
    executor: Arc<dyn NodeExecutor>,
    workflows: WorkflowStore,
    runs: RunStore,
    launcher: WorkflowRunLauncher,
    registration: RegistrationService,
    /// 申請者・委譲元（テーブルの owner）。
    alice: AuthContext,
    /// 上長（承認者の列）。
    bob: AuthContext,
    /// 経理（ロールの担当者・メモを読める・状態の遷移の規則では承認できない）。
    carol: AuthContext,
    /// 別の申請の承認者。
    dave: AuthContext,
    /// 経理のロール id。
    keiri: String,
}

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let http = reqwest::Client::new();
    let fga: Arc<dyn AuthzClient> = Arc::new(
        OpenFgaClient::connect(
            http.clone(),
            &OpenFgaConfig {
                base_url: fga_url,
                store_name: format!("shiki-wf-approval-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    );
    let s3 = storage::S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let object_store: Arc<dyn storage::ObjectStore> = Arc::new(storage::S3ObjectStore::new(&s3));
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        object_store,
        Arc::clone(&fga),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    let gateway = Arc::new(
        llm_gateway::LlmGateway::build(
            pool.clone(),
            http,
            llm_gateway::GatewayConfig {
                provider: llm_gateway::ProviderConfig {
                    kind: llm_gateway::ProviderKind::Stub,
                    ..Default::default()
                },
                catalog: llm_gateway::ModelCatalog {
                    default_model: "stub".into(),
                    models: vec![llm_gateway::ModelEntry {
                        id: "stub".into(),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )
        .expect("gateway"),
    );
    let data = Arc::new(DataStore::new(
        pool.clone(),
        Arc::clone(&fga),
        Arc::new(Users),
    ));
    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let fsms = Arc::new(FsmStore::new(Arc::clone(&artifacts), (*data).clone()));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let delegation = DelegationStore::new(pool.clone(), Arc::clone(&fga));
    let launcher = WorkflowRunLauncher::new(delegation.clone(), workflows.clone(), runs.clone());
    let registration = RegistrationService::new(pool.clone(), delegation);
    let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
        storage: Arc::clone(&storage),
        search: None,
        gateway,
        sandbox: None,
        sandbox_backend: sandbox_client::SandboxBackend::Wasm,
        secrets: None,
        tabular: None,
        skill_artifacts: None,
        launcher: launcher.clone(),
        http: reqwest::Client::new(),
        db: pool.clone(),
        data: Some(Arc::clone(&data)),
        fsms: Some(Arc::clone(&fsms)),
        web_search: None,
    });
    let executor: Arc<dyn NodeExecutor> = Arc::new(CapabilityNodeExecutor::new(
        ports,
        EffectJournal::new(pool.clone()),
        Arc::new(NoopAudit),
    ));
    let sfx = Uuid::new_v4().simple().to_string();
    let ids = ["alice", "bob", "carol", "dave"].map(|u| format!("{u}-{sfx}"));
    let sessions = Arc::new(MemorySessionStore::new());
    for u in &ids {
        sessions
            .put(
                TENANT,
                &format!("sid-{u}"),
                &api::session::SessionRecord {
                    principal: principal(u),
                    tenant_id: TENANT.into(),
                    access_token: "a".into(),
                    refresh_token: None,
                    id_token: None,
                    access_expires_at: chrono::Utc::now().timestamp() + 3600,
                    csrf_token: format!("csrf-{u}"),
                    keycloak_sid: None,
                },
                Duration::from_secs(3600),
            )
            .await
            .unwrap();
    }
    let app = build_app(
        &pool,
        config(&db_url),
        Arc::clone(&storage),
        sessions,
        &fga,
        Arc::clone(&data),
        Arc::clone(&artifacts),
    );
    let alice = ctx(&ids[0]);
    // alice はドライブのルートにフォルダを作れる（組織の member）。
    fga.write_tuple(
        &alice.subject(),
        Relation::Member,
        &alice.ns().organization("acme"),
    )
    .await
    .unwrap();
    Some(Env {
        app,
        pool,
        fga,
        storage,
        data,
        fsms,
        executor,
        workflows,
        runs,
        launcher,
        registration,
        alice,
        bob: ctx(&ids[1]),
        carol: ctx(&ids[2]),
        dave: ctx(&ids[3]),
        keiri: format!("keiri-{sfx}"),
    })
}

fn build_app(
    pool: &sqlx::PgPool,
    config: AppConfig,
    storage: Arc<storage::StorageService>,
    sessions: Arc<MemorySessionStore>,
    authz: &Arc<dyn AuthzClient>,
    data_store: Arc<DataStore>,
    artifacts: Arc<artifact::ArtifactStore>,
) -> axum::Router {
    let tabular = Arc::new(tabular::TabularService::new(
        Arc::clone(&storage),
        tabular::RunnerConfig::new("/nonexistent/shiki-tabular-runner", Duration::from_secs(5)),
        tabular::Quotas::default(),
    ));
    let collab = Arc::new(collab::CollabHub::new(
        pool.clone(),
        Arc::clone(authz),
        Arc::clone(&storage),
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
    let skill_installs = Arc::new(app_platform::SkillInstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        app_platform::TrustedKeyStore::new(pool.clone()),
        Arc::clone(&artifacts),
        Arc::clone(authz),
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
    let directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    let state = AppState {
        config: Arc::new(config),
        insights: Arc::new(api::insights::InsightsService::new(pool.clone())),
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
        fsms: Arc::new(FsmStore::new(Arc::clone(&artifacts), (*data_store).clone())),
        mini_app_code: Arc::clone(&mini_app_code),
        installs,
        skill_installs,
        bundles,
        app_usage,
        ui_actions: Arc::new(gui::ActionDispatcher::new(
            storage::audit::AuditRecorder::new(pool.clone()),
        )),
        skills: Arc::new(gui::SkillStore::new(Arc::clone(&artifacts))),
        mini_apps: Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), pool.clone())),
        secrets: None,
        workflows: Arc::new(WorkflowStore::new(Arc::clone(&artifacts))),
        workflow_launcher: None,
        workflow_runs: None,
        workflow_registration: Arc::new(RegistrationService::new(
            pool.clone(),
            DelegationStore::new(pool.clone(), Arc::clone(authz)),
        )),
        workflow_summaries: Arc::new(workflow_engine::WorkflowSummaryStore::new(pool.clone())),
        workflow_layout: Arc::new(workflow_engine::EditorLayoutStore::new(pool.clone())),
        workflow_approvals: Arc::new(workflow_engine::ApprovalStore::new(pool.clone())),
        audit: Arc::new(storage::audit::AuditRecorder::new(pool.clone())),
        directory,
        tenants: Arc::new(storage::TenantStore::new(pool.clone())),
        search: None,
        chat: None,
        llm: None,
        rag_admin: Arc::new(rag::RagAdmin::new(pool.clone(), None, None)),
        office: None,
        docx_composer: Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
    };
    build_router(state)
}

fn req(method: &str, user: &AuthContext, path: &str, body: Body) -> Request<Body> {
    let u = &user.principal.id;
    Request::builder()
        .method(method)
        .uri(path)
        .header(
            COOKIE,
            format!("shiki_session=sid-{u}.{TENANT}; shiki_csrf=csrf-{u}"),
        )
        .header("x-csrf-token", format!("csrf-{u}"))
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

/// 申請の表。行は申請者（owner）・承認者・経理が読める。メモは経理だけが読める。
fn expense_schema(fsm: Option<FsmRef>, keiri: &str) -> TableSchema {
    let mut approver = FieldDef::new("approver", FieldType::UserRef);
    approver.indexed = true;
    let mut status = FieldDef::new("status", FieldType::Select);
    status.options = ["draft", "submitted", "reviewing", "approved", "rejected"]
        .map(String::from)
        .to_vec();
    status.indexed = true;
    let mut title = FieldDef::new("title", FieldType::Text);
    title.display_name = "件名".into();
    TableSchema {
        fields: vec![
            title,
            FieldDef::new("amount", FieldType::Number),
            FieldDef::new("memo", FieldType::Text),
            approver,
            status,
        ],
        status_field: Some("status".into()),
        row_policy: Some(RowPolicy {
            read: PolicyExpr::Any(vec![
                PolicyExpr::IsOwner,
                PolicyExpr::FieldCmp {
                    field: "approver".into(),
                    op: CmpOp::Eq,
                    value: PolicyOperand::UserId,
                },
                PolicyExpr::HasRole {
                    role: keiri.into(),
                    subtree: true,
                },
            ]),
            write: None,
        }),
        field_policy: vec![FieldPolicy {
            field: "memo".into(),
            readable_by: PolicyExpr::HasRole {
                role: keiri.into(),
                subtree: true,
            },
        }],
        aggregate_min_rows: None,
        fsm_ref: fsm,
        rag: None,
    }
}

impl Env {
    /// alice の申請の表（下書き → 申請 → 受付 は作成者、受付 → 承認・却下 は承認者の列の人）。
    /// bob・carol・dave はテーブルの editor。carol は経理のロール。
    async fn expense_table(&self) -> Uuid {
        let table = self
            .data
            .create_table(
                &self.alice,
                NewDataTable {
                    name: format!("expense-{}", Uuid::new_v4().simple()),
                    schema: expense_schema(None, &self.keiri),
                },
                None,
            )
            .await
            .unwrap();
        let approver_only = PolicyExpr::FieldCmp {
            field: "approver".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        };
        let t = |from: &str, to: &str, actor: PolicyExpr| FsmTransition {
            from: from.into(),
            to: to.into(),
            actor,
        };
        let fsm = FsmBody {
            states: ["draft", "submitted", "reviewing", "approved", "rejected"]
                .map(String::from)
                .to_vec(),
            transitions: vec![
                t("draft", "submitted", PolicyExpr::IsOwner),
                t("submitted", "reviewing", PolicyExpr::IsOwner),
                t("reviewing", "approved", approver_only.clone()),
                t("reviewing", "rejected", approver_only),
            ],
        };
        let fsm_id = self
            .fsms
            .create(
                &self.alice,
                &format!("flow-{}", table.id.simple()),
                table.id,
                &fsm,
                None,
            )
            .await
            .unwrap();
        self.data
            .update_table_schema(
                &self.alice,
                table.id,
                expense_schema(
                    Some(FsmRef {
                        artifact_id: fsm_id,
                        version: 1,
                    }),
                    &self.keiri,
                ),
                Some(1),
                None,
            )
            .await
            .unwrap();
        let object = self.alice.ns().data_table(&table.id.to_string());
        for u in [&self.bob, &self.carol, &self.dave] {
            self.fga
                .write_tuple(&u.subject(), Relation::Editor, &object)
                .await
                .unwrap();
        }
        self.fga
            .write_tuple(
                &self.carol.subject(),
                Relation::Member,
                &self.carol.ns().role(&self.keiri),
            )
            .await
            .unwrap();
        table.id
    }

    /// 申請を受け付けて承認を待つワークフロー（alice が読取・書込を委譲して有効化）。
    async fn enable(&self, table: Uuid, timeout: Option<u32>) -> Uuid {
        let t = table.to_string();
        let mut approval = json!({
            "title": "経費申請の承認", "instructions": "規程どおりか確認してください。",
            "record": { "table": t, "id": { "$from": "trigger", "path": "/record_id" } },
            "assignees": [{ "field": "approver" }, { "role": self.keiri }],
            "approve": { "to": "approved" },
            "reject": { "to": "rejected", "label": "却下する", "comment": "required" },
            "show_fields": ["title", "amount", "memo"]
        });
        let mut edges = vec![
            json!({ "from": "accept", "to": "ok" }),
            json!({ "from": "ok", "from_port": "approved", "to": "done" }),
            json!({ "from": "ok", "from_port": "rejected", "to": "ng" }),
        ];
        let mut nodes = vec![
            json!({ "id": "accept", "type": "data.transition",
                    "params": { "table": t, "record": { "$from": "trigger", "path": "/record_id" }, "to": "reviewing" } }),
            json!({ "id": "done", "type": "control.join", "params": {} }),
            json!({ "id": "ng", "type": "control.join", "params": {} }),
        ];
        if let Some(s) = timeout {
            approval["timeout_sec"] = json!(s);
            approval["on_timeout"] = json!("continue");
            edges.push(json!({ "from": "ok", "from_port": "timeout", "to": "late" }));
            nodes.push(json!({ "id": "late", "type": "control.join", "params": {} }));
        }
        nodes.push(json!({ "id": "ok", "type": "human.approval", "params": approval }));
        let ir = json!({
            "ir_version": 1, "name": format!("expense-{}", Uuid::new_v4().simple()),
            "triggers": [{ "kind": "event", "source": "data.transition", "scope": { "table": t },
                "filter": { "cmp": { "left": { "$from": "trigger", "path": "/to" }, "op": "eq", "right": "submitted" } } }],
            "nodes": nodes,
            "edges": edges,
            "policies": { "run_timeout_sec": 1_209_600 }
        });
        let (wf, parsed) = self
            .workflows
            .create(&self.alice, &ir, &Catalog::default(), None)
            .await
            .expect("保存");
        assert!(parsed.declared_scopes.iter().any(|s| s == "notify.send"));
        let object = self.alice.ns().data_table(&t);
        let grants = vec![
            GrantRequest {
                scope: "data.read".into(),
                object: object.clone(),
                relation: Relation::Viewer,
            },
            GrantRequest {
                scope: "data.write".into(),
                object,
                relation: Relation::Editor,
            },
        ];
        self.registration
            .enable(&self.alice, wf, 1, &parsed, &grants)
            .await
            .expect("有効化");
        wf
    }

    /// 申請する（下書きを作って申請へ遷移する・alice）。
    async fn submit(&self, table: Uuid, title: &str, approver: &AuthContext) -> Uuid {
        let r = self
            .data
            .create_record(
                &self.alice,
                table,
                json!({ "title": title, "amount": 12000, "approver": approver.principal.id, "status": "draft" }),
                None,
            )
            .await
            .unwrap();
        // メモは経理（carol）だけが読み書きできる。
        let r = self
            .data
            .update_record(
                &self.carol,
                table,
                r.id,
                json!({ "memo": "社外秘" }),
                r.rev,
                None,
            )
            .await
            .unwrap();
        self.fsms
            .transition(&self.alice, table, r.id, "submitted", r.rev, None)
            .await
            .unwrap();
        r.id
    }

    async fn relay_once(&self) {
        let sched = SchedulerStore::new(self.pool.clone());
        api::workflow_runtime::relay_events(&api::workflow_runtime::RelayDeps {
            db: &self.pool,
            hints: &storage::event::ClaimHints::default(),
            sched: &sched,
            runs: &self.runs,
            launcher: &self.launcher,
            storage: &self.storage,
            data: &self.data,
            fsms: &self.fsms,
        })
        .await
        .expect("relay");
        let worker = workflow_engine::WorkflowWorker::new(
            self.runs.clone(),
            Arc::clone(&self.executor),
            WorkerConfig::default(),
        )
        .scoped_to_tenant(TENANT);
        while worker.claim_and_run_once("w1").await.unwrap() {}
    }

    /// データ変更がすべて workflow へ配送され、run が進み切るまで relay とワーカーを回す（2 回続けて静か）。
    async fn settle(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut quiet = 0;
        loop {
            self.relay_once().await;
            let pending: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM storage_event_outbox o \
                   WHERE o.tenant_id = $1 AND o.payload ? 'event_type' \
                     AND NOT EXISTS (SELECT 1 FROM outbox_delivery d \
                                     WHERE d.event_id = o.id AND d.consumer = 'workflow')) \
                 OR EXISTS (SELECT 1 FROM step_execution \
                            WHERE tenant_id = $1 AND status IN ('ready', 'running'))",
            )
            .bind(TENANT)
            .fetch_one(&self.pool)
            .await
            .unwrap();
            quiet = if pending { 0 } else { quiet + 1 };
            if quiet >= 2 {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "配送が終わらない");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn run_of(&self, wf: Uuid, record: Uuid) -> Uuid {
        sqlx::query_scalar(
            "SELECT run_id FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2 \
               AND input->>'record_id' = $3",
        )
        .bind(TENANT)
        .bind(wf)
        .bind(record.to_string())
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn step(&self, run: Uuid, node: &str) -> (String, Vec<String>, Option<Value>) {
        let (s, p, o): (String, Vec<String>, Option<sqlx::types::Json<Value>>) = sqlx::query_as(
            "SELECT status, taken_ports, output FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3",
        )
        .bind(TENANT)
        .bind(run)
        .bind(node)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (s, p, o.map(|j| j.0))
    }

    async fn send(&self, r: Request<Body>) -> (StatusCode, Value) {
        let resp = self.app.clone().oneshot(r).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, user: &AuthContext, path: &str) -> (StatusCode, Value) {
        self.send(req("GET", user, path, Body::empty())).await
    }

    async fn decide(&self, user: &AuthContext, id: &str, body: Value) -> (StatusCode, Value) {
        self.send(req(
            "POST",
            user,
            &format!("/approvals/{id}/decision"),
            Body::from(body.to_string()),
        ))
        .await
    }

    /// その申請の承認（受信箱の 1 件）。
    async fn item_for(&self, user: &AuthContext, record: Uuid) -> Option<Value> {
        let (st, body) = self.get(user, "/approvals").await;
        assert_eq!(st, StatusCode::OK, "{body}");
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["record"]["id"] == json!(record))
            .cloned()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approvals_are_decided_by_the_approver_in_the_inbox() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, None).await;
    let mine = env.submit(table, "出張旅費", &env.bob).await;
    let others = env.submit(table, "備品の購入", &env.dave).await;
    env.settle().await;

    // ワークフローは受付（自分の遷移）をしてから承認を待つ。直前の自分の遷移では承認が起きない。
    let run = env.run_of(wf, mine).await;
    assert_eq!(env.step(run, "accept").await.0, "succeeded");
    assert_eq!(env.step(run, "ok").await.0, "waiting_event");
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'workflow.approval.request' \
           AND metadata->>'run_id' = $2",
    )
    .bind(TENANT)
    .bind(run.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(audits, 1, "受付の突合で承認の依頼を監査に残す");

    // 受信箱: bob には自分が承認者の申請だけ（dave の申請は bob に見えないので出ない・数えない）。
    let item = env
        .item_for(&env.bob, mine)
        .await
        .expect("bob の受信箱に出る");
    assert!(env.item_for(&env.bob, others).await.is_none());
    let (_, count) = env.get(&env.bob, "/approvals/count").await;
    assert_eq!(count, json!({ "count": 1, "more": false }));
    assert_eq!(item["title"], "経費申請の承認");
    assert_eq!(item["from_state"], "reviewing");
    let keys: Vec<&str> = item["record"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["title", "amount"], "bob に読めないメモは出さない");
    assert_eq!(item["record"]["fields"][0]["display_name"], "件名");
    assert_eq!(item["decisions"][0]["to"], "approved");
    assert_eq!(item["decisions"][1]["comment_required"], true);
    let id = item["approval_id"].as_str().unwrap().to_string();
    let rev = item["record"]["rev"].as_i64().unwrap();
    // dave には dave の申請だけ。carol（経理のロール）には両方（メモも読める）。
    assert!(env.item_for(&env.dave, others).await.is_some());
    assert!(env.item_for(&env.dave, mine).await.is_none());
    let carols = env
        .item_for(&env.carol, mine)
        .await
        .expect("ロールの担当者");
    assert_eq!(carols["record"]["fields"].as_array().unwrap().len(), 3);
    // 担当でない alice（申請者）には出ず、1 件の取得も判断も 404。
    assert!(env.item_for(&env.alice, mine).await.is_none());
    assert_eq!(
        env.get(&env.alice, &format!("/approvals/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
    let (st, _) = env
        .decide(
            &env.alice,
            &id,
            json!({ "decision": "approve", "expected_rev": rev }),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 1 件の取得は、本人がいま判断できるかを添える（carol は担当だが状態の遷移の規則で承認できない）。
    let (_, detail) = env.get(&env.bob, &format!("/approvals/{id}")).await;
    assert_eq!(detail["decisions"][0]["allowed"], true);
    let (_, detail) = env.get(&env.carol, &format!("/approvals/{id}")).await;
    assert_eq!(detail["decisions"][0]["allowed"], false);
    let (st, body) = env
        .decide(
            &env.carol,
            &id,
            json!({ "decision": "approve", "expected_rev": rev }),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "not_allowed");

    // 却下はコメント必須。古い rev は 409。
    let (st, body) = env
        .decide(
            &env.bob,
            &id,
            json!({ "decision": "reject", "expected_rev": rev }),
        )
        .await;
    assert_eq!(
        (st, body["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("comment_required"))
    );
    let (st, body) = env
        .decide(
            &env.bob,
            &id,
            json!({ "decision": "approve", "comment": "了解", "expected_rev": rev - 1 }),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "rev_conflict");

    // bob が受信箱から承認する。判断の直後に再取得しても出ない（relay を待たない）。
    let (st, body) = env
        .decide(
            &env.bob,
            &id,
            json!({ "decision": "approve", "comment": "規程どおりです", "expected_rev": rev }),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["rev"], rev + 1);
    assert!(env.item_for(&env.bob, mine).await.is_none());
    assert_eq!(env.get(&env.bob, "/approvals/count").await.1["count"], 0);
    // 二度目の判断はもう遷移できない。
    let (st, _) = env
        .decide(
            &env.bob,
            &id,
            json!({ "decision": "approve", "expected_rev": rev + 1 }),
        )
        .await;
    assert!(
        st == StatusCode::UNPROCESSABLE_ENTITY
            || st == StatusCode::NOT_FOUND
            || st == StatusCode::CONFLICT,
        "{st}"
    );

    env.settle().await;
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["approved".to_string()])
    );
    let out = out.unwrap();
    assert_eq!(out["decision"], "approve");
    assert_eq!(out["decided_by"], json!(env.bob.principal.id));
    assert_eq!(out["comment"], "規程どおりです");
    assert_eq!(env.step(run, "done").await.0, "succeeded");
    assert_eq!(env.step(run, "ng").await.0, "skipped");
    let (st, _) = env
        .decide(
            &env.bob,
            &id,
            json!({ "decision": "approve", "expected_rev": rev + 1 }),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "決まった承認は 409");

    // テーブル画面からの遷移（dave が状態のメニューで承認）でも同じ承認が決まる。
    let row = env
        .data
        .get_record(&env.dave, table, others, None)
        .await
        .unwrap();
    env.fsms
        .transition(&env.dave, table, others, "approved", row.rev, None)
        .await
        .unwrap();
    env.settle().await;
    let other_run = env.run_of(wf, others).await;
    let (st, ports, out) = env.step(other_run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["approved".to_string()])
    );
    assert_eq!(out.unwrap()["decided_by"], json!(env.dave.principal.id));
    assert!(env.item_for(&env.carol, others).await.is_none());
    let resolved: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'workflow.approval.resolve' \
           AND metadata->>'run_id' = ANY($2)",
    )
    .bind(TENANT)
    .bind(vec![run.to_string(), other_run.to_string()])
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(resolved, 2, "決着を監査に残す");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_approvals_cannot_be_decided_and_take_the_timeout_port() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, Some(1)).await;
    let record = env.submit(table, "会議費", &env.bob).await;
    env.settle().await;
    let run = env.run_of(wf, record).await;
    assert_eq!(env.step(run, "ok").await.0, "waiting_event");
    let id: Uuid = sqlx::query_scalar(
        "SELECT approval_id FROM workflow_approval WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(run)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // 期限を過ぎた承認は受信箱に出ず、回収を待たずに判断を 409 で拒む。
    assert!(env.item_for(&env.bob, record).await.is_none());
    let row = env
        .data
        .get_record(&env.bob, table, record, None)
        .await
        .unwrap();
    let (st, body) = env
        .decide(
            &env.bob,
            &id.to_string(),
            json!({ "decision": "approve", "expected_rev": row.rev }),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "expired");

    // 回収で timeout の出口へ進み、台帳は expired。
    let done = env
        .runs
        .expire_due_approvals(chrono::Utc::now(), Some(TENANT))
        .await
        .unwrap();
    assert!(done.iter().any(|r| r.run_id == run));
    env.settle().await;
    let (st, ports, _) = env.step(run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["timeout".to_string()])
    );
    assert_eq!(env.step(run, "late").await.0, "succeeded");
    let status: String = sqlx::query_scalar(
        "SELECT status FROM workflow_approval WHERE tenant_id = $1 AND approval_id = $2",
    )
    .bind(TENANT)
    .bind(id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(status, "expired");
}

/// 停止の commit より前に起きた遷移（relay が購読を見つける前に配送済みにした変更）は、受付の突合で
/// いまの行から決める（判断の行き先なら出口へ・判断した人はその遷移の主体）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconcile_settles_transitions_that_raced_the_suspend() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, None).await;
    let record = env.submit(table, "研修費", &env.bob).await;
    env.settle().await;
    let run = env.run_of(wf, record).await;
    assert_eq!(env.step(run, "ok").await.0, "waiting_event");

    // 突合の前に bob が遷移し、その変更は購読より前に配送済みになった（取りこぼした）ことにする。
    sqlx::query(
        "UPDATE workflow_approval SET reconciled_at = NULL WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(run)
    .execute(&env.pool)
    .await
    .unwrap();
    let row = env
        .data
        .get_record(&env.bob, table, record, None)
        .await
        .unwrap();
    env.fsms
        .transition(&env.bob, table, record, "approved", row.rev, None)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id) \
         SELECT 'workflow', o.id, o.tenant_id FROM storage_event_outbox o \
         WHERE o.tenant_id = $1 AND NOT EXISTS (SELECT 1 FROM outbox_delivery d \
               WHERE d.consumer = 'workflow' AND d.event_id = o.id)",
    )
    .bind(TENANT)
    .execute(&env.pool)
    .await
    .unwrap();
    env.settle().await;
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["approved".to_string()])
    );
    let out = out.unwrap();
    assert_eq!(out["decided_by"], json!(env.bob.principal.id));
    assert_eq!(out["rev"], row.rev + 1);
    assert_eq!(env.step(run, "done").await.0, "succeeded");
}
