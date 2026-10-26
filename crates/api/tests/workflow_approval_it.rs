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
//! テストごとに一意にする。テナントもテストごとに作り（`t-appr-<uuid>`）、最後にそのテナントの行を撤去する
//! （[`Env::cleanup`]・BUG-66）。single 構成の既定 `default` で動かすと、手元の DB で流したときに手元のサーバと
//! 同じテナントへ承認待ちの run が残り、前の回の残りが件数・受信箱の検査にも混ざっていた。

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

fn config(db_url: &str, tenant: &str) -> AppConfig {
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
            tenant_id: Some(tenant.into()),
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

fn ctx(id: &str, tenant: &str) -> AuthContext {
    AuthContext::new(principal(id), "acme".into(), tenant.into())
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
    /// このテストのテナント（テストごとに一意・最後に [`Env::cleanup`] で撤去する）。
    tenant: String,
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
    let tenant = format!("t-appr-{sfx}");
    let ids = ["alice", "bob", "carol", "dave"].map(|u| format!("{u}-{sfx}"));
    let sessions = Arc::new(MemorySessionStore::new());
    for u in &ids {
        sessions
            .put(
                &tenant,
                &format!("sid-{u}"),
                &api::session::SessionRecord {
                    principal: principal(u),
                    tenant_id: tenant.clone(),
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
        config(&db_url, &tenant),
        Arc::clone(&storage),
        sessions,
        &fga,
        Arc::clone(&data),
        Arc::clone(&artifacts),
    );
    let alice = ctx(&ids[0], &tenant);
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
        bob: ctx(&ids[1], &tenant),
        carol: ctx(&ids[2], &tenant),
        dave: ctx(&ids[3], &tenant),
        keiri: format!("keiri-{sfx}"),
        tenant,
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
            format!(
                "shiki_session=sid-{u}.{}; shiki_csrf=csrf-{u}",
                user.tenant_id
            ),
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
        .scoped_to_tenant(self.tenant.clone());
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
            .bind(&self.tenant)
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
        .bind(&self.tenant)
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
        .bind(&self.tenant)
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
    .bind(&env.tenant)
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
    .bind(&env.tenant)
    .bind(vec![run.to_string(), other_run.to_string()])
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(resolved, 2, "決着を監査に残す");
    env.cleanup().await;
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
    .bind(&env.tenant)
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
        .expire_due_approvals(chrono::Utc::now(), Some(env.tenant.as_str()))
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
    .bind(&env.tenant)
    .bind(id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(status, "expired");
    env.cleanup().await;
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
    .bind(&env.tenant)
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
    .bind(&env.tenant)
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
    env.cleanup().await;
}

impl Env {
    /// run の承認（台帳の id と状態）。
    async fn approval_of(&self, run: Uuid) -> (Uuid, String) {
        sqlx::query_as(
            "SELECT approval_id, status FROM workflow_approval WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&self.tenant)
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
        .bind(&self.tenant)
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
    .bind(&env.tenant)
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
    env.cleanup().await;
}

impl Env {
    /// 承認を待つ間に実行主体が失権した run の終わり方（D-62）: 取り下げの出口へ進めず、step は失敗
    /// （出力なし・`error.code` = actor_unavailable）、run は failed（`fail_reason` = actor_unavailable）、後続は
    /// 走らない。台帳は cancelled で、決着の監査に理由を残す。
    async fn assert_failed_unavailable(&self, run: Uuid, later: &[&str]) {
        let (st, ports, out) = self.step(run, "ok").await;
        assert_eq!((st.as_str(), ports), ("failed", Vec::<String>::new()));
        assert!(out.unwrap_or(Value::Null).is_null(), "出力を書かない");
        let (error, status, reason): (Option<sqlx::types::Json<Value>>, String, Option<String>) =
            sqlx::query_as(
                "SELECT s.error, r.status, r.fail_reason FROM step_execution s \
                 JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id \
                 WHERE s.tenant_id = $1 AND s.run_id = $2 AND s.node_id = 'ok'",
            )
            .bind(&self.tenant)
            .bind(run)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        let error = error.expect("step の error").0;
        assert_eq!(error["code"], "actor_unavailable", "{error}");
        assert_eq!(error["retryable"], false);
        assert_eq!(error["node_id"], "ok");
        assert_eq!(
            (status.as_str(), reason.as_deref()),
            ("failed", Some("actor_unavailable"))
        );
        for node in later {
            assert_eq!(
                self.step(run, node).await.0,
                "cancelled",
                "{node} は走らない"
            );
        }
        let (id, status) = self.approval_of(run).await;
        assert_eq!(status, "cancelled");
        let audit = self.resolve_audit(id).await.expect("決着の監査");
        assert_eq!(audit["reason"], "actor_unavailable");
        assert_eq!(audit["status"], "cancelled");
        assert_eq!(audit["run_id"], json!(run));
    }

    /// このテストのテナントの行をすべて撤去する（テストの最後に呼ぶ・BUG-66）。
    ///
    /// 撤去は本番の `StorageService::purge_tenant`（テナント境界の全テーブルを導出して消す）に任せる。
    /// 承認待ちの run・購読・申請の行・outbox を同じ DB のほかのテストや手元のサーバに残さない。
    async fn cleanup(&self) {
        self.storage
            .purge_tenant(&self.tenant, "acme", "provisioner:test")
            .await
            .expect("後片付け");
        let left: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM workflow_run WHERE tenant_id = $1) \
                  + (SELECT COUNT(*) FROM workflow_approval WHERE tenant_id = $1) \
                  + (SELECT COUNT(*) FROM wait_subscription WHERE tenant_id = $1) \
                  + (SELECT COUNT(*) FROM data_record WHERE tenant_id = $1) \
                  + (SELECT COUNT(*) FROM storage_event_outbox WHERE tenant_id = $1)",
        )
        .bind(&self.tenant)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        assert_eq!(left, 0, "テストのテナントに行が残っている");
    }

    /// テストで作った run を消す（step・承認の台帳は FK で消え、購読は先に消す・同じ DB のほかのテストやサーバに
    /// 残さない）。
    async fn forget_runs(&self, wf: Uuid) {
        // 購読は run への外部キーが無いので先に消す（台帳・step は run と一緒に消える）。
        sqlx::query(
            "DELETE FROM wait_subscription w USING workflow_run r \
             WHERE w.tenant_id = r.tenant_id AND w.run_id = r.run_id \
               AND r.tenant_id = $1 AND r.workflow_id = $2",
        )
        .bind(&self.tenant)
        .bind(wf)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2")
            .bind(&self.tenant)
            .bind(wf)
            .execute(&self.pool)
            .await
            .unwrap();
    }
}

/// 承認待ちの途中で実行主体（委譲元の委譲）が外れたら、受付の突合でも判断の遷移を受けた relay でも、取り下げの
/// 出口へ進めずに run を actor_unavailable で失敗させ、理由を監査に残す（D-62）。受信箱で判断されたのに run が
/// 期限まで待つ食い違いも、後続が委譲なしで中途半端に走ることも作らない。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runs_fail_when_the_delegation_is_revoked_while_waiting() {
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
    .bind(&env.tenant)
    .bind(wf)
    .execute(&env.pool)
    .await
    .unwrap();

    // 受付の突合: 委譲が無いので行を読めない。待ち続けず、取り下げの出口へも進めずに run を失敗させる。
    sqlx::query(
        "UPDATE workflow_approval SET reconciled_at = NULL WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&env.tenant)
    .bind(first)
    .execute(&env.pool)
    .await
    .unwrap();
    env.settle().await;
    env.assert_failed_unavailable(first, &["done", "ng"]).await;

    // 判断の遷移: bob は受信箱から判断できる（行は bob の権限で承認へ進む）が、実行主体にはその遷移が
    // 見えない。期限まで待たせず、承認の出口へも進めずに run を失敗させる。
    let (st, body) = env
        .decide(
            &env.bob,
            item["approval_id"].as_str().unwrap(),
            json!({ "decision": "approve", "expected_rev": item["record"]["rev"] }),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    env.settle().await;
    env.assert_failed_unavailable(second, &["done", "ng"]).await;
    env.forget_runs(wf).await;
    env.cleanup().await;
}

/// 期限の来た承認も、実行主体が失権していれば timeout の出口へ進めずに run を actor_unavailable で失敗させる
/// （受付の突合・判断の遷移を受けた relay と揃える・D-62）。失権していない承認は従来どおり timeout の出口へ進む。
/// 回収は scheduler の tick と同じ口（`api::workflow_runtime::expire_due_approvals`）で呼ぶ。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn due_approvals_of_an_unavailable_actor_fail_instead_of_timing_out() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    // ワークフローごとに別のテーブル（同じ行の受付の遷移を 2 本の run で取り合わない）。
    let (revoked_table, kept_table) = (env.expense_table().await, env.expense_table().await);
    let revoked_wf = env.enable(revoked_table, Some(3600)).await;
    let kept_wf = env.enable(kept_table, Some(3600)).await;
    let revoked_record = env.submit(revoked_table, "出張の精算", &env.bob).await;
    let kept_record = env.submit(kept_table, "会議室の予約", &env.bob).await;
    env.settle().await;
    let revoked = env.run_of(revoked_wf, revoked_record).await;
    let kept = env.run_of(kept_wf, kept_record).await;
    assert_eq!(env.step(revoked, "ok").await.0, "waiting_event");
    assert_eq!(env.step(kept, "ok").await.0, "waiting_event");

    // 受付の突合を済ませた後で委譲が外れ、誰も判断しないまま期限が来た（relay には何も届かない）。
    sqlx::query(
        "UPDATE workflow_delegation SET revoked_at = now() WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&env.tenant)
    .bind(revoked_wf)
    .execute(&env.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE wait_subscription SET timeout_at = now() - interval '1 second' \
         WHERE tenant_id = $1 AND run_id = ANY($2) AND NOT fired",
    )
    .bind(&env.tenant)
    .bind(vec![revoked, kept])
    .execute(&env.pool)
    .await
    .unwrap();

    api::workflow_runtime::expire_due_approvals(
        &env.pool,
        &env.runs,
        &env.data,
        &env.fsms,
        chrono::Utc::now(),
        Some(env.tenant.as_str()),
    )
    .await
    .unwrap();
    env.settle().await;
    env.assert_failed_unavailable(revoked, &["late", "done", "ng"])
        .await;
    // 失権していない承認は timeout の出口へ進み、台帳は expired（理由なし）。
    let (st, ports, _) = env.step(kept, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["timeout".to_string()])
    );
    assert_eq!(env.step(kept, "late").await.0, "succeeded");
    let (kept_id, status) = env.approval_of(kept).await;
    assert_eq!(status, "expired");
    let audit = env.resolve_audit(kept_id).await.expect("決着の監査");
    assert_eq!(audit["status"], "expired");
    assert!(audit.get("reason").is_none(), "{audit}");
    env.forget_runs(revoked_wf).await;
    env.forget_runs(kept_wf).await;
    env.cleanup().await;
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
/// 判断の遷移を受けた relay でも、取り下げの出口へ進めずに run を actor_unavailable で失敗させ、理由を監査に
/// 残す（D-62）。委譲台帳を持たない主体なので、テーブル単位の NotFound / Forbidden（行が見えないだけとは別）で
/// 見分ける。起動した本人の画面に返す失敗の理由は権限の分類になる（GAP-09）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_runs_fail_when_the_launcher_loses_the_table_while_waiting() {
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
    .bind(&env.tenant)
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

    // 受付の突合: テーブルを読めないので待ち続けず、取り下げの出口へも進めずに run を失敗させる。
    sqlx::query(
        "UPDATE workflow_approval SET reconciled_at = NULL WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&env.tenant)
    .bind(first)
    .execute(&env.pool)
    .await
    .unwrap();
    env.settle().await;
    env.assert_failed_unavailable(first, &[]).await;

    // 判断の遷移: bob は受信箱から判断できる（行は bob の権限で承認へ進む）が、実行主体の alice にはテーブルが
    // 見えない（change_event_views がテーブル単位で断る）。期限まで待たせず、run を失敗させる。
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
    env.assert_failed_unavailable(second, &[]).await;

    // 起動した本人の画面に返す失敗の理由は「権限が足りない」（生の理由は出さない・GAP-09）。
    let outcomes = workflow_engine::RunOutcomeReader::new(env.pool.clone())
        .own_outcomes(&env.alice, &[first, second], &[])
        .await
        .unwrap();
    for run in [first, second] {
        let f = outcomes[&run].failure.as_ref().expect("失敗の理由");
        assert_eq!(f.category, workflow_engine::FailureCategory::Permission);
        assert!(!f.message.contains("actor_unavailable"), "{}", f.message);
    }
    env.forget_runs(wf).await;
    env.cleanup().await;
}

impl Env {
    /// 判断の監査（`workflow.approval.decide`）の (decision, metadata)（その人の分だけ・古い順）。
    async fn decide_audits(&self, approval: &str, user: &AuthContext) -> Vec<(String, Value)> {
        let rows: Vec<(String, sqlx::types::Json<Value>)> = sqlx::query_as(
            "SELECT decision, metadata FROM audit_log WHERE tenant_id = $1 \
               AND action = 'workflow.approval.decide' AND object_id = $2 AND actor = $3 ORDER BY id",
        )
        .bind(&self.tenant)
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
    .bind(&env.tenant)
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
    env.cleanup().await;
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
    .bind(&env.tenant)
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
    env.cleanup().await;
}

impl Env {
    /// run の承認を写して、`user` 宛ての承認を新しい側に `from..=to` 件並べる（step も写す・台帳の担当者は
    /// IR の作者が書けるので、本人に見えない行の承認が受信箱の台帳に並ぶことがある）。
    async fn clone_approvals(&self, run: Uuid, user: &AuthContext, from: i32, to: i32) {
        sqlx::query(
            "INSERT INTO step_execution \
             SELECT (jsonb_populate_record(NULL::step_execution, \
                     to_jsonb(s) || jsonb_build_object('step_path', s.step_path || '-' || g))).* \
             FROM step_execution s \
             JOIN workflow_approval a ON a.tenant_id = s.tenant_id AND a.run_id = s.run_id \
                                     AND a.step_path = s.step_path, \
                  generate_series($3::int, $4::int) g \
             WHERE a.tenant_id = $1 AND a.run_id = $2 AND a.step_path NOT LIKE '%-%'",
        )
        .bind(&self.tenant)
        .bind(run)
        .bind(from)
        .bind(to)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO workflow_approval \
             SELECT (jsonb_populate_record(NULL::workflow_approval, to_jsonb(a) || jsonb_build_object( \
                     'approval_id', gen_random_uuid(), 'step_path', a.step_path || '-' || g, \
                     'created_at', now() + g * interval '1 millisecond'))).* \
             FROM workflow_approval a, generate_series($3::int, $4::int) g \
             WHERE a.tenant_id = $1 AND a.run_id = $2 AND a.step_path NOT LIKE '%-%'",
        )
        .bind(&self.tenant)
        .bind(run)
        .bind(from)
        .bind(to)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO workflow_approval_assignee (tenant_id, approval_id, kind, subject) \
             SELECT tenant_id, approval_id, 'user', $3 FROM workflow_approval \
             WHERE tenant_id = $1 AND run_id = $2 AND step_path LIKE '%-%' ON CONFLICT DO NOTHING",
        )
        .bind(&self.tenant)
        .bind(run)
        .bind(&user.principal.id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// 受信箱を 1 ページずつ最後まで読む（各ページの件数と、出た行の id）。
    async fn inbox_pages(&self, user: &AuthContext) -> (Vec<usize>, Vec<Value>) {
        let (mut sizes, mut records) = (Vec::new(), Vec::new());
        let mut cursor: Option<String> = None;
        loop {
            let path = match &cursor {
                Some(c) => format!("/approvals?cursor={c}"),
                None => "/approvals".to_string(),
            };
            let (st, body) = self.get(user, &path).await;
            assert_eq!(st, StatusCode::OK, "{body}");
            let items = body["items"].as_array().unwrap();
            sizes.push(items.len());
            records.extend(items.iter().map(|i| i["record"]["id"].clone()));
            match body["next_cursor"].as_str() {
                Some(c) => cursor = Some(c.to_string()),
                None => return (sizes, records),
            }
            assert!(sizes.len() < 20, "ページが終わらない: {sizes:?}");
        }
    }
}

/// 受信箱の新しい側に本人に見えない承認が並んでも、見える承認に辿り着ける。一覧は見える項目が 1 ページ分
/// たまるか、読み直す台帳の行の上限まで読み進める。件数は上限で打ち切ったら more を立てる（0 で黙らない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_inbox_reads_past_approvals_the_user_cannot_see() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, None).await;
    let mine = env.submit(table, "出張旅費", &env.bob).await;
    let hidden = env.submit(table, "備品の購入", &env.dave).await;
    env.settle().await;
    let hidden_run = env.run_of(wf, hidden).await;
    assert_eq!(env.step(hidden_run, "ok").await.0, "waiting_event");

    // bob に見えない行（dave の申請）の承認を、bob 宛てに新しい側へ 60 件並べる（1 ページの 50 件を超える）。
    env.clone_approvals(hidden_run, &env.bob, 1, 60).await;
    let (st, body) = env.get(&env.bob, "/approvals").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{body}");
    assert_eq!(items[0]["record"]["id"], json!(mine));
    assert!(body["next_cursor"].is_null(), "{body}");
    let (_, count) = env.get(&env.bob, "/approvals/count").await;
    assert_eq!(count, json!({ "count": 1, "more": false }));

    // 見えない承認が読み直す行の上限（500）を超えると、1 ページ目は空でも続きの位置を返し、続きを読めば届く。
    env.clone_approvals(hidden_run, &env.bob, 61, 520).await;
    let (sizes, records) = env.inbox_pages(&env.bob).await;
    assert_eq!(sizes.first(), Some(&0), "{sizes:?}");
    assert_eq!(records, vec![json!(mine)], "{sizes:?}");
    // 件数は数え切れなかったことを more で返す（数えた分は 0）。
    let (_, count) = env.get(&env.bob, "/approvals/count").await;
    assert_eq!(count, json!({ "count": 0, "more": true }));
    env.cleanup().await;
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
    env.cleanup().await;
}

impl Env {
    /// 状態の遷移の規則つきの表で、`masked` の列だけを経理（carol）に限って読ませたもの（alice が owner）。
    /// bob・carol はテーブルの editor（owner ではない）。
    async fn masked_fsm_table(&self, masked: &str) -> Uuid {
        let states = ["draft", "approved", "rejected"].map(String::from).to_vec();
        let schema = |fsm: Option<FsmRef>| {
            let mut status = FieldDef::new("status", FieldType::Select);
            status.options = states.clone();
            TableSchema {
                fields: vec![
                    FieldDef::new("title", FieldType::Text),
                    FieldDef::new("approver", FieldType::UserRef),
                    FieldDef::new("deputy", FieldType::UserRef),
                    status,
                ],
                status_field: Some("status".into()),
                row_policy: None,
                field_policy: vec![FieldPolicy {
                    field: masked.into(),
                    readable_by: PolicyExpr::HasRole {
                        role: self.keiri.clone(),
                        subtree: true,
                    },
                }],
                aggregate_min_rows: None,
                fsm_ref: fsm,
                rag: None,
            }
        };
        let table = self
            .data
            .create_table(
                &self.alice,
                NewDataTable {
                    name: format!("masked-{masked}-{}", Uuid::new_v4().simple()),
                    schema: schema(None),
                },
                None,
            )
            .await
            .unwrap();
        let t = |to: &str| FsmTransition {
            from: "draft".into(),
            to: to.into(),
            actor: PolicyExpr::IsOwner,
        };
        let fsm_id = self
            .fsms
            .create(
                &self.alice,
                &format!("flow-{}", table.id.simple()),
                table.id,
                &FsmBody {
                    states: states.clone(),
                    transitions: vec![t("approved"), t("rejected")],
                },
                None,
            )
            .await
            .unwrap();
        self.data
            .update_table_schema(
                &self.alice,
                table.id,
                schema(Some(FsmRef {
                    artifact_id: fsm_id,
                    version: 1,
                })),
                Some(1),
                None,
            )
            .await
            .unwrap();
        let object = self.alice.ns().data_table(&table.id.to_string());
        for u in [&self.bob, &self.carol] {
            self.fga
                .write_tuple(&u.subject(), Relation::Editor, &object)
                .await
                .unwrap();
        }
        table.id
    }

    /// 承認の照合（V4）の答えを、検証 API・保存 API・AI の emit_workflow の 3 つの口で集める（`key` が空でなければ
    /// `<K>` に置き換える。書いた名前の違いだけを消して、答えの形を比べる）。
    async fn v4_answers(
        &self,
        tool: &chat::EmitWorkflowTool,
        who: &AuthContext,
        ir: &Value,
        key: &str,
    ) -> [String; 3] {
        use agent_core::Tool;
        let body = || Body::from(json!({ "ir": ir }).to_string());
        let (st, checked) = self
            .send(req("POST", who, "/workflows/validate", body()))
            .await;
        assert_eq!(st, StatusCode::OK, "{checked}");
        let (st, saved) = self.send(req("POST", who, "/workflows", body())).await;
        let out = tool.call(who, json!({ "ir": ir }), None).await.unwrap();
        [
            checked["errors"].to_string(),
            format!("{st} {saved}"),
            format!("{} {}", out.is_error, out.content),
        ]
        .map(|s| {
            if key.is_empty() {
                s
            } else {
                s.replace(key, "<K>")
            }
        })
    }

    /// 検証 API が返す誤りの (code, path)。
    async fn v4_codes(&self, who: &AuthContext, ir: &Value) -> Vec<(String, String)> {
        let (st, body) = self
            .send(req(
                "POST",
                who,
                "/workflows/validate",
                Body::from(json!({ "ir": ir }).to_string()),
            ))
            .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["code"].as_str().unwrap_or_default().to_string(),
                    e["path"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }
}

/// 承認の保存時の照合（V4）は保存者の見え方で行う。テーブルの owner でない編集者には、読めない列も無い列と
/// 同じ誤り（code・path・message）になり、種別（ユーザーの列か）も返らない。状態の列が読めなければ、実在の状態を
/// 書いても無い状態を書いても、FSM でないテーブル・無いテーブルと同じ誤りで止まり、「使える状態」の一覧は出ない。
/// 検証 API・保存 API・AI の emit_workflow（`catalog_for`）のどれでも同じ。列を読める人（owner・経理）には従来どおり。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approval_checks_do_not_tell_unreadable_columns_from_missing_ones() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    // メモ（文字の列）は経理（carol）だけが読める。
    let expense = env.expense_table().await;
    let deputy = env.masked_fsm_table("deputy").await;
    let status = env.masked_fsm_table("status").await;
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
    env.fga
        .write_tuple(
            &env.bob.subject(),
            Relation::Editor,
            &env.alice.ns().data_table(&plain.to_string()),
        )
        .await
        .unwrap();
    let source = api::workflow_catalog::ApiWorkflowCatalogSource::new(
        None,
        None,
        api::workflow_catalog::WorkflowModels::new(None, Vec::new()),
    )
    .with_data(Arc::clone(&env.data));
    let tool = chat::EmitWorkflowTool::new(Arc::new(env.workflows.clone()), Arc::new(source));
    // [承認の行き先, 受信箱に出す列, 担当者の列]
    let ir = |table: Uuid, [to, show, assignee]: [&str; 3]| {
        json!({
            "ir_version": 1, "name": format!("masked-oracle-{}", table.simple()),
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ok", "type": "human.approval", "params": {
                "title": "承認", "record": { "table": table.to_string(), "id": { "$from": "input", "path": "/id" } },
                "assignees": [{ "field": assignee }], "approve": { "to": to }, "show_fields": ["title", show]
            } }],
            "edges": []
        })
    };
    let ghost = "zz_nope";
    // (表, 読めない名前を入れた引数, 無い名前を入れた引数, 読めない名前, 期待する code と path)
    let pairs = [
        (
            expense,
            ["approved", "memo", "approver"],
            ["approved", ghost, "approver"],
            "memo",
            ("ir.unknown_field", "/params/show_fields/1"),
        ),
        (
            expense,
            ["approved", "title", "memo"],
            ["approved", "title", ghost],
            "memo",
            ("ir.unknown_field", "/params/assignees/0/field"),
        ),
        (
            deputy,
            ["approved", "title", "deputy"],
            ["approved", "title", ghost],
            "deputy",
            ("ir.unknown_field", "/params/assignees/0/field"),
        ),
        (
            deputy,
            ["approved", "deputy", "approver"],
            ["approved", ghost, "approver"],
            "deputy",
            ("ir.unknown_field", "/params/show_fields/1"),
        ),
    ];
    for (table, masked, missing, key, (code, path)) in pairs {
        let codes = env.v4_codes(&env.bob, &ir(table, masked)).await;
        assert_eq!(codes, vec![(code.to_string(), path.to_string())], "{key}");
        let a = env
            .v4_answers(&tool, &env.bob, &ir(table, masked), key)
            .await;
        let b = env
            .v4_answers(&tool, &env.bob, &ir(table, missing), ghost)
            .await;
        assert_eq!(a, b, "読めない列 {key} と無い列で答えが違う");
        assert!(a[1].starts_with("400 "), "{}", a[1]);
        assert!(a[2].starts_with("true "), "{}", a[2]);
    }

    // 状態の列が読めない: 実在の状態でも無い状態でも、FSM でない表・無い表と同じ誤りで止まる（状態は出さない）。
    let unseen = Uuid::new_v4();
    let real = env
        .v4_answers(
            &tool,
            &env.bob,
            &ir(status, ["approved", "title", "approver"]),
            "",
        )
        .await;
    for (table, to) in [(status, ghost), (plain, "approved"), (unseen, "approved")] {
        let other = env
            .v4_answers(&tool, &env.bob, &ir(table, [to, "title", "approver"]), "")
            .await;
        // 表の id（名前に入れている）だけを揃えて比べる。
        let norm = |s: &str, t: Uuid| {
            s.replace(&t.simple().to_string(), "<T>")
                .replace(&t.to_string(), "<T>")
        };
        for (x, y) in real.iter().zip(&other) {
            assert_eq!(
                norm(x, status),
                norm(y, table),
                "状態の列が読めない表と {table} で答えが違う"
            );
        }
    }
    assert_eq!(
        env.v4_codes(&env.bob, &ir(status, ["approved", "title", "approver"]))
            .await,
        vec![(
            "ir.unknown_table".to_string(),
            "/params/record/table".to_string()
        )]
    );
    for s in &real {
        for state in ["draft", "approved", "rejected", "使える状態"] {
            assert!(!s.contains(state), "状態の一覧を出さない: {s}");
        }
    }

    // 列を読める人には従来どおり: 経理（owner でない）はメモ・代理・状態を使える。owner は無い列を言い切れ、
    // 種別の違う担当者の列も分かる。
    for (table, args) in [
        (expense, ["approved", "memo", "approver"]),
        (deputy, ["approved", "deputy", "deputy"]),
        (status, ["approved", "title", "approver"]),
    ] {
        assert_eq!(env.v4_codes(&env.carol, &ir(table, args)).await, vec![]);
    }
    assert_eq!(
        env.v4_codes(&env.carol, &ir(status, ["accepted", "title", "approver"]))
            .await,
        vec![(
            "ir.unknown_state".to_string(),
            "/params/approve/to".to_string()
        )]
    );
    assert_eq!(
        env.v4_codes(&env.alice, &ir(expense, ["approved", "memo", "memo"]))
            .await,
        vec![(
            "ir.bad_assignee".to_string(),
            "/params/assignees/0/field".to_string()
        )]
    );
    assert_eq!(
        env.v4_codes(&env.alice, &ir(expense, ["approved", ghost, "approver"]))
            .await,
        vec![(
            "ir.unknown_field".to_string(),
            "/params/show_fields/1".to_string()
        )]
    );

    // 受信箱に出す列に状態の列は選べない（無い列とは別の誤り・BUG-67）。状態の列が読めない人には、状態の列の
    // 名前を書いても表の誤りだけが返る（状態の列の名前を探らせない）。
    assert_eq!(
        env.v4_codes(&env.bob, &ir(expense, ["approved", "status", "approver"]))
            .await,
        vec![(
            "ir.bad_show_field".to_string(),
            "/params/show_fields/1".to_string()
        )]
    );
    assert_eq!(
        env.v4_codes(&env.bob, &ir(status, ["approved", "status", "approver"]))
            .await,
        vec![(
            "ir.unknown_table".to_string(),
            "/params/record/table".to_string()
        )]
    );
    env.cleanup().await;
}

impl Env {
    /// 本人（`user`）宛ての承認の、テーブルだけを別の表（`table`）に差し替えた写しを 1 件足す（台帳の担当者・
    /// テーブルは IR の作者が書けるので、本人に見えない表の承認が受信箱の台帳に並ぶことがある）。写しの id。
    async fn approval_on_table(&self, run: Uuid, user: &AuthContext, table: Uuid) -> Uuid {
        sqlx::query(
            "INSERT INTO step_execution \
             SELECT (jsonb_populate_record(NULL::step_execution, \
                     to_jsonb(s) || jsonb_build_object('step_path', s.step_path || '-hidden'))).* \
             FROM step_execution s \
             JOIN workflow_approval a ON a.tenant_id = s.tenant_id AND a.run_id = s.run_id \
                                     AND a.step_path = s.step_path \
             WHERE a.tenant_id = $1 AND a.run_id = $2 AND a.step_path NOT LIKE '%-%'",
        )
        .bind(&self.tenant)
        .bind(run)
        .execute(&self.pool)
        .await
        .unwrap();
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO workflow_approval \
             SELECT (jsonb_populate_record(NULL::workflow_approval, to_jsonb(a) || jsonb_build_object( \
                     'approval_id', gen_random_uuid(), 'step_path', a.step_path || '-hidden', \
                     'table_id', $3::uuid, 'created_at', now()))).* \
             FROM workflow_approval a \
             WHERE a.tenant_id = $1 AND a.run_id = $2 AND a.step_path NOT LIKE '%-%' \
             RETURNING approval_id",
        )
        .bind(&self.tenant)
        .bind(run)
        .bind(table)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO workflow_approval_assignee (tenant_id, approval_id, kind, subject) \
             VALUES ($1, $2, 'user', $3)",
        )
        .bind(&self.tenant)
        .bind(id)
        .bind(&user.principal.id)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    /// `user` がテーブル `table` について残した Deny の監査の件数。
    async fn table_denials(&self, user: &AuthContext, table: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = $2 \
               AND object_id = $3 AND decision = 'deny'",
        )
        .bind(&self.tenant)
        .bind(&user.principal.id)
        .bind(table.to_string())
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
}

/// 受信箱の一覧・件数は、他人が書いた台帳の候補を本人に見える分へ絞るだけなので、見えないテーブルの承認が
/// 並んでいても、読み直すたびに Deny の監査を増やさない（件数は 60 秒ごととフォーカス時に読まれる・BUG-43）。
/// 本人がその承認を開いたときは、従来どおり 404 で断り Deny を残す。見える承認は同じテーブルの行をまとめて
/// 読み、見えない行・状態が動いた行は出さない。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inbox_reads_do_not_audit_tables_the_user_cannot_see() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, None).await;
    let first = env.submit(table, "出張旅費", &env.bob).await;
    let second = env.submit(table, "会議費", &env.bob).await;
    let hidden_row = env.submit(table, "備品の購入", &env.dave).await;
    env.settle().await;
    // bob に見えない表（alice の表・bob には共有しない）の承認を、bob 宛てに 1 件並べる。
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
    let run = env.run_of(wf, first).await;
    let unseen = env.approval_on_table(run, &env.bob, hidden).await;
    // bob に見えない行（dave の申請）の承認も bob 宛てに足す（同じ表の行のまとめ取りで落ちる）。
    let hidden_run = env.run_of(wf, hidden_row).await;
    let (hidden_approval, _) = env.approval_of(hidden_run).await;
    sqlx::query(
        "INSERT INTO workflow_approval_assignee (tenant_id, approval_id, kind, subject) \
         VALUES ($1, $2, 'user', $3)",
    )
    .bind(&env.tenant)
    .bind(hidden_approval)
    .bind(&env.bob.principal.id)
    .execute(&env.pool)
    .await
    .unwrap();

    for _ in 0..3 {
        let (st, count) = env.get(&env.bob, "/approvals/count").await;
        assert_eq!(st, StatusCode::OK, "{count}");
        assert_eq!(count, json!({ "count": 2, "more": false }));
        let (st, body) = env.get(&env.bob, "/approvals").await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let mut ids: Vec<Value> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["record"]["id"].clone())
            .collect();
        ids.sort_by_key(|v| v.to_string());
        let mut want = vec![json!(first), json!(second)];
        want.sort_by_key(|v| v.to_string());
        assert_eq!(ids, want, "{body}");
    }
    assert_eq!(
        env.table_denials(&env.bob, hidden).await,
        0,
        "一覧・件数の読み直しで Deny を残さない"
    );
    // 本人が開いた承認は 404 で断り、Deny を残す。
    let (st, _) = env.get(&env.bob, &format!("/approvals/{unseen}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(env.table_denials(&env.bob, hidden).await >= 1);
    env.forget_runs(wf).await;
    env.cleanup().await;
}

impl Env {
    /// 行を `FOR UPDATE` で押さえた Tx（遷移を止める）と、その Tx の id（待っている人を見つける）。
    async fn hold_record(
        &self,
        record: Uuid,
    ) -> (sqlx::Transaction<'static, sqlx::Postgres>, String) {
        let mut tx = self.pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM data_record WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
            .bind(&self.tenant)
            .bind(record)
            .execute(&mut *tx)
            .await
            .unwrap();
        let xid: String =
            sqlx::query_scalar("SELECT (pg_current_xact_id()::text::bigint % 4294967296)::text")
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        (tx, xid)
    }

    /// Tx `xid` のロックを誰かが待つまで待つ（上限 20 秒）。
    async fn wait_blocked_on(&self, xid: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'transactionid' \
                   AND NOT granted AND transactionid::text = $1)",
            )
            .bind(xid)
            .fetch_one(&self.pool)
            .await
            .unwrap();
            if waiting {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "誰もロックを待たない");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 行の状態が `want` になるまで待つ（上限 20 秒）。
    async fn wait_status(&self, table: Uuid, record: Uuid, want: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let row = self
                .data
                .get_record(&self.alice, table, record, None)
                .await
                .unwrap();
            if row.data["status"] == want {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "行が {want} にならない"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn comment_rows(&self, approval: &str) -> Vec<(i64, String, String)> {
        sqlx::query_as(
            "SELECT rev, author, body FROM workflow_approval_comment \
             WHERE tenant_id = $1 AND approval_id = $2 ORDER BY author",
        )
        .bind(&self.tenant)
        .bind(Uuid::parse_str(approval).unwrap())
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

/// 判断の受付（コメントを置く → 遷移 → 失敗なら外す）は、要求が途中で取り消されても最後まで走る（BUG-47）。
/// 遷移を待っている間に要求を取り消しても、判断は遷移とコメントの両方が揃って決まり、置いたコメントだけが残って
/// 次の判断を塞ぐことはない。別の人が同じ rev に置き残したコメント（直す前の置き残し）も、正しい承認者の判断を
/// 塞がない（鍵に作者を含む）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_decisions_neither_lose_nor_strand_their_comment() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, None).await;
    let first = env.submit(table, "出張旅費", &env.bob).await;
    let second = env.submit(table, "会議費", &env.bob).await;
    env.settle().await;

    // 1 件目: 遷移を待っている間に、押した人の要求が取り消された（画面を閉じた・通信が切れた）。
    let item = env
        .item_for(&env.bob, first)
        .await
        .expect("bob の受信箱に出る");
    let id = item["approval_id"].as_str().unwrap().to_string();
    let rev = item["record"]["rev"].as_i64().unwrap();
    let (hold, xid) = env.hold_record(first).await;
    let pending = tokio::spawn(env.app.clone().oneshot(req(
        "POST",
        &env.bob,
        &format!("/approvals/{id}/decision"),
        Body::from(
            json!({ "decision": "approve", "comment": "1 回目", "expected_rev": rev }).to_string(),
        ),
    )));
    env.wait_blocked_on(&xid).await;
    pending.abort();
    assert!(pending.await.is_err(), "要求は取り消された");
    hold.commit().await.unwrap();
    // 判断は最後まで走り、遷移とコメントが揃って決まる。
    env.wait_status(table, first, "approved").await;
    env.settle().await;
    let run = env.run_of(wf, first).await;
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["approved".to_string()])
    );
    assert_eq!(out.unwrap()["comment"], "1 回目");
    assert_eq!(
        env.comment_rows(&id).await,
        vec![(rev + 1, env.bob.principal.id.clone(), "1 回目".to_string())]
    );

    // 2 件目: 別の人（経理の carol）が同じ rev に置き残したコメントがあっても、bob はコメント付きで判断できる。
    let item = env
        .item_for(&env.bob, second)
        .await
        .expect("bob の受信箱に出る");
    let id = item["approval_id"].as_str().unwrap().to_string();
    let rev = item["record"]["rev"].as_i64().unwrap();
    sqlx::query(
        "INSERT INTO workflow_approval_comment (tenant_id, approval_id, rev, author, decision, body) \
         VALUES ($1, $2, $3, $4, 'approve', '置き残し')",
    )
    .bind(&env.tenant)
    .bind(Uuid::parse_str(&id).unwrap())
    .bind(rev + 1)
    .bind(&env.carol.principal.id)
    .execute(&env.pool)
    .await
    .unwrap();
    let (st, body) = env
        .decide(
            &env.bob,
            &id,
            json!({ "decision": "approve", "comment": "規程どおりです", "expected_rev": rev }),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    env.settle().await;
    let run = env.run_of(wf, second).await;
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["approved".to_string()])
    );
    assert_eq!(
        out.unwrap()["comment"],
        "規程どおりです",
        "遷移した人のコメントだけを採る"
    );
    env.forget_runs(wf).await;
    env.cleanup().await;
}

