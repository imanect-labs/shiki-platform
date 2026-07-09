//! Stage A ノード網羅 e2e: **本番** executor（CapabilityNodeExecutor＋ProdNodePorts）で
//! 全ノード種を実チョークポイント経由で流し、複合 DAG で相互作用まで検証する（W4 拡張）。
//!
//! カバー: script.run（＋Shiki.* hostcall・workflow.start）／storage.read/write/list／
//! agent.invoke（FakeSandbox）／llm.invoke（stub gateway）／http.request（ローカルサーバ）／
//! control.branch/switch/join。rag.search は dispatch＋未構成ゲートのみ（happy path の
//! SearchService 本体は rag クレートの結合テストが担保）。map/wait は Stage A 未実装
//! （`unsupported_stage_a`）のため対象外。
//! 実 PG＋OpenFGA＋MinIO が要る env ゲート（未設定はスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity,
    clippy::too_many_lines
)]

use std::sync::Arc;
use std::time::Duration;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use sandbox_client::{FakeExecResult, FakeSandbox};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use workflow_engine::{
    CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, NodeExecutor, RunStatus,
    RunStore, StepStatus, WorkerConfig, WorkflowRunLauncher, WorkflowStore,
};

// ------- 共通ハーネス --------------------------------------------------------

struct Harness {
    pool: sqlx::PgPool,
    storage: Arc<storage::StorageService>,
    gateway: Arc<llm_gateway::LlmGateway>,
    org: String,
    tenant: String,
    alice: AuthContext,
    workflows: WorkflowStore,
    runs: RunStore,
    launcher: WorkflowRunLauncher,
}

async fn setup() -> Option<Harness> {
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
        .max_connections(6)
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
                store_name: format!("shiki-wf-nodes-{}", Uuid::new_v4()),
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
                    base_url: None,
                    api_key: None,
                    timeout_secs: 30,
                },
                catalog: llm_gateway::ModelCatalog {
                    default_model: "stub".into(),
                    models: vec![llm_gateway::ModelEntry {
                        id: "stub".into(),
                        real_id: None,
                        prompt_price_micros_per_mtok: 0,
                        completion_price_micros_per_mtok: 0,
                    }],
                },
                langfuse: None,
            },
        )
        .expect("gateway"),
    );

    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let org = tenant.clone();
    let alice = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        org.clone(),
        tenant.clone(),
    );
    // storage.write を org 直下に許可（member@org）。
    fga.write_tuple(
        &alice.subject(),
        Relation::Member,
        &alice.ns().organization(&org),
    )
    .await
    .expect("member tuple");

    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(pool.clone());
    let delegation = DelegationStore::new(pool.clone(), Arc::clone(&fga));
    let launcher = WorkflowRunLauncher::new(delegation, workflows.clone(), runs.clone());

    Some(Harness {
        pool,
        storage,
        gateway,
        org,
        tenant,
        alice,
        workflows,
        runs,
        launcher,
    })
}

