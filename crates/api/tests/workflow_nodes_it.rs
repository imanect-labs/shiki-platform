//! Stage A ノード網羅 e2e: **本番** executor（CapabilityNodeExecutor＋ProdNodePorts）で
//! 全ノード種を実チョークポイント経由で流し、複合 DAG で相互作用まで検証する（W4 拡張）。
//!
//! カバー: script.run（＋Shiki.* hostcall・workflow.start）／storage.read/write/list／
//! agent.invoke（stub gateway のツールループ＋道具 code.run の FakeSandbox）／llm.invoke（stub gateway）／http.request（ローカルサーバ）／
//! control.branch/switch/join。rag.search は dispatch＋未構成ゲートのみ（happy path の
//! SearchService 本体は rag クレートの結合テストが担保）。map/wait は Stage A 未実装
//! （`unsupported_stage_a`）のため対象外。
//! 実 PG＋OpenFGA＋RustFS が要る env ゲート（未設定はスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity,
    clippy::too_many_lines
)]

use std::path::PathBuf;
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
                    user_agent: None,
                    session_header: None,
                    ..Default::default()
                },
                // stub-smart は組織の既定の切り替え（BUG-12）を見るための 2 つ目のモデル。判定用のモデルは
                // stub の決定器（decide-stub）と、stub-smart を元に LLM に判定させる decide-via（FEAT-1）。
                catalog: llm_gateway::ModelCatalog {
                    default_model: "stub".into(),
                    models: ["stub", "stub-smart"]
                        .into_iter()
                        .map(|id| llm_gateway::ModelEntry {
                            id: id.into(),
                            ..Default::default()
                        })
                        .chain([
                            llm_gateway::ModelEntry {
                                id: "decide-stub".into(),
                                modality: llm_gateway::Modality::Decision,
                                ..Default::default()
                            },
                            llm_gateway::ModelEntry {
                                id: "decide-via".into(),
                                modality: llm_gateway::Modality::Decision,
                                decision_via: Some("stub-smart".into()),
                                ..Default::default()
                            },
                        ])
                        .collect(),
                },
                langfuse: None,
                ..Default::default()
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
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    /// 本番ポート（sandbox・RAG を差し込める）。
    fn ports(
        &self,
        sandbox: Option<Arc<dyn sandbox_client::Sandbox>>,
        search: Option<Arc<rag::SearchService>>,
    ) -> Arc<api::workflow_runtime::ProdNodePorts> {
        Arc::new(api::workflow_runtime::ProdNodePorts {
            storage: Arc::clone(&self.storage),
            search,
            gateway: Arc::clone(&self.gateway),
            sandbox,
            sandbox_backend: sandbox_client::SandboxBackend::Wasm,
            secrets: None,
            tabular: None,
            skill_artifacts: None,
            launcher: self.launcher.clone(),
            http: reqwest::Client::new(),
            db: self.pool.clone(),
            data: None,
            fsms: None,
            web_search: None,
        })
    }

    /// 本番 executor を組む（FakeSandbox＋http allowlist を差し込める）。
    fn executor(
        &self,
        sandbox: Option<Arc<dyn sandbox_client::Sandbox>>,
        http_allowlist: Vec<String>,
        search: Option<Arc<rag::SearchService>>,
    ) -> Arc<dyn NodeExecutor> {
        let ports = self.ports(sandbox, search);
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

    /// 本番 executor（隔離 DuckDB ランナー付き・csv.* ノード用）を組む。
    fn executor_with_tabular(&self, runner: &std::path::Path) -> Arc<dyn NodeExecutor> {
        let tabular = Arc::new(tabular::TabularService::new(
            Arc::clone(&self.storage),
            tabular::RunnerConfig::new(
                runner.to_string_lossy().to_string(),
                Duration::from_secs(20),
            ),
            tabular::Quotas::default(),
        ));
        let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
            storage: Arc::clone(&self.storage),
            search: None,
            gateway: Arc::clone(&self.gateway),
            sandbox: None,
            sandbox_backend: sandbox_client::SandboxBackend::Wasm,
            secrets: None,
            tabular: Some(tabular),
            skill_artifacts: None,
            launcher: self.launcher.clone(),
            http: reqwest::Client::new(),
            db: self.pool.clone(),
            data: None,
            fsms: None,
            web_search: None,
        });
        Arc::new(CapabilityNodeExecutor::new(
            ports,
            EffectJournal::new(self.pool.clone()),
            Arc::new(NoopAudit),
        ))
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

