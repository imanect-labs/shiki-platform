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
        self.expense_table_with(&[]).await
    }

    /// [`Self::expense_table`] に、承認者の列の人が進められる遷移（`extra`）を足した表。
    async fn expense_table_with(&self, extra: &[(&str, &str)]) -> Uuid {
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
        let mut transitions = vec![
            t("draft", "submitted", PolicyExpr::IsOwner),
            t("submitted", "reviewing", PolicyExpr::IsOwner),
            t("reviewing", "approved", approver_only.clone()),
            t("reviewing", "rejected", approver_only.clone()),
        ];
        for (from, to) in extra {
            transitions.push(t(from, to, approver_only.clone()));
        }
        let fsm = FsmBody {
            states: ["draft", "submitted", "reviewing", "approved", "rejected"]
                .map(String::from)
                .to_vec(),
            transitions,
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
    // 二度目の判断（relay が起こす前・行はもう承認の状態）は、行が停止時の状態でないので 409。
    let (st, body) = env
        .decide(
            &env.bob,
            &id,
            json!({ "decision": "approve", "expected_rev": rev + 1 }),
        )
        .await;
    assert_eq!(
        (st, body["code"].clone()),
        (StatusCode::CONFLICT, json!("decided")),
        "{body}"
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

impl Env {
    /// run の承認（台帳の id と状態）。
    async fn approval_of(&self, run: Uuid) -> (Uuid, String) {
        sqlx::query_as(
            "SELECT approval_id, status FROM workflow_approval WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(TENANT)
        .bind(run)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 承認の決着の監査（status と reason）。無ければ `None`。
    async fn resolve_audit(&self, approval: Uuid) -> Option<Value> {
        sqlx::query_scalar(
            "SELECT metadata FROM audit_log WHERE tenant_id = $1 \
               AND action = 'workflow.approval.resolve' AND object_id = $2 ORDER BY id DESC LIMIT 1",
        )
        .bind(TENANT)
        .bind(approval.to_string())
        .fetch_optional(&self.pool)
        .await
        .unwrap()
    }
}

/// 承認待ちの行が消えたら取り下げの出口へ進み、台帳を withdrawn にして決着を監査に残す
/// （relay の `withdraw_on_deletes`）。削除のイベントが来ても行がまだ実行主体に見えるなら取り下げない。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_the_row_withdraws_its_approval() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, None).await;
    let gone = env.submit(table, "取りやめた出張", &env.bob).await;
    let kept = env.submit(table, "続ける出張", &env.bob).await;
    env.settle().await;
    let gone_run = env.run_of(wf, gone).await;
    let kept_run = env.run_of(wf, kept).await;
    assert_eq!(env.step(gone_run, "ok").await.0, "waiting_event");
    assert_eq!(env.step(kept_run, "ok").await.0, "waiting_event");

    // 申請者が行を削除する。続けて、まだある行にも削除のイベント（同じ形・行だけ違う）が届いたことにする。
    let row = env
        .data
        .get_record(&env.alice, table, gone, None)
        .await
        .unwrap();
    env.data
        .delete_record(&env.alice, table, gone, row.rev, None)
        .await
        .unwrap();
    let forged: i64 = sqlx::query_scalar(
        "INSERT INTO storage_event_outbox (org, tenant_id, node_id, version, op, actor, trace_id, payload) \
         SELECT org, tenant_id, node_id, version, op, actor, trace_id, \
                jsonb_set(payload, '{record_id}', to_jsonb($3::text)) \
         FROM storage_event_outbox \
         WHERE tenant_id = $1 AND payload->>'event_type' = 'data.record.changed' \
           AND payload->>'change' = 'delete' AND payload->>'record_id' = $2 \
         ORDER BY id DESC LIMIT 1 RETURNING id",
    )
    .bind(TENANT)
    .bind(gone.to_string())
    .bind(kept.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(forged > 0);
    env.settle().await;

    let (st, ports, out) = env.step(gone_run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["withdrawn".to_string()])
    );
    assert_eq!(out.unwrap()["decision"], "withdrawn");
    let (gone_id, status) = env.approval_of(gone_run).await;
    assert_eq!(status, "withdrawn");
    let audit = env.resolve_audit(gone_id).await.expect("決着の監査");
    assert_eq!(audit["status"], "withdrawn");
    assert_eq!(audit["run_id"], json!(gone_run));

    // まだ見える行は取り下げない（受信箱にも出続ける）。
    assert_eq!(env.step(kept_run, "ok").await.0, "waiting_event");
    assert_eq!(env.approval_of(kept_run).await.1, "pending");
    assert!(env.item_for(&env.bob, kept).await.is_some());
    assert!(env.item_for(&env.bob, gone).await.is_none());
}

/// 承認待ちの途中で実行主体（委譲元の委譲）が外れたら、受付の突合でも判断の遷移を受けた relay でも
/// 取り下げとして決め、理由を監査に残す（受信箱で判断されたのに run が期限まで待つ食い違いを作らない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approvals_are_withdrawn_when_the_delegation_is_revoked() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, None).await;
    let unreconciled = env.submit(table, "研修の申込", &env.bob).await;
    let decided = env.submit(table, "書籍の購入", &env.bob).await;
    env.settle().await;
    let first = env.run_of(wf, unreconciled).await;
    let second = env.run_of(wf, decided).await;
    assert_eq!(env.step(first, "ok").await.0, "waiting_event");
    assert_eq!(env.step(second, "ok").await.0, "waiting_event");
    let item = env
        .item_for(&env.bob, decided)
        .await
        .expect("bob の受信箱に出る");

    // 委譲元（alice）が委譲を外した（再同意で外れたのと同じ台帳の状態）。
    sqlx::query(
        "UPDATE workflow_delegation SET revoked_at = now() WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(TENANT)
    .bind(wf)
    .execute(&env.pool)
    .await
    .unwrap();

    // 受付の突合: 委譲が無いので行を読めない。待ち続けずに取り下げる。
    sqlx::query(
        "UPDATE workflow_approval SET reconciled_at = NULL WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(first)
    .execute(&env.pool)
    .await
    .unwrap();
    env.settle().await;
    let (st, ports, _) = env.step(first, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["withdrawn".to_string()])
    );
    let (first_id, status) = env.approval_of(first).await;
    assert_eq!(status, "withdrawn");
    let audit = env.resolve_audit(first_id).await.expect("決着の監査");
    assert_eq!(audit["reason"], "actor_unavailable");

    // 判断の遷移: bob は受信箱から判断できる（行は bob の権限で承認へ進む）が、実行主体にはその遷移が
    // 見えない。期限まで待たせず、取り下げとして決める。
    let (st, body) = env
        .decide(
            &env.bob,
            item["approval_id"].as_str().unwrap(),
            json!({ "decision": "approve", "expected_rev": item["record"]["rev"] }),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    env.settle().await;
    let (st, ports, _) = env.step(second, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["withdrawn".to_string()])
    );
    let (second_id, status) = env.approval_of(second).await;
    assert_eq!(status, "withdrawn");
    let audit = env.resolve_audit(second_id).await.expect("決着の監査");
    assert_eq!(audit["reason"], "actor_unavailable");
}

impl Env {
    /// 受付済みの申請（下書き → 申請 → 受付・alice）と、それを承認に掛ける対話のワークフロー（alice が起動）。
    async fn interactive_approval(&self, table: Uuid) -> Uuid {
        let t = table.to_string();
        let ir = json!({
            "ir_version": 1, "name": format!("ask-{}", Uuid::new_v4().simple()),
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ok", "type": "human.approval", "params": {
                "title": "経費申請の承認",
                "record": { "table": t, "id": { "$from": "input", "path": "/record_id" } },
                "assignees": [{ "field": "approver" }],
                "approve": { "to": "approved" },
                "reject": { "to": "rejected" } } }],
            "edges": []
        });
        self.workflows
            .create(&self.alice, &ir, &Catalog::default(), None)
            .await
            .expect("保存")
            .0
    }

    /// 申請して受付まで進め（alice）、対話のワークフローで承認に掛ける（run は alice が主体の user run）。
    async fn ask_interactively(&self, wf: Uuid, table: Uuid, title: &str) -> (Uuid, Uuid) {
        let record = self.submit(table, title, &self.bob).await;
        let row = self
            .data
            .get_record(&self.alice, table, record, None)
            .await
            .unwrap();
        self.fsms
            .transition(&self.alice, table, record, "reviewing", row.rev, None)
            .await
            .unwrap();
        let run = self
            .launcher
            .start_interactive(&self.alice, wf, &json!({ "record_id": record }))
            .await
            .unwrap()
            .expect("run");
        (record, run)
    }
}

/// user 主体の run（対話の起動）で、起動した本人が停止の後にテーブルを読めなくなったら、受付の突合でも
/// 判断の遷移を受けた relay でも取り下げとして決め、理由を監査に残す。委譲台帳を持たない主体なので、
/// テーブル単位の NotFound / Forbidden（行が見えないだけとは別）で見分ける。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approvals_of_a_user_run_are_withdrawn_when_the_launcher_loses_the_table() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.interactive_approval(table).await;
    let (_, first) = env.ask_interactively(wf, table, "研修の申込").await;
    let (decided, second) = env.ask_interactively(wf, table, "書籍の購入").await;
    env.settle().await;
    let principal: String = sqlx::query_scalar(
        "SELECT principal_kind FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(second)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(principal, "user");
    assert_eq!(env.step(first, "ok").await.0, "waiting_event");
    assert_eq!(env.step(second, "ok").await.0, "waiting_event");
    let item = env
        .item_for(&env.bob, decided)
        .await
        .expect("bob の受信箱に出る");

    // 起動した alice がテーブルの権限（owner）を外された。委譲台帳は無いので、主体そのものは作れる。
    let object = env.alice.ns().data_table(&table.to_string());
    env.fga
        .delete_tuple(&env.alice.subject(), Relation::Owner, &object)
        .await
        .unwrap();

    // 受付の突合: テーブルを読めないので待ち続けずに理由つきで取り下げる。
    sqlx::query(
        "UPDATE workflow_approval SET reconciled_at = NULL WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(first)
    .execute(&env.pool)
    .await
    .unwrap();
    env.settle().await;
    let (st, ports, _) = env.step(first, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["withdrawn".to_string()])
    );
    let (first_id, status) = env.approval_of(first).await;
    assert_eq!(status, "withdrawn");
    let audit = env.resolve_audit(first_id).await.expect("決着の監査");
    assert_eq!(audit["reason"], "actor_unavailable");

    // 判断の遷移: bob は受信箱から判断できる（行は bob の権限で承認へ進む）が、実行主体の alice にはテーブルが
    // 見えない（change_event_views がテーブル単位で断る）。期限まで待たせず、取り下げとして決める。
    let (st, body) = env
        .decide(
            &env.bob,
            item["approval_id"].as_str().unwrap(),
            json!({ "decision": "approve", "expected_rev": item["record"]["rev"] }),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    env.settle().await;
    let row = env
        .data
        .get_record(&env.bob, table, decided, None)
        .await
        .unwrap();
    assert_eq!(row.data["status"], "approved");
    let (st, ports, _) = env.step(second, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["withdrawn".to_string()])
    );
    let (second_id, status) = env.approval_of(second).await;
    assert_eq!(status, "withdrawn");
    let audit = env.resolve_audit(second_id).await.expect("決着の監査");
    assert_eq!(audit["reason"], "actor_unavailable");
    assert_eq!(audit["run_id"], json!(second));
}

impl Env {
    /// 判断の監査（`workflow.approval.decide`）の (decision, metadata)（その人の分だけ・古い順）。
    async fn decide_audits(&self, approval: &str, user: &AuthContext) -> Vec<(String, Value)> {
        let rows: Vec<(String, sqlx::types::Json<Value>)> = sqlx::query_as(
            "SELECT decision, metadata FROM audit_log WHERE tenant_id = $1 \
               AND action = 'workflow.approval.decide' AND object_id = $2 AND actor = $3 ORDER BY id",
        )
        .bind(TENANT)
        .bind(approval)
        .bind(&user.principal.id)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        rows.into_iter().map(|(d, m)| (d, m.0)).collect()
    }
}

/// 遷移の前に断った判断（テーブルの editor が無い 403・行が見えない 404）にも、遷移の失敗と同じ形の
/// `workflow.approval.decide` の Deny を残す。見えない行の承認は、この承認に無い判断・コメントの要否・
/// もう決まったかより先に 404 で断る（担当でない・存在しないと同じ・承認の形と状態を読ませない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decisions_refused_before_the_transition_are_audited() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, None).await;
    let mine = env.submit(table, "出張旅費", &env.bob).await;
    let others = env.submit(table, "備品の購入", &env.dave).await;
    env.settle().await;
    let item = env
        .item_for(&env.bob, mine)
        .await
        .expect("bob の受信箱に出る");
    let id = item["approval_id"].as_str().unwrap().to_string();
    let rev = item["record"]["rev"].as_i64().unwrap();
    let run = env.run_of(wf, mine).await;

    // 経理（carol・ロールの担当者）をテーブルの viewer だけにする: 行は見えるが遷移できない（403）。
    let object = env.alice.ns().data_table(&table.to_string());
    env.fga
        .delete_tuple(&env.carol.subject(), Relation::Editor, &object)
        .await
        .unwrap();
    env.fga
        .write_tuple(&env.carol.subject(), Relation::Viewer, &object)
        .await
        .unwrap();
    let (st, body) = env
        .decide(
            &env.carol,
            &id,
            json!({ "decision": "approve", "expected_rev": rev }),
        )
        .await;
    assert_eq!(
        (st, body["code"].clone()),
        (StatusCode::FORBIDDEN, json!("not_allowed")),
        "{body}"
    );
    let audits = env.decide_audits(&id, &env.carol).await;
    assert_eq!(audits.len(), 1, "{audits:?}");
    assert_eq!(audits[0].0, "deny");
    assert_eq!(audits[0].1["decision"], "approve");
    assert_eq!(audits[0].1["to"], "approved");
    assert_eq!(audits[0].1["run_id"], json!(run));
    assert_eq!(audits[0].1["table_id"], json!(table));
    assert_eq!(audits[0].1["record_id"], json!(mine));

    // bob を dave の申請の承認の担当者にも足す（担当者は IR の作者が書ける）。その行は bob に見えない。
    let other_run = env.run_of(wf, others).await;
    let (other_id, _) = env.approval_of(other_run).await;
    sqlx::query(
        "INSERT INTO workflow_approval_assignee (tenant_id, approval_id, kind, subject) \
         VALUES ($1, $2, 'user', $3)",
    )
    .bind(TENANT)
    .bind(other_id)
    .bind(&env.bob.principal.id)
    .execute(&env.pool)
    .await
    .unwrap();
    let other = other_id.to_string();
    // この承認に無い判断（差し戻し）・コメント必須の判断（却下）でも、400 ではなく 404。
    for body in [
        json!({ "decision": "return", "expected_rev": 1 }),
        json!({ "decision": "reject", "expected_rev": 1 }),
    ] {
        let (st, resp) = env.decide(&env.bob, &other, body).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{resp}");
    }
    let audits = env.decide_audits(&other, &env.bob).await;
    assert_eq!(
        audits.iter().map(|(d, _)| d.as_str()).collect::<Vec<_>>(),
        ["deny", "deny"]
    );
    assert_eq!(audits[0].1["to"], Value::Null, "この承認に無い判断");
    assert_eq!(audits[1].1["to"], "rejected");
    // 決まった後も 409 ではなく 404（見えない行の承認が決まったかを読ませない）。
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
    assert_eq!(env.approval_of(other_run).await.1, "approved");
    let (st, resp) = env
        .decide(
            &env.bob,
            &other,
            json!({ "decision": "approve", "expected_rev": row.rev + 1 }),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{resp}");
}

/// 判断の外で行が動いた直後（relay が取り下げる前）に、いまの rev で判断しても 409 decided になり、行は判断の
/// 行き先へ進まない（FSM にほかの状態から判断の行き先への遷移があっても）。コメントも残さず、断った判断は
/// Deny として監査に残す。relay が遷移を受けたら、承認は取り下げになる。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_row_moved_outside_the_decision_cannot_be_decided() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    // 受付 → 下書き（差し戻し）→ 承認 と、判断の外の経路でも承認の状態へ進める表。
    let table = env
        .expense_table_with(&[("reviewing", "draft"), ("draft", "approved")])
        .await;
    let wf = env.enable(table, None).await;
    let record = env.submit(table, "出張旅費", &env.bob).await;
    env.settle().await;
    let run = env.run_of(wf, record).await;
    assert_eq!(env.step(run, "ok").await.0, "waiting_event");
    let item = env
        .item_for(&env.bob, record)
        .await
        .expect("bob の受信箱に出る");
    let id = item["approval_id"].as_str().unwrap().to_string();
    let rev = item["record"]["rev"].as_i64().unwrap();

    // 判断の外（テーブル画面の状態のメニュー）で、bob が行を下書きへ戻した。
    let moved = env
        .fsms
        .transition(&env.bob, table, record, "draft", rev, None)
        .await
        .unwrap();
    // relay が取り下げる前に、いまの rev で承認する。下書きから承認へは FSM で進めるが、判断としては断る。
    let (st, body) = env
        .decide(
            &env.bob,
            &id,
            json!({ "decision": "approve", "comment": "了解", "expected_rev": moved.rev }),
        )
        .await;
    assert_eq!(
        (st, body["code"].clone()),
        (StatusCode::CONFLICT, json!("decided")),
        "{body}"
    );
    let row = env
        .data
        .get_record(&env.bob, table, record, None)
        .await
        .unwrap();
    assert_eq!(row.data["status"], "draft", "判断の行き先へ進めない");
    assert_eq!(row.rev, moved.rev);
    let comments: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_approval_comment WHERE tenant_id = $1 AND approval_id = $2",
    )
    .bind(TENANT)
    .bind(Uuid::parse_str(&id).unwrap())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(comments, 0, "断った判断のコメントを置かない");
    let audits = env.decide_audits(&id, &env.bob).await;
    assert_eq!(audits.len(), 1, "{audits:?}");
    assert_eq!(audits[0].0, "deny");
    assert_eq!(audits[0].1["comment"], true);

    // relay が下書きへの遷移を受けたら、承認は取り下げになる（行と run の結論が揃う）。
    env.settle().await;
    let (st, ports, _) = env.step(run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["withdrawn".to_string()])
    );
    assert_eq!(env.approval_of(run).await.1, "withdrawn");
}