impl Harness {
    /// 本番 executor を組む（FakeSandbox＋http allowlist を差し込める）。
    fn executor(
        &self,
        sandbox: Option<Arc<dyn sandbox_client::Sandbox>>,
        http_allowlist: Vec<String>,
        search: Option<Arc<rag::SearchService>>,
    ) -> Arc<dyn NodeExecutor> {
        let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
            storage: Arc::clone(&self.storage),
            search,
            gateway: Arc::clone(&self.gateway),
            sandbox,
            sandbox_backend: sandbox_client::SandboxBackend::Wasm,
            secrets: None,
            launcher: self.launcher.clone(),
            http: reqwest::Client::new(),
            db: self.pool.clone(),
        });
        let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
        Arc::new(
            CapabilityNodeExecutor::new(
                ports,
                EffectJournal::new(self.pool.clone()),
                Arc::new(NoopAudit),
            )
            .with_script_engine(engine, script_runtime::engine::Limits::default())
            .with_http_allowlist(http_allowlist, 5_000),
        )
    }

    /// IR を保存して workflow_id を返す。
    async fn save(&self, ir: &Value) -> Uuid {
        let (wf_id, _) = self
            .workflows
            .create(&self.alice, ir, &Catalog::default(), None)
            .await
            .expect("save ir");
        wf_id
    }

    /// 保存済みワークフローを対話起動し、ワーカーで完走まで駆動する。
    async fn run(
        &self,
        wf_id: Uuid,
        input: &Value,
        executor: Arc<dyn NodeExecutor>,
    ) -> (Uuid, RunStatus) {
        let run_id = self
            .launcher
            .start_interactive(&self.alice, wf_id, input)
            .await
            .expect("start")
            .expect("run id");
        let worker = workflow_engine::WorkflowWorker::new(
            self.runs.clone(),
            executor,
            WorkerConfig::default(),
        )
        .scoped_to_tenant(&self.tenant);
        while worker.claim_and_run_once("w1").await.unwrap() {}
        let status = self
            .runs
            .run_status(&self.tenant, run_id)
            .await
            .unwrap()
            .unwrap();
        (run_id, status)
    }

    /// 保存＋単発 run。
    async fn run_to_completion(
        &self,
        ir: &Value,
        input: &Value,
        executor: Arc<dyn NodeExecutor>,
    ) -> (Uuid, RunStatus) {
        let wf_id = self.save(ir).await;
        self.run(wf_id, input, executor).await
    }

    async fn step_map(&self, run_id: Uuid) -> std::collections::HashMap<String, StepStatus> {
        self.runs
            .step_statuses(&self.tenant, run_id)
            .await
            .unwrap()
            .into_iter()
            .collect()
    }

    /// org 直下の指定名ファイルの本文を読む（検証用）。
    async fn read_file_by_name(&self, name: &str) -> Option<String> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM node WHERE org = $1 AND name = $2 AND deleted_at IS NULL \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&self.org)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .unwrap();
        let id = id?;
        let (_n, bytes) = self
            .storage
            .read_file_internal(&self.alice, id, None)
            .await
            .expect("read back");
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

struct NoopAudit;
impl workflow_engine::CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
}

/// 固定レスポンスを返すローカル HTTP サーバ（http.request の宛先）。返り値は base URL。
async fn spawn_http_echo() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let body = br#"{"ok":true,"echo":"pong"}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body).await;
            });
        }
    });
    format!("http://{addr}")
}

// ------- Test 1: 複合 mega DAG ----------------------------------------------