/// script → storage.write → storage.read → branch → {agent（道具 code.run） / llm} → join → http → storage.write。
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
        "declared_scopes": ["storage.read", "storage.write", "http.egress", "sandbox.exec"],
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
            // stub の LLM は依頼の calltool: で道具を 1 回呼び、2 手目で回答する（BUG-6）。
            { "id": "ag", "type": "agent.invoke", "params": {
                "instruction": "calltool:code.run {\"code\":\"print('work')\"}",
                "allowed_tools": ["code.run"] } },
            { "id": "lm", "type": "llm.invoke", "params": { "prompt": "summarize" } },
            { "id": "jn", "type": "control.join", "params": { "mode": "all" } },
            { "id": "post", "type": "http.request", "params": {
                "method": "POST", "url": HTTP_URL_PLACEHOLDER,
                "body": { "$from": "nodes.readback.output", "path": "/text" } } },
            { "id": "result", "type": "storage.write", "params": {
                "name": "result.txt", "content": { "$from": "nodes.ag.output", "path": "/text" } } }
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
    // dataflow: agent.invoke の回答（/text）が最終 storage.write へ流れる。
    assert_eq!(
        h.read_file_by_name("result.txt").await.as_deref(),
        Some("回答: calltool:code.run {\"code\":\"print('work')\"}")
    );

    // 能力縮小: 道具 code.run の FakeSandbox の spec は egress 遮断（ノード設定で ReBAC 超え不可）。
    let specs = fake.created_specs();
    assert!(!specs.is_empty(), "code.run 用サンドボックスが作られた");
    assert!(
        specs[0].egress.static_allow.is_empty() && specs[0].egress.dynamic_allow.is_empty(),
        "code.run のサンドボックスは egress 遮断（capability 縮小のみ）"
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

// ------- Test 7: csv.query ノード（本番 executor＋隔離 DuckDB ランナー・Task 11P.9）--------

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

/// csv.query ノードが、アップロードした CSV を隔離 DuckDB 経由で読み、RO SQL の結果を
/// 後続 storage.write へ流す（IR → executor → scope 天井 → ProdNodePorts.csv_query →
/// TabularService → ランナー → DuckDB の全経路 e2e）。
#[tokio::test]
async fn csv_query_node_reads_uploaded_csv() {
    let Some(h) = setup().await else { return };
    let Some(runner) = runner_path() else {
        eprintln!("shiki-tabular-runner 未ビルドのためスキップ（SHIKI_TABULAR_RUNNER 参照）");
        return;
    };
    // alice が CSV をアップロード（作成者＝owner なので viewer で読める）。
    let csv = b"name,price\nAAA,50\nBBB,150\nCCC,200\n";
    let node = h
        .storage
        .write_file_internal(&h.alice, None, "data.csv", csv, "text/csv", None)
        .await
        .expect("csv アップロード");

    let executor = h.executor_with_tabular(&runner);
    let ir = json!({
        "ir_version": 1,
        "name": "csv-query-e2e",
        "declared_scopes": ["csv.read", "storage.write"],
        "nodes": [
            { "id": "q", "type": "csv.query", "params": {
                "file": { "$from": "input", "path": "/file" },
                "sql": "SELECT name FROM data WHERE name != 'AAA' ORDER BY name" } },
            { "id": "save", "type": "storage.write", "params": {
                "name": "csv-result.json",
                "content": { "$from": "nodes.q.output" },
                "content_type": "application/json" } }
        ],
        "edges": [ { "from": "q", "to": "save" } ]
    });
    let wf_id = h.save(&ir).await;
    let (_run, status) = h
        .run(wf_id, &json!({ "file": node.id.to_string() }), executor)
        .await;
    assert_eq!(
        status,
        RunStatus::Succeeded,
        "csv.query パイプラインが完走する"
    );

    let result = h
        .read_file_by_name("csv-result.json")
        .await
        .expect("結果ファイル");
    // 読み取り専用 SQL の絞り込み結果が流れている（AAA は除外）。
    assert!(result.contains("BBB"), "結果に BBB: {result}");
    assert!(result.contains("CCC"), "結果に CCC: {result}");
    assert!(!result.contains("AAA"), "AAA は WHERE で除外: {result}");
}

/// csv.query は declared_scopes に csv.read が無いと scope 天井で拒否される（fail-closed）。
#[tokio::test]
async fn csv_query_denied_without_declared_scope() {
    let Some(h) = setup().await else { return };
    let Some(runner) = runner_path() else {
        eprintln!("shiki-tabular-runner 未ビルドのためスキップ");
        return;
    };
    let executor = h.executor_with_tabular(&runner);
    // declared_scopes に csv.read を含めない → 保存時 V3 か実行時 scope 天井で弾かれる。
    let ir = json!({
        "ir_version": 1,
        "name": "csv-query-noscope",
        "declared_scopes": ["storage.write"],
        "nodes": [
            { "id": "q", "type": "csv.query", "params": {
                "file": { "$from": "input", "path": "/file" },
                "sql": "SELECT 1" } }
        ],
        "edges": []
    });
    // 保存が通る場合でも実行時に out_of_scope で失敗する（多層防御）。保存自体が弾く実装なら
    // create が Err になるため、その場合も期待どおり（csv.read 未宣言では実行できない）。
    let Ok((wf_id, _)) = h
        .workflows
        .create(&h.alice, &ir, &Catalog::default(), None)
        .await
    else {
        return; // 保存時 V(scope) で拒否＝期待どおり。
    };
    let (run_id, status) = h
        .run(wf_id, &json!({ "file": Uuid::nil().to_string() }), executor)
        .await;
    assert_eq!(status, RunStatus::Failed, "csv.read 未宣言では失敗する");
    let steps = h.step_map(run_id).await;
    assert_eq!(steps.get("q"), Some(&StepStatus::Failed));
}

// ------- Test 8: http.request の Content-Type（GAP-19）-----------------------

/// 受けた要求（ヘッダと本文の生の文字列）を溜めるローカル HTTP サーバ。返り値は (base URL, 溜まった要求)。
async fn spawn_http_recorder() -> (String, Arc<tokio::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let store = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                // ヘッダの終わりまで読み、Content-Length の分だけ本文を読む。
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        if raw.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                store
                    .lock()
                    .await
                    .push(String::from_utf8_lossy(&raw).into_owned());
                let body = br#"{"ok":true}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body).await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

/// オブジェクト・配列の本文は `Content-Type: application/json` で送る（script 経路の
/// `Shiki.http.request` と揃える）。文字列の本文には種類を付けない。
#[tokio::test]
async fn http_request_sends_object_bodies_as_json() {
    let Some(h) = setup().await else { return };
    let (base, seen) = spawn_http_recorder().await;
    let executor = h.executor(None, vec!["127.0.0.1".into()], None);
    let ir = json!({
        "ir_version": 1, "name": "http-content-type",
        "declared_scopes": ["http.egress"],
        "nodes": [
            { "id": "obj", "type": "http.request", "params": {
                "method": "POST", "url": format!("{base}/obj"),
                "body": { "text": { "$from": "input", "path": "/msg" } } } },
            { "id": "arr", "type": "http.request", "params": {
                "method": "POST", "url": format!("{base}/arr"), "body": [1, 2] } },
            { "id": "str", "type": "http.request", "params": {
                "method": "POST", "url": format!("{base}/str"), "body": "plain text" } }
        ],
        "edges": [ { "from": "obj", "to": "arr" }, { "from": "arr", "to": "str" } ]
    });
    let (_run, status) = h
        .run_to_completion(&ir, &json!({ "msg": "こんにちは" }), executor)
        .await;
    assert_eq!(status, RunStatus::Succeeded);

    let seen = seen.lock().await;
    let request = |path: &str| {
        seen.iter()
            .find(|r| r.starts_with(&format!("POST /{path} ")))
            .unwrap_or_else(|| panic!("/{path} への要求が無い: {seen:?}"))
            .to_ascii_lowercase()
    };
    let obj = request("obj");
    assert!(obj.contains("content-type: application/json"), "{obj}");
    let raw_obj = seen.iter().find(|r| r.starts_with("POST /obj ")).unwrap();
    assert!(raw_obj.ends_with(r#"{"text":"こんにちは"}"#), "{raw_obj}");
    assert!(request("arr").contains("content-type: application/json"));
    let plain = request("str");
    assert!(!plain.contains("content-type"), "{plain}");
}

// ------- Test 9: llm.invoke の誤りの文言（GAP-41）----------------------------

/// 使えないモデルの誤りは、実行履歴に利用者向けの日本語で残る（gateway の英語の原文は出さない）。
#[tokio::test]
async fn llm_model_errors_are_recorded_in_japanese() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let ir = json!({
        "ir_version": 1, "name": "llm-unknown-model",
        "declared_scopes": [],
        "nodes": [
            { "id": "ai", "type": "llm.invoke",
              "params": { "model": "no-such-model", "prompt": "hello" } }
        ],
        "edges": []
    });
    // 保存時の照合はカタログが空なので通る（実行時に gateway が拒否する経路を見る）。
    let (run_id, status) = h.run_to_completion(&ir, &json!({}), executor).await;
    assert_eq!(status, RunStatus::Failed);
    let err: Option<Value> = sqlx::query_scalar(
        "SELECT error FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'ai'",
    )
    .bind(&h.tenant)
    .bind(run_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    let err = err.expect("step の誤り");
    assert_eq!(err["code"], "model_unavailable");
    assert_eq!(
        err["message"],
        "AI モデル「no-such-model」は登録されていません。\
         ノードの設定でモデルを選び直すか、「組織の既定に従う」にしてください。"
    );
    // 非再試行（設定を直すまで通らない）なので 1 回で確定する。
    let attempts: i32 = sqlx::query_scalar(
        "SELECT attempt FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'ai'",
    )
    .bind(&h.tenant)
    .bind(run_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(attempts, 1);
}

// ------- Test 10: llm.invoke のモデルの解決（BUG-12）----------------------------

impl Harness {
    /// 組織の AI 設定を変える（管理画面の保存と同じ経路）。
    async fn set_llm_settings(&self, patch: llm_gateway::SettingsPatch) {
        self.gateway
            .update_tenant_settings(&self.alice, &patch)
            .await
            .unwrap()
            .unwrap();
    }

    /// ノード `ai` の step の出力と誤り。
    async fn step_output(&self, run_id: Uuid) -> (Value, Value) {
        let (output, error): (Option<Value>, Option<Value>) = sqlx::query_as(
            "SELECT output, error FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'ai'",
        )
        .bind(&self.tenant)
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (output.unwrap_or(Value::Null), error.unwrap_or(Value::Null))
    }

    /// このテナントのワークフローの会計の行のモデル名（記録順）。
    async fn workflow_usage_models(&self) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT model FROM llm_usage WHERE tenant_id = $1 AND source = 'workflow' ORDER BY id",
        )
        .bind(&self.tenant)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn llm_ir(name: &str, model: Option<&str>) -> Value {
    let mut params = json!({ "prompt": "hello" });
    if let Some(m) = model {
        params["model"] = json!(m);
    }
    json!({
        "ir_version": 1, "name": name, "declared_scopes": [],
        "nodes": [{ "id": "ai", "type": "llm.invoke", "params": params }],
        "edges": []
    })
}

/// モデルを省略した llm.invoke は、デプロイ既定（stub）ではなく組織の既定で動き、出力・会計に同じ名前が残る。
/// ワークフローの実行主体（schedule / event の run）でも同じモデルになる。
#[tokio::test]
async fn llm_omitted_model_follows_the_organization_default() {
    let Some(h) = setup().await else { return };
    h.set_llm_settings(llm_gateway::SettingsPatch {
        default_model: Some("stub-smart".into()),
        ..Default::default()
    })
    .await;
    let (run_id, status) = h
        .run_to_completion(
            &llm_ir("llm-org-default", None),
            &json!({}),
            h.executor(None, vec![], None),
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded);
    let (output, _) = h.step_output(run_id).await;
    assert_eq!(output["model"], "stub-smart", "{output}");
    assert_eq!(h.workflow_usage_models().await, ["stub-smart"]);

    // Auto＋振り先の上書き → 上書き先。実行主体がワークフロー（委譲で動く run）でも組織の設定は同じ。
    h.set_llm_settings(llm_gateway::SettingsPatch {
        default_model: Some(llm_gateway::AUTO_MODEL.into()),
        auto_model: Some(Some("stub-smart".into())),
        ..Default::default()
    })
    .await;
    let ec = workflow_engine::ExecCtx {
        tenant_id: h.tenant.clone(),
        org: h.org.clone(),
        principal: "wf-schedule".into(),
        principal_kind: "workflow".into(),
        trace_id: None,
        run_id: None,
        step_path: Some("ai".into()),
    };
    let out = workflow_engine::NodePorts::llm_invoke(
        h.ports(None, None).as_ref(),
        &ec,
        workflow_engine::LlmInvokeReq {
            model: None,
            system: None,
            prompt: "hello".into(),
            max_tokens: None,
            idempotency_key: format!("it:{}", Uuid::new_v4()),
            effort: None,
            output_schema: None,
            repair: None,
            call_suffix: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(out["model"], "stub-smart");
    assert_eq!(
        h.workflow_usage_models().await,
        ["stub-smart", "stub-smart"]
    );
}

/// 明示したモデルを管理者が無効にすると、組織の既定へ戻さずに非再試行で失敗し、生成も会計も起きない。
#[tokio::test]
async fn llm_explicit_model_fails_closed_once_disabled() {
    let Some(h) = setup().await else { return };
    // executor は組むのが重い（script engine）ので 1 つを使い回す。
    let executor = h.executor(None, vec![], None);
    let wf_id = h.save(&llm_ir("llm-pinned", Some("stub-smart"))).await;
    h.set_llm_settings(llm_gateway::SettingsPatch {
        models: vec![("stub-smart".into(), false)],
        ..Default::default()
    })
    .await;
    let (run_id, status) = h.run(wf_id, &json!({}), Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Failed);
    let (_, err) = h.step_output(run_id).await;
    assert_eq!(err["code"], "model_unavailable", "{err}");
    assert_eq!(
        err["message"],
        "AI モデル「stub-smart」はこの組織では使えません。\
         ノードの設定でモデルを選び直すか、「組織の既定に従う」にしてください。"
    );
    assert!(h.workflow_usage_models().await.is_empty());

    // 組織がモデルの選択を禁じると、既定と違うモデルは失敗する（省略したものは既定で動く）。
    h.set_llm_settings(llm_gateway::SettingsPatch {
        models: vec![("stub-smart".into(), true)],
        default_model: Some("stub".into()),
        allow_user_choice: Some(false),
        ..Default::default()
    })
    .await;
    let (run_id, status) = h.run(wf_id, &json!({}), Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Failed);
    assert_eq!(
        h.step_output(run_id).await.1["code"],
        "model_choice_disabled"
    );
    let (run_id, status) = h
        .run_to_completion(&llm_ir("llm-locked-default", None), &json!({}), executor)
        .await;
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(h.step_output(run_id).await.0["model"], "stub");
}

// ------- Test 11: llm.invoke の構造化出力（GAP-13）----------------------------

impl Harness {
    /// このテナントの会計の行の冪等キー（記録順）。
    async fn usage_keys(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT idempotency_key FROM llm_usage WHERE tenant_id = $1 ORDER BY id")
            .bind(&self.tenant)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }

    /// テストで作った会計の行を消す（次の確かめの数を汚さない）。
    async fn clear_usage(&self) {
        sqlx::query("DELETE FROM llm_usage WHERE tenant_id = $1")
            .bind(&self.tenant)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// このテストのテナントに作った run と会計の行を消す（run を消せば step は FK で消える）。
    async fn cleanup_runs(&self) {
        self.clear_usage().await;
        sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1")
            .bind(&self.tenant)
            .execute(&self.pool)
            .await
            .unwrap();
    }
}

/// 出力の形（問い合わせの仕分け）を書いた llm.invoke の IR。
fn structured_ir(name: &str, prompt: &str) -> Value {
    json!({
        "ir_version": 1, "name": name, "declared_scopes": [],
        "nodes": [{ "id": "ai", "type": "llm.invoke", "params": {
            "prompt": prompt, "effort": "low",
            "output_schema": { "type": "object", "required": ["category", "summary"],
                "properties": {
                    "category": { "type": "string", "enum": ["billing", "bug", "howto"] },
                    "summary": { "type": "string" } } } } }],
        "edges": []
    })
}

/// 本番のポートと stub で、構造化出力が data に載る。直させた経路・失敗の経路の会計のキーは
/// 成功が step のキー（修復は #c1）、使えなかった呼び出しが呼び出しごとに一意な #f キーになる。
#[tokio::test]
async fn llm_structured_output_through_the_production_port() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let step_key = |run_id: Uuid| format!("wf:{}:{run_id}:ai", h.tenant);

    // 形に合う出力（stub は prompt に現れた選択肢を選ぶ）。会計は step のキーの 1 行。
    let (run_id, status) = h
        .run_to_completion(
            &structured_ir("llm-structured-ok", "アプリが落ちる bug の報告です"),
            &json!({}),
            Arc::clone(&executor),
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded);
    let (output, _) = h.step_output(run_id).await;
    assert_eq!(
        output["data"],
        json!({ "category": "bug", "summary": "" }),
        "{output}"
    );
    assert_eq!(output["model"], "stub");
    assert!(output.get("stop_reason").is_none(), "{output}");
    assert_eq!(
        serde_json::from_str::<Value>(output["text"].as_str().unwrap()).unwrap(),
        output["data"]
    );
    assert_eq!(h.usage_keys().await, [step_key(run_id)]);
    h.clear_usage().await;

    // 1 回目が JSON でない → 直させて成功。1 回目は使えなかった呼び出し（#f）、修復は #c1。
    let (run_id, status) = h
        .run_to_completion(
            &structured_ir(
                "llm-structured-repair",
                "json:申し訳ありません、分類できません",
            ),
            &json!({}),
            Arc::clone(&executor),
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(h.step_output(run_id).await.0["data"]["category"], "billing");
    let keys = h.usage_keys().await;
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(
        keys[0].starts_with(&format!("{}#f", step_key(run_id))),
        "{keys:?}"
    );
    assert_eq!(keys[1], format!("{}#c1", step_key(run_id)));
    h.clear_usage().await;

    // 直しても合わない → llm_output_invalid（再試行しない）。どちらの呼び出しも #f で別々に残る。
    let (run_id, status) = h
        .run_to_completion(
            &structured_ir(
                "llm-structured-invalid",
                r#"jsonalways:{"category":"refund"}"#,
            ),
            &json!({}),
            Arc::clone(&executor),
        )
        .await;
    assert_eq!(status, RunStatus::Failed);
    let (_, err) = h.step_output(run_id).await;
    assert_eq!(err["code"], "llm_output_invalid", "{err}");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("category: 選択肢にない値です"),
        "{err}"
    );
    let keys = h.usage_keys().await;
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(keys
        .iter()
        .all(|k| k.starts_with(&format!("{}#f", step_key(run_id)))));
    assert_ne!(keys[0], keys[1]);
    h.clear_usage().await;

    // 拒否・上限で切れた出力は直さずに失敗する（1 回だけ呼ぶ）。
    for (prompt, code) in [
        ("refuse:経費の不正を判定して", "llm_refused"),
        ("truncate:長い出力", "llm_output_truncated"),
    ] {
        let (run_id, status) = h
            .run_to_completion(
                &structured_ir(
                    &format!("llm-structured-{}", &code[4..]).replace('_', "-"),
                    prompt,
                ),
                &json!({}),
                Arc::clone(&executor),
            )
            .await;
        assert_eq!(status, RunStatus::Failed, "{code}");
        assert_eq!(h.step_output(run_id).await.1["code"], code);
        let keys = h.usage_keys().await;
        assert_eq!(keys.len(), 1, "{code}: {keys:?}");
        assert!(keys[0].contains("#f"), "{keys:?}");
        h.clear_usage().await;
    }
    h.cleanup_runs().await;
}

/// 同じ step のキーで、使えなかった呼び出しの後に成功しても、会計は 2 行になる（失敗を step のキーで
/// 記録すると、再試行・再開の成功が重複除去で消える・R2）。
#[tokio::test]
async fn llm_failure_then_success_on_the_same_step_keeps_both_usage_rows() {
    let Some(h) = setup().await else { return };
    let ec = workflow_engine::ExecCtx {
        tenant_id: h.tenant.clone(),
        org: h.org.clone(),
        principal: "alice".into(),
        principal_kind: "user".into(),
        trace_id: None,
        run_id: None,
        step_path: Some("ai".into()),
    };
    let key = format!("wf:{}:{}:ai", h.tenant, Uuid::new_v4());
    let schema = json!({ "type": "object", "required": ["category"], "additionalProperties": false,
                         "properties": { "category": { "type": "string", "enum": ["a", "b"] } } });
    let call = |prompt: &str| workflow_engine::LlmInvokeReq {
        model: None,
        system: None,
        prompt: prompt.into(),
        max_tokens: None,
        idempotency_key: key.clone(),
        effort: None,
        output_schema: Some(schema.clone()),
        repair: None,
        call_suffix: None,
    };
    let ports = h.ports(None, None);
    let refused = workflow_engine::NodePorts::llm_invoke(ports.as_ref(), &ec, call("refuse:x"))
        .await
        .unwrap();
    assert_eq!(refused["stop_reason"], "refusal");
    let ok = workflow_engine::NodePorts::llm_invoke(ports.as_ref(), &ec, call("b を選ぶ"))
        .await
        .unwrap();
    assert_eq!(ok["stop_reason"], "end_turn");
    let keys = h.usage_keys().await;
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(keys[0].starts_with(&format!("{key}#f")), "{keys:?}");
    assert_eq!(keys[1], key);
    h.cleanup_runs().await;
}

// ------- Test 12: ai.decide（FEAT-1）-------------------------------------------

/// 問い合わせの種類で振り分ける判定（値で・しきい値 0.8）と、出口ごとの合流ノード。
fn decide_ir(name: &str, model: &str) -> Value {
    json!({
        "ir_version": 1, "name": name, "declared_scopes": [],
        "nodes": [
            { "id": "ai", "type": "ai.decide", "params": {
                "model": model,
                "state": { "$from": "input", "path": "/body" },
                "questions": [
                    { "key": "category", "type": "choice", "instructions": "問い合わせの種類はどれか",
                      "options": [ { "key": "billing", "description": "請求・返金" },
                                   { "key": "bug", "description": "不具合" } ] },
                    { "key": "angry", "type": "boolean", "instructions": "強い不満を示しているか" } ],
                "route": { "question": "category", "min_confidence": 0.8 } } },
            { "id": "on_billing", "type": "control.join", "params": {} },
            { "id": "on_bug", "type": "control.join", "params": {} },
            { "id": "on_uncertain", "type": "control.join", "params": {} }
        ],
        "edges": [
            { "from": "ai", "from_port": "billing", "to": "on_billing" },
            { "from": "ai", "from_port": "bug", "to": "on_bug" },
            { "from": "ai", "from_port": "uncertain", "to": "on_uncertain" }
        ]
    })
}

/// 本番のポートと stub の決定器で、答えの出口へ進み、ほかの出口の先は skip される。会計はチョークポイントが
/// step のキーで 1 行だけ記録する（使えなかった判定は #f）。
#[tokio::test]
async fn decide_routes_through_the_production_port() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let step_key = |run_id: Uuid| format!("wf:{}:{run_id}:ai", h.tenant);
    let wf_id = h.save(&decide_ir("decide-route", "decide-stub")).await;

    // 材料に選択肢のキーがある → stub は確からしさ 0.9 で選ぶ → billing の出口。
    let (run_id, status) = h
        .run(
            wf_id,
            &json!({ "body": "billing の件で連絡しました" }),
            Arc::clone(&executor),
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded);
    let steps = h.step_map(run_id).await;
    assert_eq!(steps["on_billing"], StepStatus::Succeeded, "{steps:?}");
    assert_eq!(steps["on_bug"], StepStatus::Skipped);
    assert_eq!(steps["on_uncertain"], StepStatus::Skipped);
    let (output, _) = h.step_output(run_id).await;
    assert_eq!(
        output["answers"]["category"]["value"], "billing",
        "{output}"
    );
    assert_eq!(output["answers"]["category"]["label"], "請求・返金");
    assert_eq!(output["answers"]["category"]["confidence"], 0.9);
    assert_eq!(output["answers"]["angry"]["type"], "boolean");
    assert_eq!(
        output["routed"],
        json!({ "question": "category", "port": "billing" })
    );
    assert_eq!(output["model"], "decide-stub");
    assert_eq!(output["calibrated"], false);
    assert!(output["resolved_model"]
        .as_str()
        .is_some_and(|m| !m.is_empty()));
    assert_eq!(h.usage_keys().await, [step_key(run_id)]);
    assert_eq!(h.workflow_usage_models().await, ["decide-stub"]);
    h.clear_usage().await;

    // 手がかりが無い → 先頭を 0.4 で選ぶ → しきい値に届かず uncertain。
    let (run_id, status) = h
        .run(
            wf_id,
            &json!({ "body": "よく分からない" }),
            Arc::clone(&executor),
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded);
    let steps = h.step_map(run_id).await;
    assert_eq!(steps["on_uncertain"], StepStatus::Succeeded, "{steps:?}");
    assert_eq!(steps["on_billing"], StepStatus::Skipped);
    assert_eq!(h.step_output(run_id).await.0["routed"]["port"], "uncertain");
    h.clear_usage().await;

    // 決定器の失敗（到達障害・再試行の分類）→ step は失敗し、使った分は #f で別に残る。
    let (run_id, status) = h
        .run(
            wf_id,
            &json!({ "body": "stub:fail" }),
            Arc::clone(&executor),
        )
        .await;
    assert_eq!(status, RunStatus::Failed);
    let (_, err) = h.step_output(run_id).await;
    assert_eq!(err["code"], "unavailable", "{err}");
    assert_eq!(err["retryable"], true, "{err}");
    assert!(!err["message"].as_str().unwrap().contains("stub"), "{err}");
    let keys = h.usage_keys().await;
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert!(
        keys[0].starts_with(&format!("{}#f", step_key(run_id))),
        "{keys:?}"
    );
    h.cleanup_runs().await;
}

/// 管理者が判定用のモデル（または元にする chat モデル）を止めると、実行時にも非再試行で失敗し、判定も会計も
/// 起きない（保存時の照合の後に変わっても fail-closed）。元に戻すと LLM に判定させるモデルも動く。
#[tokio::test]
async fn decide_fails_closed_once_the_model_or_its_base_is_disabled() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let input = json!({ "body": "billing の件" });

    let wf_stub = h.save(&decide_ir("decide-off", "decide-stub")).await;
    h.set_llm_settings(llm_gateway::SettingsPatch {
        models: vec![("decide-stub".into(), false)],
        ..Default::default()
    })
    .await;
    let (run_id, status) = h.run(wf_stub, &input, Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Failed);
    let (_, err) = h.step_output(run_id).await;
    assert_eq!(err["code"], "model_unavailable", "{err}");
    assert_eq!(err["retryable"], false);
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .starts_with("判定用のモデル「decide-stub」はこの組織では使えません"),
        "{err}"
    );

    // 元にする chat モデル（stub-smart）を止めると、判定用のモデル（decide-via）は許可のままでも使えない。
    let wf_via = h.save(&decide_ir("decide-via-off", "decide-via")).await;
    h.set_llm_settings(llm_gateway::SettingsPatch {
        models: vec![("stub-smart".into(), false)],
        ..Default::default()
    })
    .await;
    let (run_id, status) = h.run(wf_via, &input, Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Failed);
    let (_, err) = h.step_output(run_id).await;
    assert_eq!(err["code"], "model_unavailable", "{err}");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("元になる AI モデル「stub-smart」"),
        "{err}"
    );
    assert!(h.usage_keys().await.is_empty(), "判定も会計も起きない");

    // 戻すと、stub-smart に判定させる決定器（llm_decider）で動き、会計は元のモデルの名前で付く。
    h.set_llm_settings(llm_gateway::SettingsPatch {
        models: vec![("stub-smart".into(), true)],
        ..Default::default()
    })
    .await;
    let (run_id, status) = h.run(wf_via, &input, Arc::clone(&executor)).await;
    assert_eq!(
        status,
        RunStatus::Succeeded,
        "{:?}",
        h.step_output(run_id).await
    );
    let (output, _) = h.step_output(run_id).await;
    assert_eq!(output["model"], "decide-via");
    assert_eq!(output["method"], "verbalized", "{output}");
    assert!(
        output["resolved_model"]
            .as_str()
            .unwrap()
            .contains("+llm_decider/"),
        "{output}"
    );
    assert_eq!(h.workflow_usage_models().await, ["stub-smart"]);
    h.cleanup_runs().await;
}