/// AI（chat の emit_workflow）が書いた承認も、保存 API と同じ照合（V4）を通る。見えないテーブル・FSM で
/// 管理しないテーブル・無い状態・無い列は保存できない（材料を IR から引くカタログ・`catalog_for`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emit_workflow_checks_approvals_like_the_save_api() {
    use agent_core::Tool;

    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let fsm_table = env.expense_table().await;
    let plain = env
        .data
        .create_table(
            &env.alice,
            NewDataTable {
                name: format!("plain-{}", Uuid::new_v4().simple()),
                schema: expense_schema(None, &env.keiri),
            },
            None,
        )
        .await
        .unwrap()
        .id;
    // dave には見えないテーブル（alice の表）。
    let hidden = env
        .data
        .create_table(
            &env.alice,
            NewDataTable {
                name: format!("hidden-{}", Uuid::new_v4().simple()),
                schema: expense_schema(None, &env.keiri),
            },
            None,
        )
        .await
        .unwrap()
        .id;
    let source = api::workflow_catalog::ApiWorkflowCatalogSource::new(
        None,
        None,
        api::workflow_catalog::WorkflowModels::new(None, Vec::new()),
    )
    .with_data(Arc::clone(&env.data));
    let tool = chat::EmitWorkflowTool::new(Arc::new(env.workflows.clone()), Arc::new(source));
    // [承認の行き先, 受信箱に出す列, 担当者の列]
    let ir = |table: Uuid, [to, show, assignee]: [&str; 3]| {
        json!({ "ir": {
            "ir_version": 1, "name": format!("ai-approval-{}", Uuid::new_v4().simple()),
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ok", "type": "human.approval", "params": {
                "title": "承認", "record": { "table": table.to_string(), "id": { "$from": "input", "path": "/id" } },
                "assignees": [{ "field": assignee }], "approve": { "to": to }, "show_fields": [show]
            } }],
            "edges": []
        } })
    };
    let ok = ["approved", "title", "approver"];
    let cases = [
        (&env.alice, plain, ok, "ir.unknown_table"),
        (&env.dave, hidden, ok, "ir.unknown_table"),
        (
            &env.alice,
            fsm_table,
            ["accepted", "title", "approver"],
            "ir.unknown_state",
        ),
        (
            &env.alice,
            fsm_table,
            ["approved", "note", "approver"],
            "ir.unknown_field",
        ),
        (
            &env.alice,
            fsm_table,
            ["approved", "title", "title"],
            "ir.bad_assignee",
        ),
    ];
    for (who, table, params, code) in cases {
        let out = tool.call(who, ir(table, params), None).await.unwrap();
        assert!(out.is_error, "{code}: {}", out.content);
        assert!(out.content.contains(code), "{code}: {}", out.content);
        assert!(out.workflow_refs.is_empty());
    }
    let out = tool
        .call(&env.alice, ir(fsm_table, ok), None)
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.workflow_refs.len(), 1);
}