/// script → storage.write → storage.read → branch → {agent / llm} → join → http → storage.write。
/// 8 ノード種＋ノード間 dataflow＋dead branch skip を本番 executor で通す。
#[tokio::test]
async fn combined_pipeline_covers_many_node_types() {
    let Some(h) = setup().await else { return };
    let echo_url = spawn_http_echo().await;
    let fake = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
        stdout: b"agent-did-work".to_vec(),
        ..FakeExecResult::default()
    }));
    let sandbox = Arc::clone(&fake) as Arc<dyn sandbox_client::Sandbox>;
    let executor = h.executor(Some(sandbox), vec!["127.0.0.1".into()], None);

    let ir = json!({
        "ir_version": 1,
        "name": "e2e-mega",
        "declared_scopes": ["storage.read", "storage.write", "http.egress"],
        "nodes": [
            { "id": "gen", "type": "script.run", "params": {
                "source": { "inline": "function main(i){ return { doc: 'content-' + i.name, n: i.n }; }" },
                "input": { "$from": "input" } } },
            { "id": "src", "type": "storage.write", "params": {
                "name": "src.txt", "content": { "$from": "nodes.gen.output", "path": "/doc" } } },
            { "id": "readback", "type": "storage.read", "params": {
                "file": { "$from": "nodes.src.output", "path": "/id" } } },
            { "id": "br", "type": "control.branch", "params": {
                "condition": { "cmp": { "left": { "$from": "nodes.gen.output", "path": "/n" }, "op": "gt", "right": 5 } } } },
            { "id": "ag", "type": "agent.invoke", "params": { "instruction": "do the work", "egress_allowlist": [] } },
            { "id": "lm", "type": "llm.invoke", "params": { "prompt": "summarize" } },
            { "id": "jn", "type": "control.join", "params": { "mode": "all" } },
            { "id": "post", "type": "http.request", "params": {
                "method": "POST", "url": HTTP_URL_PLACEHOLDER,
                "body": { "$from": "nodes.readback.output", "path": "/text" } } },
            { "id": "result", "type": "storage.write", "params": {
                "name": "result.txt", "content": { "$from": "nodes.ag.output", "path": "/stdout" } } }
        ],
        "edges": [
            { "from": "gen", "to": "src" },
            { "from": "src", "to": "readback" },
            { "from": "readback", "to": "br" },
            { "from": "br", "from_port": "true", "to": "ag" },
            { "from": "br", "from_port": "false", "to": "lm" },
            { "from": "ag", "to": "jn" },
            { "from": "lm", "to": "jn" },
            { "from": "jn", "to": "post" },
            { "from": "post", "to": "result" }
        ],
        "triggers": [{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]
    });
    // url プレースホルダを実サーバへ差し替え（host リテラル制約のため文字列で埋める）。
    let ir = replace_url(ir, &format!("{echo_url}/hook"));

    let (run_id, status) = h
        .run_to_completion(&ir, &json!({ "name": "world", "n": 10 }), executor)
        .await;
    assert_eq!(status, RunStatus::Succeeded, "複合 DAG が完走する");

    let steps = h.step_map(run_id).await;
    assert_eq!(
        steps.get("ag"),
        Some(&StepStatus::Succeeded),
        "true 分岐の agent が実行される"
    );
    assert_eq!(
        steps.get("lm"),
        Some(&StepStatus::Skipped),
        "false 分岐の llm は skip される"
    );
    assert_eq!(
        steps.get("jn"),
        Some(&StepStatus::Succeeded),
        "join が発火する"
    );
    assert_eq!(
        steps.get("post"),
        Some(&StepStatus::Succeeded),
        "http.request が成功する"
    );

    // dataflow: script→storage.write の内容が読み戻せる。
    assert_eq!(
        h.read_file_by_name("src.txt").await.as_deref(),
        Some("content-world")
    );
    // dataflow: agent.invoke の stdout が最終 storage.write へ流れる。
    assert_eq!(
        h.read_file_by_name("result.txt").await.as_deref(),
        Some("agent-did-work")
    );

    // 能力縮小: FakeSandbox の spec は egress 遮断（ノード設定で ReBAC 超え不可）。
    let specs = fake.created_specs();
    assert!(!specs.is_empty(), "agent 用サンドボックスが作られた");
    assert!(
        specs[0].egress.static_allow.is_empty() && specs[0].egress.dynamic_allow.is_empty(),
        "agent サンドボックスは egress 遮断（allowlist 空＝capability 縮小のみ）"
    );
    // 隔離ティアは admin ポリシー（ここでは既定 wasm）が spec に反映される。
    assert_eq!(specs[0].backend, sandbox_client::SandboxBackend::Wasm);
}

/// テスト用に url プレースホルダを差し替える（json! で動的 URL を host リテラルに埋めるため）。
const HTTP_URL_PLACEHOLDER: &str = "http://127.0.0.1:0/__placeholder__";
fn replace_url(mut ir: Value, url: &str) -> Value {
    if let Some(nodes) = ir.get_mut("nodes").and_then(Value::as_array_mut) {
        for n in nodes {
            if n.get("id").and_then(Value::as_str) == Some("post") {
                n["params"]["url"] = json!(url);
            }
        }
    }
    ir
}

// ------- Test 2: switch ルーティング＋storage.list＋llm.invoke ----------------

#[tokio::test]
async fn switch_routes_to_list_or_llm() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let ir = json!({
        "ir_version": 1, "name": "e2e-switch",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "sw", "type": "control.switch", "params": {
                "value": { "$from": "input", "path": "/kind" },
                "cases": [ { "port": "listing", "equals": "list" } ] } },
            { "id": "ls", "type": "storage.list", "params": {} },
            { "id": "ai", "type": "llm.invoke", "params": { "prompt": "hello" } }
        ],
        "edges": [
            { "from": "sw", "from_port": "listing", "to": "ls" },
            { "from": "sw", "from_port": "default", "to": "ai" }
        ],
        "triggers": [{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]
    });
    let wf = h.save(&ir).await;

    // case 一致: storage.list が実行され、default(llm) は skip。
    let (r1, s1) = h
        .run(wf, &json!({ "kind": "list" }), Arc::clone(&executor))
        .await;
    assert_eq!(s1, RunStatus::Succeeded);
    let m1 = h.step_map(r1).await;
    assert_eq!(
        m1.get("ls"),
        Some(&StepStatus::Succeeded),
        "switch case で storage.list 実行"
    );
    assert_eq!(
        m1.get("ai"),
        Some(&StepStatus::Skipped),
        "default(llm) は skip"
    );

    // default: llm.invoke が実行され、case(list) は skip。
    let (r2, s2) = h.run(wf, &json!({ "kind": "other" }), executor).await;
    assert_eq!(s2, RunStatus::Succeeded);
    let m2 = h.step_map(r2).await;
    assert_eq!(
        m2.get("ai"),
        Some(&StepStatus::Succeeded),
        "switch default で llm.invoke 実行"
    );
    assert_eq!(
        m2.get("ls"),
        Some(&StepStatus::Skipped),
        "case(list) は skip"
    );
}

