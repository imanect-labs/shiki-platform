//! ファイルを扱うワークフローの結合テスト（実 Postgres ＋実 OpenFGA ＋ RustFS ＋隔離 DuckDB ランナー）。
//!
//! - **BUG-8**: ファイル保存トリガ（event storage.write）の run 入力に、保存されたファイルの id・版・操作・
//!   書込主体が載り、`{"$from":"trigger","path":"/file_id"}` で storage.read / csv.query が読める。
//!   削除とフォルダの作成では発火しない。wait(event) の起床にも同じ形が渡る。イベントは本番と同じ
//!   `api::workflow_runtime::relay_events` で配り、run は本番の executor（`ProdNodePorts`）で流す。
//! - **BUG-10**: 同意計画が csv.* の対象ファイルを提案し、その委譲で schedule の run（workflow
//!   プリンシパル）が csv.query / csv.patch できる。委譲が無ければファイルが見えず（not_found）失敗する。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が無いとスキップする。csv.* の部分は隔離ランナー
//! （`SHIKI_TABULAR_RUNNER` か `crates/tabular/runner` のビルド成果物）が無いとスキップする。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{
    CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, GrantRequest, NodeExecutor,
    RegistrationService, RunStatus, RunStore, SchedulerStore, StepStatus, WorkerConfig, WorkflowIr,
    WorkflowRunLauncher, WorkflowStore,
};

struct NoopAudit;
impl workflow_engine::CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
}

struct Env {
    pool: sqlx::PgPool,
    storage: Arc<storage::StorageService>,
    workflows: WorkflowStore,
    runs: RunStore,
    launcher: WorkflowRunLauncher,
    registration: RegistrationService,
    executor: Arc<dyn NodeExecutor>,
    /// 隔離ランナーがあり、csv.* を実行できるか。
    tabular: bool,
    tenant: String,
    alice: AuthContext,
}

/// 隔離ランナーの実体（`SHIKI_TABULAR_RUNNER` か除外クレートの既定ビルド先）。
fn runner_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SHIKI_TABULAR_RUNNER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    ["release", "debug"]
        .iter()
        .map(|profile| {
            root.join("crates/tabular/runner/target")
                .join(profile)
                .join("shiki-tabular-runner")
        })
        .find(|p| p.exists())
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
                store_name: format!("shiki-wf-files-{}", Uuid::new_v4()),
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
    object_store.ensure_bucket().await.expect("bucket");
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

    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        tenant.clone(),
        tenant.clone(),
    );
    // alice はドライブのルートにファイル・フォルダを作れる（member@org）。
    fga.write_tuple(
        &alice.subject(),
        Relation::Member,
        &alice.ns().organization(&alice.org),
    )
    .await
    .expect("member tuple");

    let runner = runner_path();
    if runner.is_none() {
        eprintln!("shiki-tabular-runner 未ビルドのため csv.* の確認はスキップ（SHIKI_TABULAR_RUNNER 参照）");
    }
    let tabular = runner.map(|r| {
        Arc::new(tabular::TabularService::new(
            Arc::clone(&storage),
            tabular::RunnerConfig::new(r.to_string_lossy().to_string(), Duration::from_secs(20)),
            tabular::Quotas::default(),
        ))
    });
    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let delegation = DelegationStore::new(pool.clone(), Arc::clone(&fga));
    let launcher = WorkflowRunLauncher::new(delegation.clone(), workflows.clone(), runs.clone());
    let registration = RegistrationService::new(pool.clone(), delegation);
    let has_tabular = tabular.is_some();
    let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
        storage: Arc::clone(&storage),
        search: None,
        gateway,
        sandbox: None,
        sandbox_backend: sandbox_client::SandboxBackend::Wasm,
        secrets: None,
        tabular,
        skill_artifacts: None,
        launcher: launcher.clone(),
        http: reqwest::Client::new(),
        db: pool.clone(),
        data: None,
        fsms: None,
    });
    let executor: Arc<dyn NodeExecutor> = Arc::new(CapabilityNodeExecutor::new(
        ports,
        EffectJournal::new(pool.clone()),
        Arc::new(NoopAudit),
    ));
    Some(Env {
        pool,
        storage,
        workflows,
        runs,
        launcher,
        registration,
        executor,
        tabular: has_tabular,
        tenant,
        alice,
    })
}

