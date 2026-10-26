//! データ変更トリガと wait(event) のデータ版の結合テスト（GAP-07・実 Postgres ＋実 OpenFGA ＋ S3）。
//!
//! 本番の relay（`api::workflow_runtime::relay_events`）・launcher・executor（`ProdNodePorts` の data 結線）で流す。
//!
//! - テーブルの行の追加・更新・状態変更で、有効化した人（委譲元）に見える変更だけでワークフローが起動する。
//!   run 入力は委譲元の見え方（読めない列は載らない）
//! - 見えない行・読めない列だけの変更・一括取込（作成・更新）では起動しない
//! - 自分の書込で自分を起こさない。workflow.start で起動した子の書込でも親を起こさない（因果の鎖）
//! - wait(event) は行に束縛でき、停止より前の自分の書込では起きず、後の変更で起きる（filter も効く）
//!
//! relay はテナントを問わず未配送のイベントを掴む。別の OpenFGA ストアを持つテストが互いのイベントを
//! 掴むと見え方が作れない（発火しないまま配送済みになる）ので、このファイルのテストは直列に流す。

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

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use data::{
    BulkImportOptions, BulkUpsertRow, CmpOp, DataStore, FieldDef, FieldPolicy, FieldType, FsmBody,
    FsmRef, FsmStore, FsmTransition, NewDataTable, PolicyExpr, PolicyOperand, RefResolver,
    RowPolicy, TableSchema,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{
    CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, GrantRequest, NodeExecutor,
    RegistrationService, RunStatus, RunStore, SchedulerStore, StepStatus, WorkerConfig,
    WorkflowRunLauncher, WorkflowStore,
};

/// このファイルのテストを直列にする（上のモジュールの説明）。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Users;

#[async_trait]
impl RefResolver for Users {
    async fn user_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(matches!(id, "alice" | "bob"))
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

struct Env {
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
    tenant: String,
    alice: AuthContext,
    bob: AuthContext,
}

fn user(tenant: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        tenant.into(),
        tenant.into(),
    )
}

async fn setup() -> Option<Env> {
    let (Ok(db), Ok(fga_url)) = (
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
        .connect(&db)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let http = reqwest::Client::new();
    let fga = Arc::new(
        OpenFgaClient::connect(
            http.clone(),
            &OpenFgaConfig {
                base_url: fga_url,
                store_name: format!("shiki-wf-data-trigger-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    ) as Arc<dyn AuthzClient>;
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
    });
    let executor: Arc<dyn NodeExecutor> = Arc::new(CapabilityNodeExecutor::new(
        ports,
        EffectJournal::new(pool.clone()),
        Arc::new(NoopAudit),
    ));
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let bob = user(&tenant, "bob");
    // alice はドライブのルートにフォルダを作れる（workflow.start の子の保存先などで使う）。
    fga.write_tuple(
        &alice.subject(),
        Relation::Member,
        &alice.ns().organization(&alice.org),
    )
    .await
    .unwrap();
    Some(Env {
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
        tenant,
        alice,
        bob,
    })
}

/// 申請の表。行は申請者（applicant）と作成者が読める。メモ（memo）は経理（role keiri）だけが読める。
fn requests_schema(fsm: Option<FsmRef>) -> TableSchema {
    let mut applicant = FieldDef::new("applicant", FieldType::UserRef);
    applicant.indexed = true;
    let mut status = FieldDef::new("status", FieldType::Select);
    status.options = vec!["draft".into(), "submitted".into(), "done".into()];
    status.indexed = true;
    TableSchema {
        fields: vec![
            FieldDef::new("title", FieldType::Text),
            FieldDef::new("amount", FieldType::Number),
            FieldDef::new("memo", FieldType::Text),
            applicant,
            status,
        ],
        status_field: Some("status".into()),
        row_policy: Some(RowPolicy {
            read: PolicyExpr::Any(vec![
                PolicyExpr::IsOwner,
                PolicyExpr::FieldCmp {
                    field: "applicant".into(),
                    op: CmpOp::Eq,
                    value: PolicyOperand::UserId,
                },
            ]),
            write: None,
        }),
        field_policy: vec![FieldPolicy {
            field: "memo".into(),
            readable_by: PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        }],
        aggregate_min_rows: None,
        fsm_ref: fsm,
        rag: None,
    }
}

impl Env {
    /// alice の申請の表（bob も editor・FSM は draft → submitted → done を作成者が進める）。
    async fn requests_table(&self) -> Uuid {
        let table = self
            .data
            .create_table(
                &self.alice,
                NewDataTable {
                    name: format!("requests-{}", Uuid::new_v4().simple()),
                    schema: requests_schema(None),
                },
                None,
            )
            .await
            .unwrap();
        let fsm = FsmBody {
            states: vec!["draft".into(), "submitted".into(), "done".into()],
            transitions: vec![
                FsmTransition {
                    from: "draft".into(),
                    to: "submitted".into(),
                    actor: PolicyExpr::IsOwner,
                },
                FsmTransition {
                    from: "submitted".into(),
                    to: "done".into(),
                    actor: PolicyExpr::IsOwner,
                },
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
                requests_schema(Some(FsmRef {
                    artifact_id: fsm_id,
                    version: 1,
                })),
                Some(1),
                None,
            )
            .await
            .unwrap();
        self.fga
            .write_tuple(
                &self.bob.subject(),
                Relation::Editor,
                &self.alice.ns().data_table(&table.id.to_string()),
            )
            .await
            .unwrap();
        table.id
    }

    /// ワークフローを保存し、alice がテーブルを委譲して有効化する（読取 viewer・書込 editor）。
    async fn enable(&self, ir: &Value, table: Uuid, extra: Vec<GrantRequest>) -> Uuid {
        let (wf, parsed) = self
            .workflows
            .create(&self.alice, ir, &Catalog::default(), None)
            .await
            .expect("保存");
        let object = self.alice.ns().data_table(&table.to_string());
        let mut grants = vec![GrantRequest {
            scope: "data.read".into(),
            object: object.clone(),
            relation: Relation::Viewer,
        }];
        if parsed.declared_scopes.iter().any(|s| s == "data.write") {
            grants.push(GrantRequest {
                scope: "data.write".into(),
                object,
                relation: Relation::Editor,
            });
        }
        grants.extend(extra);
        self.registration
            .enable(&self.alice, wf, 1, &parsed, &grants)
            .await
            .expect("有効化");
        wf
    }

    fn worker(&self) -> workflow_engine::WorkflowWorker {
        workflow_engine::WorkflowWorker::new(
            self.runs.clone(),
            Arc::clone(&self.executor),
            WorkerConfig::default(),
        )
        .scoped_to_tenant(&self.tenant)
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
        let worker = self.worker();
        while worker.claim_and_run_once("w1").await.unwrap() {}
    }

    /// テナントのデータ変更がすべて workflow へ配送され、run が進み切るまで relay とワーカーを回す。
    async fn settle(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
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
            if !pending {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "配送が終わらない");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// ワークフローの run の入力（作成順）。
    async fn run_inputs(&self, wf: Uuid) -> Vec<Value> {
        sqlx::query_scalar(
            "SELECT input FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2 \
             ORDER BY created_at, run_id",
        )
        .bind(&self.tenant)
        .bind(wf)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn step_output(&self, run: Uuid, node: &str) -> (String, Option<Value>) {
        sqlx::query_as(
            "SELECT status, output FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3",
        )
        .bind(&self.tenant)
        .bind(run)
        .bind(node)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
}

/// 何もしない 1 ノード（run の入力だけを確かめる）。
fn passthrough() -> Value {
    json!([{ "id": "noop", "type": "control.join", "params": {} }])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn data_change_triggers_fire_on_changes_the_delegator_can_see() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.requests_table().await;
    let t = table.to_string();
    // bob は経理（メモを読み書きできる）。委譲元の alice はメモを読めない。
    env.fga
        .write_tuple(
            &env.bob.subject(),
            Relation::Member,
            &env.bob.ns().role("keiri"),
        )
        .await
        .unwrap();
    let created = env
        .enable(
            &json!({
                "ir_version": 1, "name": "on-created",
                "triggers": [{ "kind": "event", "source": "data.record.created", "scope": { "table": t } }],
                "nodes": passthrough(), "edges": []
            }),
            table,
            vec![],
        )
        .await;
    let updated = env
        .enable(
            &json!({
                "ir_version": 1, "name": "on-updated",
                "triggers": [{ "kind": "event", "source": "data.record.updated", "scope": { "table": t },
                    "filter": { "cmp": { "left": { "$from": "trigger", "path": "/change" }, "op": "eq", "right": "update" } } }],
                "nodes": passthrough(), "edges": []
            }),
            table,
            vec![],
        )
        .await;
    let submitted = env
        .enable(
            &json!({
                "ir_version": 1, "name": "on-submitted",
                "triggers": [{ "kind": "event", "source": "data.transition", "scope": { "table": t },
                    "filter": { "cmp": { "left": { "$from": "trigger", "path": "/to" }, "op": "eq", "right": "submitted" } } }],
                "nodes": passthrough(), "edges": []
            }),
            table,
            vec![],
        )
        .await;

    // bob が alice 宛ての申請を作る（alice に見える）。bob だけの行は alice に見えない。
    let visible = env
        .data
        .create_record(
            &env.bob,
            table,
            json!({ "title": "備品の購入", "amount": 1200, "memo": "社外秘", "applicant": "alice", "status": "draft" }),
            None,
        )
        .await
        .unwrap();
    env.data
        .create_record(
            &env.bob,
            table,
            json!({ "title": "私用", "amount": 1, "applicant": "bob", "status": "draft" }),
            None,
        )
        .await
        .unwrap();
    env.settle().await;
    let inputs = env.run_inputs(created).await;
    assert_eq!(inputs.len(), 1, "見える行の作成だけで起動する: {inputs:?}");
    let input = &inputs[0];
    assert_eq!(input["source"], "data.record.created");
    assert_eq!(input["record_id"], visible.id.to_string());
    assert_eq!(input["actor"], "bob");
    assert_eq!(input["by_workflow"], false);
    assert_eq!(input["record"]["data"]["title"], "備品の購入");
    assert!(
        input["record"]["data"].get("memo").is_none(),
        "委譲元に読めない列は載らない: {input}"
    );
    assert!(!input["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("memo")));

    // 読めない列だけの更新では起動しない。見える列の更新で起動する（変わった列と前後の値）。
    let r = env
        .data
        .update_record(
            &env.bob,
            table,
            visible.id,
            json!({ "memo": "差し替え" }),
            1,
            None,
        )
        .await
        .unwrap();
    let r = env
        .data
        .update_record(
            &env.bob,
            table,
            visible.id,
            json!({ "amount": 1500 }),
            r.rev,
            None,
        )
        .await
        .unwrap();
    env.settle().await;
    let inputs = env.run_inputs(updated).await;
    assert_eq!(inputs.len(), 1, "{inputs:?}");
    assert_eq!(inputs[0]["changed"], json!(["amount"]));
    assert_eq!(
        inputs[0]["changes"]["amount"],
        json!({ "old": 1200, "new": 1500 })
    );

    // 状態の変更（filter で to=submitted だけ）。遷移は行の更新でもあるが、filter で除いている。
    let s = env
        .fsms
        .transition(&env.bob, table, visible.id, "submitted", r.rev, None)
        .await
        .unwrap();
    env.fsms
        .transition(&env.bob, table, visible.id, "done", s.rev, None)
        .await
        .unwrap();
    env.settle().await;
    let inputs = env.run_inputs(submitted).await;
    assert_eq!(inputs.len(), 1, "{inputs:?}");
    assert_eq!(inputs[0]["from"], "draft");
    assert_eq!(inputs[0]["to"], "submitted");
    assert_eq!(
        env.run_inputs(updated).await.len(),
        1,
        "遷移は change=transition"
    );

    // 一括取込（作成・更新）では起動しない。
    let import = env
        .data
        .start_bulk_upsert(
            &env.alice,
            table,
            vec![
                BulkUpsertRow {
                    key: Some("n1".into()),
                    record_id: None,
                    expected_rev: None,
                    data: json!({ "title": "一括", "applicant": "alice", "status": "draft" }),
                },
                BulkUpsertRow {
                    key: None,
                    record_id: Some(visible.id),
                    expected_rev: None,
                    data: json!({ "title": "一括で更新" }),
                },
            ],
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    sqlx::query("DELETE FROM job_queue WHERE queue = $1 AND payload->>'import_id' = $2")
        .bind(data::BULK_IMPORT_QUEUE)
        .bind(import.to_string())
        .execute(&env.pool)
        .await
        .unwrap();
    let step = env
        .data
        .run_bulk_import(&env.tenant, table, import)
        .await
        .unwrap();
    assert_eq!(step.status, "done");
    env.settle().await;
    assert_eq!(
        env.run_inputs(created).await.len(),
        1,
        "一括の作成では起動しない"
    );
    assert_eq!(
        env.run_inputs(updated).await.len(),
        1,
        "一括の更新では起動しない"
    );

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_run WHERE tenant_id = $1")
        .bind(&env.tenant)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(total, 3, "作成・更新・状態の変更で 1 回ずつ");

    // 読めないテーブルの変更は購読できない（有効化者がテーブルの viewer でなければ有効化を拒む）。
    let carol = user(&env.tenant, "carol");
    let (spy, parsed) = env
        .workflows
        .create(
            &carol,
            &json!({
                "ir_version": 1, "name": "carol-watch",
                "triggers": [{ "kind": "event", "source": "data.record.created", "scope": { "table": t } }],
                "nodes": passthrough(), "edges": []
            }),
            &Catalog::default(),
            None,
        )
        .await
        .expect("保存はできる（テーブルの実在は見ない）");
    let refused = env.registration.enable(&carol, spy, 1, &parsed, &[]).await;
    assert!(
        matches!(
            refused,
            Err(workflow_engine::EnableError::Delegation(
                workflow_engine::DelegationError::OutOfScope(_)
            ))
        ),
        "{refused:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn own_writes_and_started_children_do_not_retrigger() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.requests_table().await;
    let t = table.to_string();
    // 子（B）: 渡された行のタイトルに「!」を足す。
    let (child, _) = env
        .workflows
        .create(
            &env.alice,
            &json!({
                "ir_version": 1, "name": "append-mark",
                "triggers": [{ "kind": "interactive" }],
                "nodes": [{ "id": "u", "type": "data.record.update", "params": {
                    "table": t,
                    "record": { "$from": "input", "path": "/id" },
                    "patch": { "title": { "$template": "{t}!", "vars": { "t": { "$from": "input", "path": "/title" } } } } } }],
                "edges": []
            }),
            &Catalog::default(),
            None,
        )
        .await
        .expect("子を保存");
    // A: 行が更新されたら、自分でタイトルを書き換え（自分の書込）、子 B も起動する（子の書込）。
    let parent = env
        .enable(
            &json!({
                "ir_version": 1, "name": "on-update-mark",
                "triggers": [{ "kind": "event", "source": "data.record.updated", "scope": { "table": t } }],
                "nodes": [
                    { "id": "own", "type": "data.record.update", "params": {
                        "table": t,
                        "record": { "$from": "trigger", "path": "/record_id" },
                        "patch": { "amount": { "$from": "trigger", "path": "/rev" } } } },
                    { "id": "kick", "type": "workflow.start", "params": {
                        "name": "append-mark",
                        "input": { "id": { "$from": "trigger", "path": "/record_id" },
                                   "title": { "$from": "trigger", "path": "/record/data/title" } } } }
                ],
                "edges": [{ "from": "own", "to": "kick" }]
            }),
            table,
            vec![GrantRequest {
                scope: "workflow.start".into(),
                object: env.alice.ns().artifact(&child.to_string()),
                relation: Relation::Viewer,
            }],
        )
        .await;
    let rec = env
        .data
        .create_record(
            &env.bob,
            table,
            json!({ "title": "出張", "amount": 0, "applicant": "alice", "status": "draft" }),
            None,
        )
        .await
        .unwrap();
    env.data
        .update_record(
            &env.bob,
            table,
            rec.id,
            json!({ "title": "出張（大阪）" }),
            1,
            None,
        )
        .await
        .unwrap();
    // 数回まわしても A の run は 1 つ（自分の書込・子の書込で A は起きない）。
    for _ in 0..3 {
        env.settle().await;
    }
    let runs = env.run_inputs(parent).await;
    assert_eq!(runs.len(), 1, "人の変更 1 回につき 1 回だけ: {runs:?}");
    let children: Vec<(Uuid, Vec<Uuid>, String)> = sqlx::query_as(
        "SELECT run_id, trigger_chain, status FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&env.tenant)
    .bind(child)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(children.len(), 1, "{children:?}");
    assert_eq!(children[0].1, vec![parent], "子は親の鎖を引き継ぐ");
    assert_eq!(children[0].2, "succeeded");
    let now = env
        .data
        .get_record(&env.alice, table, rec.id, None)
        .await
        .unwrap();
    assert_eq!(now.data["title"], "出張（大阪）!", "子の書込は 1 回だけ");
    // 抑止は監査に残る（循環）。
    let suppressed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'workflow.trigger.suppressed' \
           AND metadata->>'reason' = 'suppressed_cycle'",
    )
    .bind(&env.tenant)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(suppressed >= 2, "自分の書込と子の書込の 2 回: {suppressed}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn data_wait_wakes_only_on_later_changes_of_the_bound_row() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let table = env.requests_table().await;
    let t = table.to_string();
    let (wf, _) = env
        .workflows
        .create(
            &env.alice,
            &json!({
                "ir_version": 1, "name": "wait-for-title",
                "triggers": [{ "kind": "interactive" }],
                "nodes": [
                    { "id": "make", "type": "data.record.create", "params": {
                        "table": t, "data": { "title": "下書き", "applicant": "alice", "status": "draft" } } },
                    { "id": "touch", "type": "data.record.update", "params": {
                        "table": t, "record": { "$from": "nodes.make.output", "path": "/id" },
                        "patch": { "title": "下書き（2）" } } },
                    { "id": "w", "type": "control.wait", "params": {
                        "kind": "event", "source": "data.record.updated",
                        "scope": { "table": t, "record": { "$from": "nodes.make.output", "path": "/id" } },
                        "filter": { "cmp": { "left": { "$from": "event", "path": "/changed" }, "op": "eq", "right": "title" } },
                        "timeout_sec": 600 } }
                ],
                "edges": [{ "from": "make", "to": "touch" }, { "from": "touch", "to": "w" }]
            }),
            &Catalog::default(),
            None,
        )
        .await
        .expect("保存");
    let run = env
        .launcher
        .start_interactive(&env.alice, wf, &json!({}))
        .await
        .unwrap()
        .unwrap();
    env.settle().await;
    let status = |s: &str| s.to_string();
    let (w, _) = env.step_output(run, "w").await;
    assert_eq!(
        w,
        status("waiting_event"),
        "停止より前の自分の更新では起きない"
    );
    let (_, made) = env.step_output(run, "make").await;
    let id = Uuid::parse_str(made.unwrap()["id"].as_str().unwrap()).unwrap();

    // filter に合わない変更（金額だけ）では起きない。別の行の変更でも起きない。
    let r = env
        .data
        .update_record(&env.alice, table, id, json!({ "amount": 10 }), 2, None)
        .await
        .unwrap();
    let other = env
        .data
        .create_record(
            &env.alice,
            table,
            json!({ "title": "別", "applicant": "alice", "status": "draft" }),
            None,
        )
        .await
        .unwrap();
    env.data
        .update_record(
            &env.alice,
            table,
            other.id,
            json!({ "title": "別（2）" }),
            1,
            None,
        )
        .await
        .unwrap();
    env.settle().await;
    assert_eq!(env.step_output(run, "w").await.0, status("waiting_event"));

    // 束縛した行のタイトルの変更で起き、変更の内容が出力に渡る。
    env.data
        .update_record(
            &env.alice,
            table,
            id,
            json!({ "title": "確定" }),
            r.rev,
            None,
        )
        .await
        .unwrap();
    env.settle().await;
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    let (w, out) = env.step_output(run, "w").await;
    assert_eq!(w, status(StepStatus::Succeeded.as_str()));
    let out = out.unwrap();
    assert_eq!(out["record_id"], id.to_string());
    assert_eq!(out["actor"], "alice");
    assert_eq!(
        out["changes"]["title"],
        json!({ "old": "下書き（2）", "new": "確定" })
    );
}

/// relay の処理時間（設計 GAP-07 の性能の受け入れ: 500 件のバッチ・候補のトリガが 0 / 1 / 10）。
///
/// `WORKFLOW_RELAY_BENCH=1` のときだけ走る（CI では流さない・数字は報告に書く）。filter は一致しない条件にして、
/// 見え方の作成と filter の評価までの費用を測る（run の作成は含めない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_cost_for_a_batch_of_500_changes() {
    if std::env::var("WORKFLOW_RELAY_BENCH").as_deref() != Ok("1") {
        eprintln!("WORKFLOW_RELAY_BENCH=1 でないためスキップ");
        return;
    }
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    for triggers in [0usize, 1, 10] {
        let table = env.requests_table().await;
        let t = table.to_string();
        for i in 0..triggers {
            env.enable(
                &json!({
                    "ir_version": 1, "name": format!("bench-{triggers}-{i}"),
                    "triggers": [{ "kind": "event", "source": "data.record.created", "scope": { "table": t },
                        "filter": { "cmp": { "left": { "$from": "trigger", "path": "/record/data/title" }, "op": "eq", "right": "一致しない" } } }],
                    "nodes": passthrough(), "edges": []
                }),
                table,
                vec![],
            )
            .await;
        }
        // 先に溜まっている配送を流しておく（測るのはこのテーブルの 500 件だけ）。
        env.settle().await;
        for i in 0..500 {
            env.data
                .create_record(
                    &env.bob,
                    table,
                    json!({ "title": format!("行 {i}"), "applicant": "alice", "status": "draft" }),
                    None,
                )
                .await
                .unwrap();
        }
        let started = std::time::Instant::now();
        env.settle().await;
        let elapsed = started.elapsed();
        eprintln!("relay: 500 件・候補のトリガ {triggers} 本 → {elapsed:?}");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workflow_run WHERE tenant_id = $1")
                .bind(&env.tenant)
                .fetch_one(&env.pool)
                .await
                .unwrap(),
            0
        );
    }
}
