//! ワークフローの data ノード（Task 15.7）の結合テスト（実 Postgres ＋実 OpenFGA ＋ moto S3）。
//!
//! 受け入れ条件: **スケジュールトリガ**のワークフローが `data.query` → `data.record.update` を実行でき、
//! **委譲元ユーザーの権限で**行が絞られる。
//!
//! - 本番の executor（`CapabilityNodeExecutor` ＋ `ProdNodePorts` の data 結線）・本番の launcher
//!   （委譲チェック → workflow プリンシパルの run）・本番のスケジューラ（`tick_schedules`）で流す。
//! - 行述語は「担当者本人だけが読み書きできる」。委譲元は alice。bob 担当の行はワークフローから見えず、
//!   更新もされない。script の `Shiki.data.query` も同じ権限で絞られる。
//! - 委譲タプルを撤去すると次の run の data ノードは受動的に拒否される（engine.md §6.4）。

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
use chrono::Utc;
use data::{
    CmpOp, DataStore, FieldType, FsmStore, NewDataTable, PolicyExpr, PolicyOperand, RefResolver,
    RowPolicy, TableSchema,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{
    CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, GrantRequest, NodeExecutor,
    RegistrationService, RunStatus, RunStore, SchedulerStore, WorkerConfig, WorkflowIr,
    WorkflowRunLauncher, WorkflowStore,
};

struct Users;

#[async_trait]
impl RefResolver for Users {
    async fn user_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(matches!(id, "alice" | "bob"))
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

struct NoopAudit;
impl workflow_engine::CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
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

struct Env {
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    data: Arc<DataStore>,
    executor: Arc<dyn NodeExecutor>,
    workflows: WorkflowStore,
    runs: RunStore,
    launcher: WorkflowRunLauncher,
    registration: RegistrationService,
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
                store_name: format!("shiki-wf-data-{}", Uuid::new_v4()),
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
        storage,
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
        fsms: Some(fsms),
    });
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let executor: Arc<dyn NodeExecutor> = Arc::new(
        CapabilityNodeExecutor::new(ports, EffectJournal::new(pool.clone()), Arc::new(NoopAudit))
            .with_script_engine(engine, script_runtime::engine::Limits::default()),
    );
    Some(Env {
        pool,
        fga,
        data,
        executor,
        workflows,
        runs,
        launcher,
        registration,
    })
}