// ------- Test 3: script の Shiki.*（storage.write）＋Shiki.workflow.start -------

#[tokio::test]
async fn script_shiki_hostcalls_and_workflow_start_child() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);

    // 子ワークフロー（名前 child-wf）: storage.write でファイルを作る。
    let child_ir = json!({
        "ir_version": 1, "name": "child-wf",
        "declared_scopes": ["storage.write"],
        "nodes": [ { "id": "w", "type": "storage.write", "params": {
            "name": "child-out.txt", "content": "child-ran" } } ],
        "edges": [],
        "triggers": [{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]
    });
    let child_id = h.save(&child_ir).await;

    // 親: script が Shiki.storage.write ＋ Shiki.workflow.start('child-wf') を呼ぶ。
    let parent_ir = json!({
        "ir_version": 1, "name": "parent-wf",
        "declared_scopes": ["storage.write", "workflow.start"],
        "nodes": [ { "id": "s", "type": "script.run", "params": {
            "source": { "inline":
                "function main(i){ Shiki.storage.write(null, 'from-script.txt', 'hi-from-script', 'text/plain'); var r = Shiki.workflow.start('child-wf', { x: 1 }); return { child: r }; }" },
            "input": { "$from": "input" } } } ],
        "edges": [],
        "triggers": [{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]
    });
    let parent = h.save(&parent_ir).await;

    let (_r, status) = h.run(parent, &json!({}), executor).await;
    assert_eq!(status, RunStatus::Succeeded, "親 script が完走する");

    // Shiki.storage.write が実ファイルを作った。
    assert_eq!(
        h.read_file_by_name("from-script.txt").await.as_deref(),
        Some("hi-from-script"),
        "script の Shiki.storage.write が能力ゲートウェイ経由で書き込む"
    );
    // Shiki.workflow.start で子 run が作られ、ワーカーが完走させた（child-out.txt が存在）。
    let child_runs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&h.tenant)
    .bind(child_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert!(child_runs >= 1, "Shiki.workflow.start で子 run が起動する");
    assert_eq!(
        h.read_file_by_name("child-out.txt").await.as_deref(),
        Some("child-ran"),
        "子ワークフローも完走してファイルを書く"
    );
}

// ------- Test 4: rag.search の dispatch＋未構成ゲート -------------------------

/// rag.search は executor が rag ポートへ dispatch する（happy path の SearchService 本体は
/// rag クレートの結合テストが担保）。ここでは rag 未構成時に fail-closed（forbidden）することを確認。
#[tokio::test]
async fn rag_search_dispatch_denied_when_unconfigured() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None); // search=None
    let ir = json!({
        "ir_version": 1, "name": "e2e-rag",
        "declared_scopes": ["rag.query"],
        "nodes": [ { "id": "q", "type": "rag.search", "params": { "query": "hello", "top_k": 3 } } ],
        "edges": [],
        "triggers": [{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]
    });
    let (run_id, status) = h.run_to_completion(&ir, &json!({}), executor).await;
    assert_eq!(status, RunStatus::Failed, "rag 未構成では fail-closed");
    let steps = h.step_map(run_id).await;
    assert_eq!(
        steps.get("q"),
        Some(&StepStatus::Failed),
        "rag.search が forbidden で失敗"
    );
}

// ------- Test 5: map 動的 fan-out（本番 executor・要素ごと storage.write）--------