impl Env {
    /// 期限の回収（scheduler の tick と同じ口）を `at` の時刻で呼ぶ。
    async fn expire_at(&self, at: chrono::DateTime<chrono::Utc>) {
        api::workflow_runtime::expire_due_approvals(
            &self.pool,
            &self.runs,
            &self.data,
            &self.fsms,
            at,
            Some(self.tenant.as_str()),
        )
        .await
        .unwrap();
    }
}

/// 判断の遷移と期限の回収が競っても、行と run の結論が分かれない（BUG-45）。
///
/// 1. 期限の前に受け付けた判断の遷移が終わる前に、期限の回収が来た: 回収は判断の受付を待ち、期限切れにせず
///    その判断で決める（行は承認・run も承認の出口）。
/// 2. 判断を受け付ける前に期限が過ぎた（判断できるかを確かめてから台帳の行を取るまでの間）: 遷移せずに 409
///    期限切れで断り、行は動かない（回収で時間切れの出口へ進む）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decisions_racing_the_deadline_keep_the_row_and_the_run_in_step() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.expense_table().await;
    let wf = env.enable(table, Some(3600)).await;
    let first = env.submit(table, "出張旅費", &env.bob).await;
    let second = env.submit(table, "会議費", &env.bob).await;
    env.settle().await;
    let later = chrono::Utc::now() + chrono::Duration::hours(2);
    // 2 件目の期限は 1 日先へずらす（1 件目の回収で一緒に期限切れにしない）。
    let second_run = env.run_of(wf, second).await;
    for sql in [
        "UPDATE workflow_approval SET deadline = deadline + interval '1 day' \
         WHERE tenant_id = $1 AND run_id = $2",
        "UPDATE wait_subscription SET timeout_at = timeout_at + interval '1 day' \
         WHERE tenant_id = $1 AND run_id = $2",
    ] {
        sqlx::query(sql)
            .bind(&env.tenant)
            .bind(second_run)
            .execute(&env.pool)
            .await
            .unwrap();
    }

    // 1. 遷移を止めている間に、期限の回収が来る。
    let item = env
        .item_for(&env.bob, first)
        .await
        .expect("bob の受信箱に出る");
    let id = item["approval_id"].as_str().unwrap().to_string();
    let rev = item["record"]["rev"].as_i64().unwrap();
    let (hold, xid) = env.hold_record(first).await;
    let deciding = tokio::spawn(
        env.app.clone().oneshot(req(
            "POST",
            &env.bob,
            &format!("/approvals/{id}/decision"),
            Body::from(
                json!({ "decision": "approve", "comment": "期限の直前", "expected_rev": rev })
                    .to_string(),
            ),
        )),
    );
    env.wait_blocked_on(&xid).await;
    let expiring = tokio::spawn({
        let (pool, runs, data, fsms, tenant) = (
            env.pool.clone(),
            env.runs.clone(),
            Arc::clone(&env.data),
            Arc::clone(&env.fsms),
            env.tenant.clone(),
        );
        async move {
            api::workflow_runtime::expire_due_approvals(
                &pool,
                &runs,
                &data,
                &fsms,
                later,
                Some(tenant.as_str()),
            )
            .await
            .unwrap();
        }
    });
    tokio::time::sleep(Duration::from_millis(1000)).await;
    hold.commit().await.unwrap();
    let resp = deciding.await.unwrap().unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    expiring.await.unwrap();
    env.settle().await;
    let run = env.run_of(wf, first).await;
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["approved".to_string()]),
        "行は承認へ進んだので、run も承認の出口へ"
    );
    let out = out.unwrap();
    assert_eq!(out["decided_by"], json!(env.bob.principal.id));
    assert_eq!(out["comment"], "期限の直前");
    assert_eq!(env.approval_of(run).await.1, "approved");
    assert_eq!(env.step(run, "late").await.0, "skipped");
    let row = env
        .data
        .get_record(&env.alice, table, first, None)
        .await
        .unwrap();
    assert_eq!(row.data["status"], "approved");
    assert_eq!(
        env.resolve_audit(Uuid::parse_str(&id).unwrap())
            .await
            .unwrap()["status"],
        "approved"
    );

    // 2. 判断できるかを確かめた後、台帳の行を取る前に期限が過ぎる。
    let item = env
        .item_for(&env.bob, second)
        .await
        .expect("bob の受信箱に出る");
    let id = item["approval_id"].as_str().unwrap().to_string();
    let rev = item["record"]["rev"].as_i64().unwrap();
    let mut ledger = env.pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE workflow_approval SET deadline = now() - interval '1 second' \
         WHERE tenant_id = $1 AND approval_id = $2",
    )
    .bind(&env.tenant)
    .bind(Uuid::parse_str(&id).unwrap())
    .execute(&mut *ledger)
    .await
    .unwrap();
    let xid: String =
        sqlx::query_scalar("SELECT (pg_current_xact_id()::text::bigint % 4294967296)::text")
            .fetch_one(&mut *ledger)
            .await
            .unwrap();
    let deciding = tokio::spawn(env.app.clone().oneshot(req(
        "POST",
        &env.bob,
        &format!("/approvals/{id}/decision"),
        Body::from(json!({ "decision": "approve", "expected_rev": rev }).to_string()),
    )));
    env.wait_blocked_on(&xid).await;
    ledger.commit().await.unwrap();
    let resp = deciding.await.unwrap().unwrap();
    let status = resp.status();
    let body: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("expired")),
        "{body}"
    );
    let row = env
        .data
        .get_record(&env.alice, table, second, None)
        .await
        .unwrap();
    assert_eq!(
        row.data["status"], "reviewing",
        "期限切れの判断では行を進めない"
    );
    env.expire_at(later + chrono::Duration::days(2)).await;
    env.settle().await;
    let run = env.run_of(wf, second).await;
    let (st, ports, _) = env.step(run, "ok").await;
    assert_eq!(
        (st.as_str(), ports),
        ("succeeded", vec!["timeout".to_string()])
    );
    assert_eq!(env.approval_of(run).await.1, "expired");
    env.forget_runs(wf).await;
    env.cleanup().await;
}