impl Env {
    /// スケジュールを 1 回発火させ、ワーカーで完走させる。作られた run の id を返す。
    async fn fire_schedule_and_run(&self, tenant: &str, wf: Uuid, at_secs: i64) -> Uuid {
        let sched = SchedulerStore::new(self.pool.clone());
        let fired = sched
            .tick_schedules(
                Utc::now() + chrono::Duration::seconds(at_secs),
                Some(tenant),
                &self.launcher,
            )
            .await
            .unwrap();
        assert!(fired >= 1, "schedule が発火する（fired={fired}）");
        let worker = workflow_engine::WorkflowWorker::new(
            self.runs.clone(),
            Arc::clone(&self.executor),
            WorkerConfig::default(),
        )
        .scoped_to_tenant(tenant);
        while worker.claim_and_run_once("w1").await.unwrap() {}
        sqlx::query_scalar(
            "SELECT run_id FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(tenant)
        .bind(wf)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn step(
        &self,
        tenant: &str,
        run: Uuid,
        node: &str,
    ) -> (String, Option<Value>, Option<Value>) {
        sqlx::query_as(
            "SELECT status, output, error FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3",
        )
        .bind(tenant)
        .bind(run)
        .bind(node)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
}

fn expenses_schema() -> TableSchema {
    let mut title = data::FieldDef::new("title", FieldType::Text);
    title.indexed = true;
    let mut amount = data::FieldDef::new("amount", FieldType::Number);
    amount.indexed = true;
    let mut assignee = data::FieldDef::new("assignee", FieldType::UserRef);
    assignee.indexed = true;
    let mut status = data::FieldDef::new("status", FieldType::Select);
    status.indexed = true;
    status.options = vec!["pending".into(), "approved".into()];
    TableSchema {
        fields: vec![title, amount, assignee, status],
        status_field: None,
        // 担当者本人だけが読み書きできる（委譲元の行述語で絞られることを見る）。
        row_policy: Some(RowPolicy {
            read: PolicyExpr::FieldCmp {
                field: "assignee".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            write: None,
        }),
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_workflow_queries_and_updates_with_the_delegators_permissions() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let bob = user(&tenant, "bob");

    // alice のテーブル。bob にも editor を付ける（bob の行は bob の担当）。
    let table = env
        .data
        .create_table(
            &alice,
            NewDataTable {
                name: "expenses".into(),
                schema: expenses_schema(),
            },
            None,
        )
        .await
        .unwrap();
    let table_obj = alice.ns().data_table(&table.id.to_string());
    env.fga
        .write_tuple(&bob.subject(), Relation::Editor, &table_obj)
        .await
        .unwrap();
    let mk = |t: &str, a: i64, who: &str| json!({ "title": t, "amount": a, "assignee": who, "status": "pending" });
    let a_small = env
        .data
        .create_record(&alice, table.id, mk("taxi", 1200, "alice"), None)
        .await
        .unwrap();
    let a_big = env
        .data
        .create_record(&alice, table.id, mk("hotel", 30000, "alice"), None)
        .await
        .unwrap();
    // bob の行は金額が最大（alice の権限で絞られなければ、これが先頭になって更新される）。
    let b_row = env
        .data
        .create_record(&bob, table.id, mk("flight", 90000, "bob"), None)
        .await
        .unwrap();

    // スケジュールトリガ: 承認待ちを金額順に引き、先頭を承認する。script でも同じ表を数える。
    let t = table.id.to_string();
    let ir = json!({
        "ir_version": 1, "name": "approve-top-expense",
        "declared_scopes": ["data.read", "data.write"],
        "triggers": [ { "kind": "schedule", "cron": "* * * * *", "tz": "UTC" } ],
        "nodes": [
            { "id": "q", "type": "data.query", "params": {
                "table": t,
                "query": {
                    "filter": { "cmp": { "field": "status", "op": "eq", "value": "pending" } },
                    "sort": [ { "field": "amount", "descending": true } ],
                    "limit": 10 } } },
            { "id": "u", "type": "data.record.update", "params": {
                "table": t,
                "record": { "$from": "nodes.q.output", "path": "/items/0/id" },
                "patch": { "status": "approved" },
                "expected_rev": { "$from": "nodes.q.output", "path": "/items/0/rev" } } },
            { "id": "sc", "type": "script.run", "params": {
                "source": { "inline": "function main(input) { const r = Shiki.data.query(input.table, { filter: { cmp: { field: \"status\", op: \"eq\", value: \"approved\" } } }); return { approved: r.items.map(x => x.data.title) }; }" },
                "input": { "table": t } } }
        ],
        "edges": [ { "from": "q", "to": "u" }, { "from": "u", "to": "sc" } ]
    });
    let (wf, _) = env
        .workflows
        .create(&alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存（data ノード・data スコープは保存できる）");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    // 有効化: alice がテーブルを委譲する（読取 viewer・書込 editor）。
    env.registration
        .enable(
            &alice,
            wf,
            1,
            &parsed,
            &[
                GrantRequest {
                    scope: "data.read".into(),
                    object: table_obj.clone(),
                    relation: Relation::Viewer,
                },
                GrantRequest {
                    scope: "data.write".into(),
                    object: table_obj.clone(),
                    relation: Relation::Editor,
                },
            ],
        )
        .await
        .expect("enable");

    let run = env.fire_schedule_and_run(&tenant, wf, 61).await;
    let principal: (String, String) = sqlx::query_as(
        "SELECT trigger_kind, principal_kind FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(principal, ("schedule".into(), "workflow".into()));
    assert_eq!(
        env.runs.run_status(&tenant, run).await.unwrap().unwrap(),
        RunStatus::Succeeded,
        "q: {:?} / u: {:?} / sc: {:?}",
        env.step(&tenant, run, "q").await,
        env.step(&tenant, run, "u").await,
        env.step(&tenant, run, "sc").await,
    );

    // data.query: 委譲元（alice）の行述語で絞られる（bob 担当の行は見えない）。
    let (_, q_out, _) = env.step(&tenant, run, "q").await;
    let q_out = q_out.unwrap();
    let ids: Vec<String> = q_out["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        vec![a_big.id.to_string(), a_small.id.to_string()],
        "alice の行だけが金額順に返る（bob の 90,000 円は見えない）"
    );

    // data.record.update: 見えた先頭（alice の hotel）だけが承認される。
    let a_big_now = env
        .data
        .get_record(&alice, table.id, a_big.id, None)
        .await
        .unwrap();
    assert_eq!(a_big_now.data["status"], "approved");
    assert_eq!(a_big_now.rev, 2);
    let b_now = env
        .data
        .get_record(&bob, table.id, b_row.id, None)
        .await
        .unwrap();
    assert_eq!(b_now.data["status"], "pending", "bob の行は触られない");
    assert_eq!(b_now.rev, 1);
    // 書込の主体は委譲元（監査・リビジョンに alice が残る）。
    let actor: String = sqlx::query_scalar(
        "SELECT actor FROM audit_log WHERE tenant_id = $1 AND action = 'data.record.update' \
           AND object_id = $2",
    )
    .bind(&tenant)
    .bind(a_big.id.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(actor, "alice");

    // script の Shiki.data.query も同じ権限で絞られる。
    let (_, sc_out, _) = env.step(&tenant, run, "sc").await;
    assert_eq!(sc_out.unwrap()["approved"], json!(["hotel"]));

    // 委譲タプルを撤去すると（棚卸しの撤去と同じ）、次の run の data ノードは受動的に拒否される。
    let wf_subject = alice.ns().workflow_principal(&wf.to_string());
    for rel in [Relation::Viewer, Relation::Editor] {
        env.fga
            .delete_tuple(&wf_subject, rel, &table_obj)
            .await
            .unwrap();
    }
    let run2 = env.fire_schedule_and_run(&tenant, wf, 181).await;
    assert_ne!(run2, run);
    let (status, _, err) = env.step(&tenant, run2, "q").await;
    assert_eq!(status, "failed", "{err:?}");
    // テーブルが見えなくなった主体には、存在しないのと同じ not_found（data-platform §16.1）。
    assert_eq!(err.unwrap()["code"], "not_found");
    let still = env
        .data
        .get_record(&alice, table.id, a_small.id, None)
        .await
        .unwrap();
    assert_eq!(still.rev, 1, "拒否された run は何も書かない");
}

/// R3-2（confused-deputy）: 書込は**書込を委譲した**委譲元の権限でだけ走る。
///
/// bob が editor（data.write）でテーブルを委譲した後、alice（editor を持つ）が viewer（data.read）だけで
/// 再有効化する。台帳には両方が残り、ワークフロー主体には bob の editor タプルが残る。書込は最新の
/// alice ではなく bob の権限で走る（alice は書込を委譲していない）。bob の委譲が台帳から外れると、
/// タプルが残っていても書込は拒否される（fail-closed）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_run_only_as_a_delegator_who_delegated_write() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let bob = user(&tenant, "bob");
    let mut schema = expenses_schema();
    schema.row_policy = None;
    let table = env
        .data
        .create_table(
            &alice,
            NewDataTable {
                name: "expenses".into(),
                schema,
            },
            None,
        )
        .await
        .unwrap();
    let table_obj = alice.ns().data_table(&table.id.to_string());
    env.fga
        .write_tuple(&bob.subject(), Relation::Editor, &table_obj)
        .await
        .unwrap();
    let rec = env
        .data
        .create_record(
            &alice,
            table.id,
            json!({ "title": "taxi", "amount": 1200, "assignee": "alice", "status": "pending" }),
            None,
        )
        .await
        .unwrap();

    let t = table.id.to_string();
    let ir = json!({
        "ir_version": 1, "name": "approve-one",
        "declared_scopes": ["data.read", "data.write"],
        "triggers": [ { "kind": "schedule", "cron": "* * * * *", "tz": "UTC" } ],
        "nodes": [
            { "id": "u", "type": "data.record.update", "params": {
                "table": t, "record": rec.id.to_string(), "patch": { "status": "approved" } } }
        ],
        "edges": []
    });
    let (wf, _) = env
        .workflows
        .create(&alice, &ir, &Catalog::default(), None)
        .await
        .unwrap();
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    // bob が書込（editor）を委譲する。
    env.registration
        .enable(
            &bob,
            wf,
            1,
            &parsed,
            &[GrantRequest {
                scope: "data.write".into(),
                object: table_obj.clone(),
                relation: Relation::Editor,
            }],
        )
        .await
        .expect("bob の有効化");
    // alice が読取（viewer）だけで再有効化する（alice は editor を持つが書込は委譲しない）。
    env.registration
        .enable(
            &alice,
            wf,
            1,
            &parsed,
            &[GrantRequest {
                scope: "data.read".into(),
                object: table_obj.clone(),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect("alice の再有効化");

    let run = env.fire_schedule_and_run(&tenant, wf, 61).await;
    let (status, _, err) = env.step(&tenant, run, "u").await;
    assert_eq!(status, "succeeded", "{err:?}");
    // 書込の主体は書込を委譲した bob（最新の委譲元 alice ではない）。
    let actors: Vec<String> = sqlx::query_scalar(
        "SELECT actor FROM audit_log WHERE tenant_id = $1 AND action = 'data.record.update' \
           AND object_id = $2",
    )
    .bind(&tenant)
    .bind(rec.id.to_string())
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(actors, vec!["bob".to_string()]);

    // bob の委譲が台帳から外れる（タプルは残ったまま）→ 書込を委譲した者がいないので拒否。
    sqlx::query(
        "UPDATE workflow_delegation SET revoked_at = now() \
         WHERE tenant_id = $1 AND workflow_id = $2 AND delegator = 'bob'",
    )
    .bind(&tenant)
    .bind(wf)
    .execute(&env.pool)
    .await
    .unwrap();
    let before = env
        .data
        .get_record(&alice, table.id, rec.id, None)
        .await
        .unwrap();
    let run2 = env.fire_schedule_and_run(&tenant, wf, 181).await;
    assert_ne!(run2, run);
    let (status, _, err) = env.step(&tenant, run2, "u").await;
    assert_eq!(status, "failed", "{err:?}");
    assert_eq!(err.unwrap()["code"], "forbidden");
    let after = env
        .data
        .get_record(&alice, table.id, rec.id, None)
        .await
        .unwrap();
    assert_eq!(after.rev, before.rev, "拒否された run は何も書かない");
}

fn plain_schema(fields: Vec<data::FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

/// C3（engine.md §6.8）: 委譲したテーブルから、委譲元だけが読める**未委譲の**テーブルへリンクで辿れない。
///
/// alice が注文（A）だけを委譲し、顧客（B）は委譲しない。A のリンク列で B を結合・半結合しても 403、
/// B を引く lookup は null、リンク値（B の id）は落ちる（委譲元が B を読めないのと同じ応答）。書込を
/// 委譲した bob の権限で走る更新は、alice が委譲した B を辿らない（委譲元本人が委譲したテーブルだけ）。
/// alice が B も委譲すると辿れる。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workflow_cannot_follow_links_into_undelegated_tables() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let bob = user(&tenant, "bob");

    let mut name = data::FieldDef::new("name", FieldType::Text);
    name.indexed = true;
    let customers = env
        .data
        .create_table(
            &alice,
            NewDataTable {
                name: "customers".into(),
                schema: plain_schema(vec![name]),
            },
            None,
        )
        .await
        .unwrap();
    let mut title = data::FieldDef::new("title", FieldType::Text);
    title.indexed = true;
    let mut customer = data::FieldDef::new("customer", FieldType::RecordRef);
    customer.ref_table = Some(customers.id);
    customer.link = Some(data::LinkDef {
        cardinality: data::LinkCardinality::ManyToOne,
        symmetric_field: None,
        symmetric_key: None,
        symmetric_display_name: None,
        reverse: false,
    });
    let mut customer_name = data::FieldDef::new("customer_name", FieldType::Lookup);
    customer_name.lookup = Some(data::LookupDef {
        via_field: "customer".into(),
        target_field: "name".into(),
    });
    let orders = env
        .data
        .create_table(
            &alice,
            NewDataTable {
                name: "orders".into(),
                schema: plain_schema(vec![title, customer, customer_name]),
            },
            None,
        )
        .await
        .unwrap();
    let acme = env
        .data
        .create_record(&alice, customers.id, json!({ "name": "Acme" }), None)
        .await
        .unwrap();
    let order = env
        .data
        .create_record(
            &alice,
            orders.id,
            json!({ "title": "o1", "customer": [acme.id.to_string()] }),
            None,
        )
        .await
        .unwrap();
    // 委譲元の alice 本人は lookup で B の値を読める（天井の無い読取は従来どおり）。
    let own = env
        .data
        .get_record(&alice, orders.id, order.id, None)
        .await
        .unwrap();
    assert_eq!(own.data["customer_name"], json!("Acme"), "{:?}", own.data);
    let orders_obj = alice.ns().data_table(&orders.id.to_string());
    let customers_obj = alice.ns().data_table(&customers.id.to_string());
    env.fga
        .write_tuple(&bob.subject(), Relation::Viewer, &customers_obj)
        .await
        .unwrap();

    let o = orders.id.to_string();
    let mk = |name: &str, query: Value| {
        json!({
            "ir_version": 1, "name": name,
            "declared_scopes": ["data.read"],
            "triggers": [ { "kind": "schedule", "cron": "* * * * *", "tz": "UTC" } ],
            "nodes": [ { "id": "q", "type": "data.query", "params": { "table": o, "query": query } } ],
            "edges": []
        })
    };
    let irs = [
        ("plain", mk("plain", json!({}))),
        (
            "semi",
            mk(
                "semi",
                json!({ "filter": { "link": { "field": "customer",
                    "filter": { "cmp": { "field": "name", "op": "eq", "value": "Acme" } } } } }),
            ),
        ),
        (
            "join",
            mk(
                "join",
                json!({ "joins": [ { "alias": "c", "field": "customer" } ],
                        "select": ["title", "c.name"] }),
            ),
        ),
    ];
    let read = |obj: &authz::FgaObject| GrantRequest {
        scope: "data.read".into(),
        object: obj.clone(),
        relation: Relation::Viewer,
    };
    let mut wfs = Vec::new();
    for (label, ir) in &irs {
        let (wf, _) = env
            .workflows
            .create(&alice, ir, &Catalog::default(), None)
            .await
            .unwrap();
        let parsed = WorkflowIr::from_json(ir).unwrap();
        // alice は注文（A）だけを委譲する。
        env.registration
            .enable(&alice, wf, 1, &parsed, &[read(&orders_obj)])
            .await
            .expect("enable");
        wfs.push((*label, wf, parsed));
    }
    let latest_q = |wf: Uuid| {
        let env = &env;
        let tenant = tenant.clone();
        async move {
            let run: Uuid = sqlx::query_scalar(
                "SELECT run_id FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2 \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(&tenant)
            .bind(wf)
            .fetch_one(&env.pool)
            .await
            .unwrap();
            env.step(&tenant, run, "q").await
        }
    };
    env.fire_schedule_and_run(&tenant, wfs[0].1, 61).await;

    // 未委譲の B を引く lookup は null・リンク値（B の id）は落ちる。A の行そのものは読める。
    let (status, out, err) = latest_q(wfs[0].1).await;
    assert_eq!(status, "succeeded", "{err:?}");
    let items = out.unwrap()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["data"]["title"], "o1");
    assert!(
        items[0]["data"]["customer_name"].is_null(),
        "lookup は B の値を返さない: {:?}",
        items[0]
    );
    let linked = items[0]["data"]["customer"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(linked.is_empty(), "B の id は落ちる: {:?}", items[0]);
    // 半結合・結合は、委譲元が B を読めないときと同じ 403。
    for i in [1, 2] {
        let (status, _, err) = latest_q(wfs[i].1).await;
        assert_eq!(status, "failed", "{}: {err:?}", wfs[i].0);
        assert_eq!(err.unwrap()["code"], "forbidden", "{}", wfs[i].0);
    }

    // 委譲元本人が委譲したテーブルだけを辿る（confused-deputy）: 書込を委譲した bob の権限で走る更新は、
    // alice が委譲した B を bob の権限では辿らない（bob は B を読めるが委譲していない）。
    env.fga
        .write_tuple(&bob.subject(), Relation::Editor, &orders_obj)
        .await
        .unwrap();
    let w_ir = json!({
        "ir_version": 1, "name": "rename",
        "declared_scopes": ["data.read", "data.write"],
        "triggers": [ { "kind": "schedule", "cron": "* * * * *", "tz": "UTC" } ],
        "nodes": [ { "id": "q", "type": "data.record.update", "params": {
            "table": o, "record": order.id.to_string(), "patch": { "title": "o1x" } } } ],
        "edges": []
    });
    let (w_wf, _) = env
        .workflows
        .create(&alice, &w_ir, &Catalog::default(), None)
        .await
        .unwrap();
    let w_parsed = WorkflowIr::from_json(&w_ir).unwrap();
    env.registration
        .enable(
            &bob,
            w_wf,
            1,
            &w_parsed,
            &[GrantRequest {
                scope: "data.write".into(),
                object: orders_obj.clone(),
                relation: Relation::Editor,
            }],
        )
        .await
        .expect("bob の有効化（A の書込）");
    env.registration
        .enable(
            &alice,
            w_wf,
            1,
            &w_parsed,
            &[read(&orders_obj), read(&customers_obj)],
        )
        .await
        .expect("alice の有効化（A・B の読取）");
    env.fire_schedule_and_run(&tenant, w_wf, 181).await;
    let (status, out, err) = latest_q(w_wf).await;
    assert_eq!(status, "succeeded", "{err:?}");
    let out = out.unwrap();
    assert_eq!(out["data"]["title"], "o1x", "{out}");
    assert!(
        out["data"]["customer_name"].is_null(),
        "bob の権限の書込は alice が委譲した B を辿らない: {out}"
    );
    let actor: String = sqlx::query_scalar(
        "SELECT actor FROM audit_log WHERE tenant_id = $1 AND action = 'data.record.update' \
           AND object_id = $2",
    )
    .bind(&tenant)
    .bind(order.id.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(actor, "bob");

    // alice が B も委譲すると、結合で B の列を読める（天井は委譲したテーブルを通す）。
    let (_, join_wf, join_ir) = &wfs[2];
    env.registration
        .enable(
            &alice,
            *join_wf,
            1,
            join_ir,
            &[read(&orders_obj), read(&customers_obj)],
        )
        .await
        .expect("alice の再有効化");
    env.fire_schedule_and_run(&tenant, *join_wf, 301).await;
    let (status, out, err) = latest_q(*join_wf).await;
    assert_eq!(status, "succeeded", "{err:?}");
    let out = out.unwrap();
    assert!(out.to_string().contains("Acme"), "{out}");
}

/// 受け入れ条件: 配列値に eq / neq / in / contains（と starts_with / ends_with）を使う保存済みワークフローが切替前に一覧化され、
/// 保存時に警告が出る（`shiki-admin workflow-conditions` の本体と保存 API が同じ検出を使う）。
#[tokio::test]
async fn saved_workflows_with_array_conditions_are_listed() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let flagged = json!({
        "ir_version": 1, "name": "tags-branch",
        "input_schema": { "type": "object", "properties": {
            "tags": { "type": "array", "items": { "type": "string" } } } },
        "triggers": [ { "kind": "interactive" } ],
        "nodes": [ { "id": "br", "type": "control.branch", "params": { "condition":
            { "cmp": { "left": { "$from": "input", "path": "/tags" }, "op": "eq", "right": "urgent" } } } } ],
        "edges": []
    });
    let plain = json!({
        "ir_version": 1, "name": "plain-branch",
        "triggers": [ { "kind": "interactive" } ],
        "nodes": [ { "id": "br", "type": "control.branch", "params": { "condition":
            { "cmp": { "left": { "$from": "input", "path": "/n" }, "op": "eq", "right": 3 } } } } ],
        "edges": []
    });
    let (wf, ir) = env
        .workflows
        .create(&alice, &flagged, &Catalog::default(), None)
        .await
        .unwrap();
    // 保存時の警告（保存 API・チャットの emit_workflow が返す）。
    let warnings = workflow_engine::semantics_warnings(&ir);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, workflow_engine::SEMANTICS_CHANGED);
    env.workflows
        .create(&alice, &plain, &Catalog::default(), None)
        .await
        .unwrap();
    // R3-3: 配列への starts_with（旧は常に偽・新はいずれかの要素）も DB の事前絞り込みを通って一覧に載る。
    let prefix = json!({
        "ir_version": 1, "name": "tags-prefix",
        "input_schema": { "type": "object", "properties": {
            "tags": { "type": "array", "items": { "type": "string" } } } },
        "triggers": [ { "kind": "interactive" } ],
        "nodes": [ { "id": "sw", "type": "control.branch", "params": { "condition":
            { "cmp": { "left": { "$from": "input", "path": "/tags" }, "op": "starts_with", "right": "ur" } } } } ],
        "edges": []
    });
    let (wf_prefix, prefix_ir) = env
        .workflows
        .create(&alice, &prefix, &Catalog::default(), None)
        .await
        .unwrap();
    assert_eq!(workflow_engine::semantics_warnings(&prefix_ir).len(), 1);

    // 一覧化（1 件ずつのページで keyset を回す）。
    let mut after = None;
    let mut found = Vec::new();
    loop {
        let (page, next) = api::workflow_semantics_audit::scan_page(
            &env.pool,
            Some(&tenant),
            false,
            after.as_ref(),
            1,
        )
        .await
        .unwrap();
        found.extend(page);
        match next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    assert_eq!(found.len(), 2, "{found:?}");
    let eq_hit = found
        .iter()
        .find(|f| f.workflow_id == wf)
        .expect("tags-branch");
    assert_eq!(eq_hit.name, "tags-branch");
    assert_eq!(eq_hit.node_id.as_deref(), Some("br"));
    let sw_hit = found
        .iter()
        .find(|f| f.workflow_id == wf_prefix)
        .expect("tags-prefix");
    assert_eq!(sw_hit.node_id.as_deref(), Some("sw"));
}