impl Env {
    fn worker(&self) -> workflow_engine::WorkflowWorker {
        workflow_engine::WorkflowWorker::new(
            self.runs.clone(),
            Arc::clone(&self.executor),
            WorkerConfig::default(),
        )
        .scoped_to_tenant(&self.tenant)
    }

    /// 本番と同じ relay で outbox を配り、ワーカーで捌く。`done` が真になるまで繰り返す。
    ///
    /// relay はテナントを問わず未配送のイベントを掴むので、同じ DB で並行するテストに先に掴まれることが
    /// ある（掴んだ側も同じ DB へ run を作るので、待てば届く）。
    async fn relay_until<F, Fut>(&self, what: &str, done: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let sched = SchedulerStore::new(self.pool.clone());
        let worker = self.worker();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            api::workflow_runtime::relay_events(
                &self.pool,
                &storage::event::ClaimHints::default(),
                &sched,
                &self.runs,
                &self.launcher,
            )
            .await
            .expect("relay");
            while worker.claim_and_run_once("w1").await.unwrap() {}
            if done().await {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "{what} にならない");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// ワークフローの run（作成順）: (run_id, 入力, 状態)。
    async fn runs_of(&self, wf: Uuid) -> Vec<(Uuid, Value, String)> {
        sqlx::query_as::<_, (Uuid, sqlx::types::Json<Value>, String)>(
            "SELECT run_id, input, status FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 ORDER BY created_at",
        )
        .bind(&self.tenant)
        .bind(wf)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, input, status)| (id, input.0, status))
        .collect()
    }

    async fn step(&self, run: Uuid, node: &str) -> (String, Option<Value>, Option<Value>) {
        sqlx::query_as(
            "SELECT status, output, error FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3",
        )
        .bind(&self.tenant)
        .bind(run)
        .bind(node)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 自分のテナントで、その操作の outbox イベントが workflow へ配送済みか。
    async fn delivered(&self, op: &str, node_id: Uuid) -> bool {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM storage_event_outbox o \
               JOIN outbox_delivery d ON d.event_id = o.id AND d.consumer = 'workflow' \
               WHERE o.tenant_id = $1 AND o.op = $2 AND o.node_id = $3)",
        )
        .bind(&self.tenant)
        .bind(op)
        .bind(node_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 同意計画の提案のうち対象が確定したものを、有効化の委譲にする（同意画面で全部を選んだのと同じ）。
    fn grants_from_plan(&self, ir: &WorkflowIr) -> Vec<GrantRequest> {
        RegistrationService::consent_plan(ir)
            .into_iter()
            .filter(|g| !g.needs_user_pick)
            .map(|g| {
                let id = g.object_id.expect("確定した対象");
                let object = match g.object_kind.as_str() {
                    "file" => self.alice.ns().file(&id),
                    "folder" => self.alice.ns().folder(&id),
                    other => panic!("想定外の対象: {other}"),
                };
                GrantRequest {
                    scope: g.scope,
                    object,
                    relation: Relation::parse(&g.relation).expect("relation"),
                }
            })
            .collect()
    }

    /// スケジュールを 1 回発火させ、ワーカーで完走させる。作られた run の id を返す。
    async fn fire_schedule_and_run(&self, wf: Uuid, at_secs: i64) -> Uuid {
        let sched = SchedulerStore::new(self.pool.clone());
        let fired = sched
            .tick_schedules(
                Utc::now() + chrono::Duration::seconds(at_secs),
                Some(&self.tenant),
                &self.launcher,
            )
            .await
            .unwrap();
        assert!(fired >= 1, "schedule が発火する（fired={fired}）");
        let worker = self.worker();
        while worker.claim_and_run_once("w1").await.unwrap() {}
        let runs = self.runs_of(wf).await;
        runs.last().expect("run が作られる").0
    }
}

const ORDERS_CSV: &[u8] = b"item,qty\nA,1\nB,2\nC,3\n";