/// map が items 各要素を storage.write で書き、集約後に後続が実行される（#178）。
#[tokio::test]
async fn map_fans_out_storage_writes_per_item() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let ir = json!({
        "ir_version": 1, "name": "e2e-map",
        "declared_scopes": ["storage.write"],
        "nodes": [
            { "id": "m", "type": "control.map", "params": {
                "items": { "$from": "input", "path": "/files" } } },
            { "id": "w", "type": "storage.write", "parent": "m", "params": {
                "name": { "$from": "each", "path": "/item" },
                "content": { "$from": "each", "path": "/item" } } },
            { "id": "done", "type": "storage.write", "params": { "name": "done.txt", "content": "ok" } }
        ],
        "edges": [{ "from": "m", "to": "done" }],
        "triggers": [{ "kind": "interactive" }]
    });
    let (run_id, status) = h
        .run_to_completion(
            &ir,
            &json!({ "files": ["alpha", "beta", "gamma"] }),
            executor,
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded, "map fan-out が完走する");

    // 各要素が each.item で別ファイルを書く（能力ゲートウェイ→StorageService を要素ごと通る）。
    for name in ["alpha", "beta", "gamma"] {
        assert_eq!(
            h.read_file_by_name(name).await.as_deref(),
            Some(name),
            "{name} が書かれる"
        );
    }
    let steps = h.step_map(run_id).await;
    for i in 0..3 {
        assert_eq!(
            steps.get(&format!("m[{i}].w")),
            Some(&StepStatus::Succeeded),
            "要素 {i} が succeeded"
        );
    }
    assert_eq!(
        steps.get("done"),
        Some(&StepStatus::Succeeded),
        "map 集約後の後続が実行"
    );
}

// ------- Test 6: wait durable ＋ error ポート（本番 executor）------------------

/// wait(duration) で中断→スケジューラ起床→storage.read 失敗を error ポートで握って継続する（#178/#179）。
#[tokio::test]
async fn wait_then_error_port_through_production_executor() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let bad_id = Uuid::new_v4().to_string();
    let ir = json!({
        "ir_version": 1, "name": "e2e-wait-err",
        "declared_scopes": ["storage.read", "storage.write"],
        "nodes": [
            { "id": "wait", "type": "control.wait", "params": { "kind": "duration", "duration_sec": 0 } },
            { "id": "rd", "type": "storage.read", "on_error": "continue",
              "params": { "file": bad_id } },
            { "id": "recover", "type": "storage.write", "params": { "name": "recovered.txt", "content": "recovered" } },
            { "id": "normal", "type": "storage.write", "params": { "name": "normal.txt", "content": "normal" } }
        ],
        "edges": [
            { "from": "wait", "to": "rd" },
            { "from": "rd", "from_port": "error", "to": "recover" },
            { "from": "rd", "to": "normal" }
        ],
        "triggers": [{ "kind": "interactive" }]
    });
    let wf_id = h.save(&ir).await;
    let run_id = h
        .launcher
        .start_interactive(&h.alice, wf_id, &json!({}))
        .await
        .unwrap()
        .unwrap();
    let worker = workflow_engine::WorkflowWorker::new(
        h.runs.clone(),
        Arc::clone(&executor),
        WorkerConfig::default(),
    )
    .scoped_to_tenant(&h.tenant);

    // ワーカーは wait で中断し解放される（done は未実行）。
    while worker.claim_and_run_once("w1").await.unwrap() {}
    assert_eq!(
        h.step_map(run_id).await.get("wait"),
        Some(&StepStatus::WaitingTimer),
        "wait は waiting_timer で待機"
    );

    // スケジューラ相当の起床（ワーカー解放を跨ぐ durable 継続）。
    let woke = h
        .runs
        .wake_due_timers(
            chrono::Utc::now() + chrono::Duration::seconds(10),
            Some(&h.tenant),
        )
        .await
        .unwrap();
    assert_eq!(woke, 1);
    while worker.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        h.runs.run_status(&h.tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded),
        "error ポートで握って run は succeeded"
    );
    let steps = h.step_map(run_id).await;
    assert_eq!(
        steps.get("rd"),
        Some(&StepStatus::Failed),
        "storage.read が失敗"
    );
    assert_eq!(
        steps.get("recover"),
        Some(&StepStatus::Succeeded),
        "error 後続が実行される"
    );
    assert_eq!(
        steps.get("normal"),
        Some(&StepStatus::Skipped),
        "out 後続は skip される"
    );
    assert_eq!(
        h.read_file_by_name("recovered.txt").await.as_deref(),
        Some("recovered")
    );
}