/// ファイル保存トリガの run は、保存されたファイルを `$from trigger /file_id` で読める（BUG-8）。
/// 削除・フォルダの作成では発火しない。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_save_trigger_hands_the_saved_file_to_the_run() {
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "受付", None)
        .await
        .unwrap();

    let file_ref = json!({ "$from": "trigger", "path": "/file_id" });
    let mut nodes =
        vec![json!({ "id": "rd", "type": "storage.read", "params": { "file": file_ref } })];
    let mut edges = vec![];
    let mut scopes = vec!["storage.read"];
    if env.tabular {
        nodes.push(json!({ "id": "q", "type": "csv.query", "params": {
            "file": file_ref, "sql": "SELECT count(*) AS n FROM data" } }));
        edges.push(json!({ "from": "rd", "to": "q" }));
        scopes.push("csv.read");
    }
    let ir = json!({
        "ir_version": 1, "name": "inbox-csv",
        "declared_scopes": scopes,
        "triggers": [ { "kind": "event", "source": "storage.write",
                        "scope": { "folder": inbox.id.to_string() } } ],
        "nodes": nodes, "edges": edges
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    // 委譲はトリガのフォルダの閲覧だけ（ファイルはトリガで決まるので選ばない）。配下のファイルは
    // フォルダの viewer で読める。
    let grants = env.grants_from_plan(&parsed);
    assert_eq!(grants.len(), 1, "{grants:?}");
    assert_eq!(
        grants[0].object,
        env.alice.ns().folder(&inbox.id.to_string())
    );
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &grants)
        .await
        .expect("enable");

    let file = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "orders.csv",
            ORDERS_CSV,
            "text/csv",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    env.relay_until("保存イベントの run の完了", || async move {
        e.runs_of(wf)
            .await
            .first()
            .is_some_and(|(_, _, s)| s == "succeeded" || s == "failed")
    })
    .await;

    let runs = env.runs_of(wf).await;
    assert_eq!(runs.len(), 1, "保存 1 回で run 1 件: {runs:?}");
    let (run, input, status) = &runs[0];
    // run 入力（$from trigger）に、保存されたファイルの id・版・操作・書込主体が載る。
    assert_eq!(input["file_id"], file.id.to_string());
    assert_eq!(input["version"], file.version);
    assert_eq!(input["op"], "create");
    assert_eq!(input["kind"], "file");
    assert_eq!(input["parent_id"], inbox.id.to_string());
    assert_eq!(input["size"], ORDERS_CSV.len());
    assert_eq!(input["actor"], "alice");
    assert_eq!(status, "succeeded", "rd: {:?}", env.step(*run, "rd").await);
    let (_, rd, _) = env.step(*run, "rd").await;
    let rd = rd.unwrap();
    assert_eq!(rd["id"], file.id.to_string());
    assert_eq!(rd["name"], "orders.csv");
    assert_eq!(rd["text"], String::from_utf8_lossy(ORDERS_CSV).as_ref());
    if env.tabular {
        // 委譲元のフォルダ経由で、workflow プリンシパルが CSV を集計できる。
        let (_, q, _) = env.step(*run, "q").await;
        assert_eq!(q.unwrap()["rows"], json!([["3"]]));
    }

    // 削除とフォルダの作成は「ファイルが保存されたとき」ではない（配送はされるが発火しない）。
    env.storage
        .soft_delete_file(&env.alice, file.id, None)
        .await
        .unwrap();
    let sub = env
        .storage
        .create_folder(&env.alice, Some(inbox.id), "sub", None)
        .await
        .unwrap();
    let (file_id, sub_id) = (file.id, sub.id);
    env.relay_until("削除・フォルダ作成の配送", || async move {
        e.delivered("delete", file_id).await && e.delivered("create", sub_id).await
    })
    .await;
    assert_eq!(
        env.runs_of(wf).await.len(),
        1,
        "削除・フォルダの作成では起動しない"
    );
}

/// wait(event) の起床にも、保存されたファイルの id が届く（同じペイロード・BUG-8）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_event_receives_the_saved_file() {
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "提出", None)
        .await
        .unwrap();
    let ir = json!({
        "ir_version": 1, "name": "wait-for-upload",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "interactive" } ],
        "nodes": [
            { "id": "w", "type": "control.wait", "params": {
                "kind": "event", "source": "storage.write",
                "scope": { "folder": inbox.id.to_string() }, "timeout_sec": 600 } },
            { "id": "rd", "type": "storage.read", "params": {
                "file": { "$from": "nodes.w.output", "path": "/file_id" } } }
        ],
        "edges": [ { "from": "w", "to": "rd" } ]
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let run = env
        .launcher
        .start_interactive(&env.alice, wf, &json!({}))
        .await
        .unwrap()
        .unwrap();
    let worker = env.worker();
    while worker.claim_and_run_once("w1").await.unwrap() {}
    let steps: std::collections::HashMap<String, StepStatus> = env
        .runs
        .step_statuses(&env.tenant, run)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(steps.get("w"), Some(&StepStatus::WaitingEvent));

    let file = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "report.txt",
            b"done",
            "text/plain",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    env.relay_until("wait の起床と run の完了", || async move {
        e.runs
            .run_status(&e.tenant, run)
            .await
            .unwrap()
            .is_some_and(|s| s != RunStatus::Running)
    })
    .await;
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Succeeded),
        "rd: {:?}",
        env.step(run, "rd").await
    );
    let (_, w, _) = env.step(run, "w").await;
    let w = w.unwrap();
    assert_eq!(w["file_id"], file.id.to_string());
    assert_eq!(w["op"], "create");
    assert_eq!(w["actor"], "alice");
    let (_, rd, _) = env.step(run, "rd").await;
    assert_eq!(rd.unwrap()["text"], "done");
}

/// 同意計画が csv.* の対象ファイルを提案し、その委譲で schedule の run が CSV を集計・編集できる
/// （BUG-10）。委譲が無いまま有効化すると、run はファイルが見えずに失敗する（存在を明かさない not_found）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_csv_nodes_run_with_the_files_delegated_in_the_consent_plan() {
    let Some(env) = setup().await else { return };
    if !env.tabular {
        return;
    }
    let file = env
        .storage
        .write_file_internal(&env.alice, None, "sales.csv", ORDERS_CSV, "text/csv", None)
        .await
        .unwrap();
    let id = file.id.to_string();
    let ir = json!({
        "ir_version": 1, "name": "nightly-csv",
        "declared_scopes": ["csv.read", "csv.write"],
        "triggers": [ { "kind": "schedule", "cron": "* * * * *", "tz": "UTC" } ],
        "nodes": [
            { "id": "q", "type": "csv.query", "params": {
                "file": id, "sql": "SELECT sum(qty) AS total FROM data" } },
            { "id": "p", "type": "csv.patch", "params": {
                "file": id, "base_rev": file.version,
                "ops": [ { "op": "cell_update", "row": 0, "col": 1, "value": "10" } ] } }
        ],
        "edges": [ { "from": "q", "to": "p" } ]
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    let file_obj = env.alice.ns().file(&id);
    let grants = env.grants_from_plan(&parsed);
    let want = [
        ("csv.read", Relation::Viewer),
        ("csv.write", Relation::Editor),
    ];
    for (scope, relation) in want {
        assert!(
            grants
                .iter()
                .any(|g| g.scope == scope && g.object == file_obj && g.relation == relation),
            "{scope} の委譲が提案される: {grants:?}"
        );
    }

    // 委譲なしで有効化（以前の同意計画と同じ状態）: 有効化はできるが、run は権限不足で落ちる。
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &[])
        .await
        .expect("enable（委譲なし）");
    let denied = env.fire_schedule_and_run(wf, 61).await;
    let (status, _, err) = env.step(denied, "q").await;
    assert_eq!(status, "failed", "{err:?}");
    assert_eq!(err.unwrap()["code"], "not_found");

    // 同意計画の提案どおりに委譲すると、workflow プリンシパルが読み書きできる。
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &grants)
        .await
        .expect("enable（委譲あり）");
    let run = env.fire_schedule_and_run(wf, 181).await;
    assert_ne!(run, denied);
    let principal: (String, String) = sqlx::query_as(
        "SELECT trigger_kind, principal_kind FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&env.tenant)
    .bind(run)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(principal, ("schedule".into(), "workflow".into()));
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Succeeded),
        "q: {:?} / p: {:?}",
        env.step(run, "q").await,
        env.step(run, "p").await
    );
    let (_, q, _) = env.step(run, "q").await;
    assert_eq!(q.unwrap()["rows"], json!([["6"]]));
    let (_, p, _) = env.step(run, "p").await;
    assert_eq!(p.unwrap()["version"], file.version + 1);
    let (_, bytes) = env
        .storage
        .read_file_internal(&env.alice, file.id, None)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        "item,qty\nA,10\nB,2\nC,3\n"
    );
}
