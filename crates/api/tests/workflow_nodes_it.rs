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

/// 参照先を確かめない RefResolver（data を続きの位置の封にだけ使う・D-131）。
struct NoRefs;

#[async_trait::async_trait]
impl data::RefResolver for NoRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

struct Harness {
    /// このハーネスの OpenFGA のストア（片付けるテストが消す・[`Harness::delete_fga_store`]）。
    fga_url: String,
    fga_store: String,
    fga: Arc<dyn AuthzClient>,
    pool: sqlx::PgPool,
    storage: Arc<storage::StorageService>,
    gateway: Arc<llm_gateway::LlmGateway>,
    org: String,
    tenant: String,
    alice: AuthContext,
    artifacts: Arc<artifact::ArtifactStore>,
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
    let fga_client = OpenFgaClient::connect(
        http.clone(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("shiki-wf-nodes-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("fga");
    let fga_store = fga_client.store_id().to_string();
    let fga = Arc::new(fga_client) as Arc<dyn AuthzClient>;

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
                // stub-smart は組織の既定の切り替え（BUG-12）を見るための 2 つ目のモデル。stub-think は思考の深さに
                // 対応するモデル（ノードの思考の深さがモデルへ届くかを見る・D-75）。判定用のモデルは
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
                                id: "stub-think".into(),
                                supports_effort: Some(true),
                                ..Default::default()
                            },
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
        fga_url,
        fga_store,
        fga: Arc::clone(&fga),
        pool,
        storage,
        gateway,
        org,
        tenant,
        alice,
        artifacts,
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
            http: api::workflow_runtime::HttpEgress::default(),
            db: self.pool.clone(),
            data: None,
            fsms: None,
            web_search: None,
        })
    }

    /// 本番 executor を組む（FakeSandbox＋http allowlist を差し込める）。送り口の内部の送り先
    /// （`workflow.http_internal_hosts`）には、許可と同じホストを書く（ループバックの相手は `127.0.0.1` を許可に
    /// 書く・D-135）。内部の送り先を許可と分けて決めるときは [`Self::executor_with_http`] を使う。
    fn executor(
        &self,
        sandbox: Option<Arc<dyn sandbox_client::Sandbox>>,
        http_allowlist: Vec<String>,
        search: Option<Arc<rag::SearchService>>,
    ) -> Arc<dyn NodeExecutor> {
        let http = api::workflow_runtime::HttpEgress::new(&http_allowlist);
        self.executor_with_http(sandbox, http_allowlist, search, http)
    }

    /// [`Self::executor`] の送り口（名前解決）を差し替えられる形（BUG-181）。
    fn executor_with_http(
        &self,
        sandbox: Option<Arc<dyn sandbox_client::Sandbox>>,
        http_allowlist: Vec<String>,
        search: Option<Arc<rag::SearchService>>,
        http: api::workflow_runtime::HttpEgress,
    ) -> Arc<dyn NodeExecutor> {
        let mut ports = Arc::into_inner(self.ports(sandbox, search)).expect("ports");
        ports.http = http;
        let ports = Arc::new(ports);
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

    /// 本番 executor に構造化データ（続きの位置の封・D-131）を結線した形。続きの位置を同じ鍵で作れるよう、data も返す。
    fn executor_with_data(&self) -> (Arc<dyn NodeExecutor>, Arc<data::DataStore>) {
        let data = Arc::new(data::DataStore::new(
            self.pool.clone(),
            Arc::clone(&self.fga),
            Arc::new(NoRefs),
        ));
        let mut ports = Arc::into_inner(self.ports(None, None)).expect("ports");
        ports.data = Some(Arc::clone(&data));
        let exec = Arc::new(CapabilityNodeExecutor::new(
            Arc::new(ports),
            EffectJournal::new(self.pool.clone()),
            Arc::new(NoopAudit),
        ));
        (exec, data)
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
            http: api::workflow_runtime::HttpEgress::default(),
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

    /// このハーネスの OpenFGA のストアを消す（テストの最後の片付け・タプルはストアごと消える）。
    async fn delete_fga_store(&self) {
        let resp = reqwest::Client::new()
            .delete(format!("{}/stores/{}", self.fga_url, self.fga_store))
            .send()
            .await
            .expect("OpenFGA delete store");
        assert!(
            resp.status().is_success(),
            "delete store: {}",
            resp.status()
        );
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
/// rag クレートの結合テストが担保）。ここでは rag 未構成時に fail-closed することを確認。権限の誤り（forbidden）では
/// なく、この環境で使えない機能の誤り（not_configured・管理者に連絡する・BUG-175）にする。
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
        "rag.search が構成されていない誤りで失敗"
    );
    let (code, message, retryable): (Option<String>, Option<String>, Option<bool>) =
        sqlx::query_as(
            "SELECT error->>'code', error->>'message', (error->>'retryable')::bool \
             FROM step_execution WHERE run_id = $1 AND node_id = 'q'",
        )
        .bind(run_id)
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(
        (code.as_deref(), retryable),
        (
            Some(workflow_engine::nodes::ports::NOT_CONFIGURED),
            Some(false)
        )
    );
    assert_eq!(
        message.as_deref(),
        Some("この環境では社内文書の検索を使えません。管理者に連絡してください。")
    );
    assert_eq!(
        workflow_engine::run::failure::category_of(workflow_engine::nodes::ports::NOT_CONFIGURED),
        workflow_engine::FailureCategory::Environment
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

/// storage.read は文章として読めるファイルだけを読む（BUG-101）。PDF・Word などの種類（MIME か拡張子）と、
/// 文字コードの分からない中身・文章でない中身は `not_text_file`（やり直さない）で失敗し、文字化けした中身を
/// 成功として後ろへ渡さない。Excel が保存する Shift_JIS・BOM 付きの UTF-8・UTF-16LE の CSV は読み、読んだ
/// 文字コードを結果の `encoding` に残す（GAP-45）。
#[tokio::test]
async fn storage_read_refuses_files_that_are_not_text() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let ir = json!({
        "ir_version": 1, "name": "read-text-only",
        "declared_scopes": ["storage.read"],
        "triggers": [{ "kind": "interactive" }],
        "nodes": [
            { "id": "rd", "type": "storage.read", "params": { "file": { "$from": "input", "path": "/file" } } }
        ],
        "edges": []
    });
    let wf_id = h.save(&ir).await;
    let csv = "経費,金額\n交通費,1200\n";
    let (sjis, _, _) = textenc::encoding_rs::SHIFT_JIS.encode(csv);
    let bom8 = [b"\xEF\xBB\xBF".as_slice(), csv.as_bytes()].concat();
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(csv.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let files: [(&str, &[u8], &str); 9] = [
        ("メモ.txt", "経費の申請\n".as_bytes(), "text/plain"),
        // 日本語の Windows の Excel が既定で保存する CSV（ブラウザーは ms-excel の MIME を付ける）。
        ("経費.csv", &sjis, "application/vnd.ms-excel"),
        ("経費-utf8.csv", &bom8, "text/csv"),
        ("経費-unicode.txt", &utf16, "text/plain"),
        (
            "契約書.pdf",
            b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj",
            "application/pdf",
        ),
        // MIME が分からなくても拡張子で断る（中身を読み込まない）。
        (
            "報告.docx",
            b"PK\x03\x04\x14\x00\x06\x00",
            "application/octet-stream",
        ),
        // テキストの種類でも、文字コードの分からない中身（西欧の windows-1252 の café）は断る。
        ("latin1.txt", b"caf\xe9\n", "text/plain"),
        // NUL を含む中身は文章にしない。
        ("data.log", b"abc\x00def", "application/octet-stream"),
        // Shift_JIS として読めても、NUL を含む中身は文章にしない（NUL のない制御文字は UTF-8 と同じく文章として読む）。
        ("bin.txt", b"\x8c\x6f\x94\xef\x00\x01\x02", "text/plain"),
    ];
    let mut outcomes = Vec::new();
    let mut written = Vec::new();
    for (name, bytes, ct) in files {
        let node = h
            .storage
            .write_file_internal(&h.alice, None, name, bytes, ct, None)
            .await
            .expect("upload");
        written.push(node.id);
        let (run_id, status) = h
            .run(
                wf_id,
                &json!({ "file": node.id.to_string() }),
                Arc::clone(&executor),
            )
            .await;
        let (code, retryable, output): (Option<String>, Option<bool>, Option<Value>) =
            sqlx::query_as(
                "SELECT error->>'code', (error->>'retryable')::bool, output FROM step_execution \
                 WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
            )
            .bind(&h.tenant)
            .bind(run_id)
            .fetch_one(&h.pool)
            .await
            .unwrap();
        outcomes.push((name, status, code, retryable, output));
    }
    // 片付け（run の履歴・アップロードしたファイル・ワークフロー）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for id in written {
        h.storage
            .soft_delete_file(&h.alice, id, None)
            .await
            .unwrap();
    }
    h.artifacts.delete(&h.alice, wf_id, None).await.unwrap();

    for (i, (text, encoding)) in [
        ("経費の申請\n", "UTF-8"),
        (csv, "Shift_JIS"),
        (csv, "UTF-8"),
        (csv, "UTF-16LE"),
    ]
    .into_iter()
    .enumerate()
    {
        let (name, status, code, _, output) = &outcomes[i];
        assert_eq!(
            (*status, code.as_deref()),
            (RunStatus::Succeeded, None),
            "{name}"
        );
        let output = output.as_ref().unwrap();
        assert_eq!(output["text"], text, "{name}");
        assert_eq!(output["encoding"], encoding, "{name}");
    }
    for (name, status, code, retryable, _) in &outcomes[4..] {
        // PDF・Word は社内文書の検索の本文で読む。この executor は社内文書の検索を結線しないので、この環境で使えない
        // 機能の誤り（search_disabled・BUG-158）。文章の種類で読めない中身は not_text_file。
        let want = if name.ends_with(".pdf") || name.ends_with(".docx") {
            workflow_engine::nodes::ports::SEARCH_DISABLED
        } else {
            workflow_engine::nodes::ports::NOT_TEXT_FILE
        };
        assert_eq!(*status, RunStatus::Failed, "{name}");
        assert_eq!(
            (code.as_deref(), *retryable),
            (Some(want), Some(false)),
            "{name}"
        );
    }
    assert_eq!(
        workflow_engine::run::failure::category_of(workflow_engine::nodes::ports::NOT_TEXT_FILE),
        workflow_engine::FailureCategory::Configuration
    );
    assert_eq!(
        workflow_engine::run::failure::category_of(workflow_engine::nodes::ports::SEARCH_DISABLED),
        workflow_engine::FailureCategory::Environment
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
    // 集計した CSV のバージョンが結果に載る（後ろの「CSV を編集」の前提にできる・D-96）。
    let out: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(out["version"], node.version, "{out}");
}

/// csv.* のブロックは、Excel が保存する文字コードの CSV を読む（GAP-45）。Shift_JIS（日本語の Windows の既定）・
/// BOM 付きの UTF-8・UTF-16LE（BOM 付き）の「経費,金額」の CSV を「CSV を集計」が日本語の列名で集計でき、
/// 「CSV を編集」は元の文字コードのまま書き戻す。元の文字コードで書けない文字は書かずに失敗する（中身を変えない）。
#[tokio::test]
async fn csv_nodes_read_excel_encodings_and_keep_the_encoding_when_patching() {
    use textenc::encoding_rs::{SHIFT_JIS, UTF_16LE};
    let Some(h) = setup().await else { return };
    let Some(runner) = runner_path() else {
        eprintln!("shiki-tabular-runner 未ビルドのためスキップ（SHIKI_TABULAR_RUNNER 参照）");
        return;
    };
    let csv = "経費,金額\n交通費,1200\n宿泊費,8000\n";
    let (sjis, _, _) = SHIFT_JIS.encode(csv);
    let bom8 = [b"\xEF\xBB\xBF".as_slice(), csv.as_bytes()].concat();
    let utf16 = textenc::encode_like(csv, UTF_16LE, true).unwrap();
    let mut written = Vec::new();
    for (name, bytes, ct) in [
        ("経費-sjis.csv", sjis.as_ref(), "application/vnd.ms-excel"),
        ("経費-utf8.csv", bom8.as_slice(), "text/csv"),
        ("経費-utf16.csv", utf16.as_slice(), "text/csv"),
    ] {
        let node = h
            .storage
            .write_file_internal(&h.alice, None, name, bytes, ct, None)
            .await
            .expect("csv アップロード");
        written.push(node);
    }
    let step = |run_id: Uuid| {
        let pool = h.pool.clone();
        let tenant = h.tenant.clone();
        async move {
            let row: (String, Option<Value>, Option<Value>) = sqlx::query_as(
                "SELECT status, output, error FROM step_execution \
                 WHERE tenant_id = $1 AND run_id = $2 ORDER BY node_id LIMIT 1",
            )
            .bind(&tenant)
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            row
        }
    };

    let query = h
        .save(&json!({
            "ir_version": 1, "name": "csv-encodings",
            "declared_scopes": ["csv.read"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [ { "id": "q", "type": "csv.query", "params": {
                "file": { "$from": "input", "path": "/file" },
                "sql": "SELECT \"経費\", sum(\"金額\") AS 合計 FROM data GROUP BY \"経費\" ORDER BY 合計" } } ],
            "edges": []
        }))
        .await;
    let mut queried = Vec::new();
    for node in &written {
        let (run_id, status) = h
            .run(
                query,
                &json!({ "file": node.id.to_string() }),
                h.executor_with_tabular(&runner),
            )
            .await;
        queried.push((node.name.clone(), status, step(run_id).await));
    }

    // Shift_JIS の CSV を編集する（金額を書き換える・元の文字コードのまま保存する）。
    let sjis_file = &written[0];
    let patch = h
        .save(&json!({
            "ir_version": 1, "name": "csv-patch-sjis",
            "declared_scopes": ["csv.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [ { "id": "p", "type": "csv.patch", "params": {
                "file": { "$from": "input", "path": "/file" },
                "base_rev": { "$from": "input", "path": "/rev" },
                "ops": { "$from": "input", "path": "/ops" } } } ],
            "edges": []
        }))
        .await;
    let patched = h
        .run(
            patch,
            &json!({ "file": sjis_file.id.to_string(), "rev": sjis_file.version,
                     "ops": [ { "op": "cell_update", "row": 0, "col": 1, "value": "1500" } ] }),
            h.executor_with_tabular(&runner),
        )
        .await;
    let patched_step = step(patched.0).await;
    let (_, after_patch) = h
        .storage
        .read_file_internal(&h.alice, sjis_file.id, None)
        .await
        .unwrap();
    // Shift_JIS に無い文字（絵文字）は書かずに失敗し、ファイルは変わらない。
    let refused = h
        .run(
            patch,
            &json!({ "file": sjis_file.id.to_string(), "rev": sjis_file.version + 1,
                     "ops": [ { "op": "cell_update", "row": 0, "col": 0, "value": "交通費😀" } ] }),
            h.executor_with_tabular(&runner),
        )
        .await;
    let refused_step = step(refused.0).await;
    let (unchanged, after_refusal) = h
        .storage
        .read_file_internal(&h.alice, sjis_file.id, None)
        .await
        .unwrap();

    // 片付け（run の履歴・アップロードしたファイル・ワークフロー）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for node in &written {
        h.storage
            .soft_delete_file(&h.alice, node.id, None)
            .await
            .unwrap();
    }
    for wf in [query, patch] {
        h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    }

    for (name, status, (_, output, error)) in &queried {
        assert_eq!(*status, RunStatus::Succeeded, "{name}: {error:?}");
        let output = output.as_ref().unwrap();
        assert_eq!(
            output["columns"],
            json!(["経費", "合計"]),
            "{name}: {output}"
        );
        assert_eq!(
            output["rows"],
            json!([["交通費", "1200"], ["宿泊費", "8000"]]),
            "{name}: {output}"
        );
    }
    assert_eq!(patched.1, RunStatus::Succeeded, "{patched_step:?}");
    assert_eq!(
        patched_step.1.as_ref().unwrap()["version"],
        sjis_file.version + 1
    );
    let edited = textenc::decode_text(&after_patch).unwrap();
    assert_eq!(edited.encoding, SHIFT_JIS, "元の文字コードのまま保存する");
    assert_eq!(edited.text, "経費,金額\n交通費,1500\n宿泊費,8000\n");
    assert_eq!(refused.1, RunStatus::Failed);
    let error = refused_step.2.unwrap();
    assert_eq!(error["code"], "invalid", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("「😀」は、この CSV の文字コード（Shift_JIS）では書けません"),
        "{error}"
    );
    assert_eq!(unchanged.version, sjis_file.version + 1, "書かない");
    assert_eq!(after_refusal, after_patch);
}

/// 「CSV を集計」の結果の items は、1 行ずつ列の名前で値を引け、数・真偽・空を種類のまま渡す。rows は以前からの
/// テキストのまま（日付は以前のデバッグ表示ではなく 2024-04-01 の形）。結果の全体を「CSV を保存」に渡すと、rows の
/// 表から CSV を組み立てる。SQL の末尾のセミコロンでも失敗しない（GAP-40）。
#[tokio::test]
async fn csv_query_items_keep_value_kinds() {
    let Some(runner) = runner_path() else {
        eprintln!("shiki-tabular-runner 未ビルドのためスキップ（SHIKI_TABULAR_RUNNER 参照）");
        return;
    };
    let Some(h) = setup().await else { return };
    let tag = Uuid::new_v4().simple().to_string()[..8].to_string();
    let src = h
        .storage
        .write_file_internal(
            &h.alice,
            None,
            &format!("items-{tag}.csv"),
            "品目,金額,日付\n交通費,1200,2024-04-01\n会議費,,2024-04-02\n".as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("csv アップロード");
    let wf = h
        .save(&json!({
            "ir_version": 1, "name": format!("csv-items-{tag}"),
            "declared_scopes": ["csv.read", "csv.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "q", "type": "csv.query", "params": {
                    "file": src.id.to_string(),
                    "sql": "SELECT 品目, 金額, 金額 > 1000 AS 高い, 日付 FROM data ORDER BY 日付;" } },
                { "id": "save", "type": "csv.write", "params": {
                    "name": format!("items-out-{tag}"),
                    "content": { "$from": "nodes.q.output" } } }
            ],
            "edges": [ { "from": "q", "to": "save" } ]
        }))
        .await;
    let (run_id, status) = h
        .run(wf, &json!({}), h.executor_with_tabular(&runner))
        .await;
    let outputs: Vec<(String, Option<Value>, Option<Value>)> = sqlx::query_as(
        "SELECT node_id, output, error FROM step_execution WHERE tenant_id = $1 AND run_id = $2 ORDER BY node_id",
    )
    .bind(&h.tenant)
    .bind(run_id)
    .fetch_all(&h.pool)
    .await
    .unwrap();
    let saved = h.read_file_by_name(&format!("items-out-{tag}.csv")).await;
    let saved_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM node WHERE tenant_id = $1 AND name = $2 AND deleted_at IS NULL",
    )
    .bind(&h.tenant)
    .bind(format!("items-out-{tag}.csv"))
    .fetch_optional(&h.pool)
    .await
    .unwrap();

    // 片付け（run の履歴・ファイル・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for id in saved_id.into_iter().chain([src.id]) {
        h.storage
            .soft_delete_file(&h.alice, id, None)
            .await
            .unwrap();
    }
    h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    h.delete_fga_store().await;

    assert_eq!(status, RunStatus::Succeeded, "{outputs:?}");
    let q = outputs
        .iter()
        .find(|(id, _, _)| id == "q")
        .and_then(|(_, out, _)| out.clone())
        .expect("q の結果");
    assert_eq!(
        q["items"],
        json!([
            { "品目": "交通費", "金額": 1200, "高い": true, "日付": "2024-04-01" },
            { "品目": "会議費", "金額": null, "高い": null, "日付": "2024-04-02" }
        ]),
        "{q}"
    );
    assert_eq!(
        q["rows"],
        json!([
            ["交通費", "1200", "true", "2024-04-01"],
            ["会議費", null, null, "2024-04-02"]
        ]),
        "{q}"
    );
    assert_eq!(
        q["column_types"],
        json!(["VARCHAR", "BIGINT", "BOOLEAN", "DATE"])
    );
    assert_eq!(q["total_rows"], 2);
    assert_eq!(
        saved.as_deref(),
        Some("品目,金額,高い,日付\r\n交通費,1200,true,2024-04-01\r\n会議費,,,2024-04-02\r\n")
    );
}

/// 「CSV を集計」は Excel（.xlsx）も読む（D-17）。左端のシートを表として読み、いちばん上の行を列の名前にする。
/// 列の型はセルの種類から決め（日付・はい／いいえ・整数・文字の 007）、数式は保存された値を読み、空の行は飛ばす。
/// 古い形式の Excel（.xls など）は理由を言って断る（not_text_file）。「CSV を編集」は Excel を書き換えず、
/// 「CSV を保存」は Excel の名前で CSV を作らない（invalid）。
#[tokio::test]
async fn csv_query_reads_the_leftmost_sheet_of_an_excel_file() {
    let Some(runner) = runner_path() else {
        eprintln!("shiki-tabular-runner 未ビルドのためスキップ（SHIKI_TABULAR_RUNNER 参照）");
        return;
    };
    let Some(h) = setup().await else { return };
    let tag = Uuid::new_v4().simple().to_string()[..8].to_string();
    let xlsx = h
        .storage
        .write_file_internal(
            &h.alice,
            None,
            &format!("経費-{tag}.xlsx"),
            include_bytes!("../../tabular/tests/fixtures/expenses.xlsx"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            None,
        )
        .await
        .expect("xlsx アップロード");
    // 古い形式の Excel（OLE2 の複合ファイルの先頭）。
    let xls = h
        .storage
        .write_file_internal(
            &h.alice,
            None,
            &format!("経費-{tag}.xls"),
            &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0],
            "application/vnd.ms-excel",
            None,
        )
        .await
        .expect("xls アップロード");
    let wf = h
        .save(&json!({
            "ir_version": 1, "name": format!("csv-xlsx-{tag}"),
            "declared_scopes": ["csv.read"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [ { "id": "q", "type": "csv.query", "params": {
                "file": { "$from": "input", "path": "/file" },
                "sql": "SELECT * FROM data ORDER BY 日付" } } ],
            "edges": []
        }))
        .await;
    let step = |run_id: Uuid| {
        let pool = h.pool.clone();
        let tenant = h.tenant.clone();
        async move {
            let row: (Option<Value>, Option<Value>) = sqlx::query_as(
                "SELECT output, error FROM step_execution WHERE tenant_id = $1 AND run_id = $2",
            )
            .bind(&tenant)
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            row
        }
    };
    let read = h
        .run(
            wf,
            &json!({ "file": xlsx.id.to_string() }),
            h.executor_with_tabular(&runner),
        )
        .await;
    let read_step = step(read.0).await;
    let old = h
        .run(
            wf,
            &json!({ "file": xls.id.to_string() }),
            h.executor_with_tabular(&runner),
        )
        .await;
    let old_step = step(old.0).await;
    // 「CSV を編集」は Excel を書き換えない。「CSV を保存」は Excel の名前で CSV を作らない（D-17）。
    let refuse = h
        .save(&json!({
            "ir_version": 1, "name": format!("csv-xlsx-refuse-{tag}"),
            "declared_scopes": ["csv.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "p", "type": "csv.patch", "params": {
                    "file": xlsx.id.to_string(), "base_rev": xlsx.version,
                    "ops": [ { "op": "cell_update", "row": 0, "col": 1, "value": "x" } ] } }
            ],
            "edges": []
        }))
        .await;
    let patched = h
        .run(refuse, &json!({}), h.executor_with_tabular(&runner))
        .await;
    let patched_step = step(patched.0).await;
    let (after, after_bytes) = h
        .storage
        .read_file_internal(&h.alice, xlsx.id, None)
        .await
        .unwrap();
    let write = h
        .save(&json!({
            "ir_version": 1, "name": format!("csv-xlsx-write-{tag}"),
            "declared_scopes": ["csv.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [ { "id": "w", "type": "csv.write", "params": {
                "name": format!("集計-{tag}.xlsx"), "content": "a,b\n1,2\n" } } ],
            "edges": []
        }))
        .await;
    let written = h
        .run(write, &json!({}), h.executor_with_tabular(&runner))
        .await;
    let written_step = step(written.0).await;
    let made: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node WHERE tenant_id = $1 AND name LIKE $2")
            .bind(&h.tenant)
            .bind(format!("集計-{tag}%"))
            .fetch_one(&h.pool)
            .await
            .unwrap();

    // 片付け（run の履歴・ファイル・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for id in [xlsx.id, xls.id] {
        h.storage
            .soft_delete_file(&h.alice, id, None)
            .await
            .unwrap();
    }
    for id in [wf, refuse, write] {
        h.artifacts.delete(&h.alice, id, None).await.unwrap();
    }
    h.delete_fga_store().await;

    assert_eq!(read.1, RunStatus::Succeeded, "{read_step:?}");
    let out = read_step.0.unwrap();
    assert_eq!(
        out["columns"],
        json!(["日付", "品目", "金額", "精算済み", "社員番号"])
    );
    assert_eq!(
        out["column_types"],
        json!(["DATE", "VARCHAR", "BIGINT", "BOOLEAN", "VARCHAR"])
    );
    assert_eq!(
        out["items"][0],
        json!({ "日付": "2024-04-01", "品目": "交通費", "金額": 1200, "精算済み": true, "社員番号": "007" }),
        "{out}"
    );
    assert_eq!(
        out["items"][2]["金額"], 800,
        "数式は保存された値を読む: {out}"
    );
    assert_eq!(
        out["rows"][0],
        json!(["2024-04-01", "交通費", "1200", "true", "007"])
    );
    assert_eq!(out["total_rows"], 4, "空の行は飛ばす: {out}");
    assert_eq!(out["version"], xlsx.version);

    assert_eq!(old.1, RunStatus::Failed);
    let error = old_step.1.unwrap();
    assert_eq!(error["code"], "not_text_file", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("古い形式の Excel（.xls）"),
        "{error}"
    );

    assert_eq!(patched.1, RunStatus::Failed);
    let error = patched_step.1.unwrap();
    assert_eq!(error["code"], "invalid", "{error}");
    assert_eq!(error["message"], tabular::input::NOT_EDITABLE, "{error}");
    assert_eq!(after.version, xlsx.version, "書き換えない");
    assert_eq!(
        after_bytes,
        include_bytes!("../../tabular/tests/fixtures/expenses.xlsx").as_slice()
    );
    assert_eq!(written.1, RunStatus::Failed);
    let error = written_step.1.unwrap();
    assert_eq!(error["code"], "invalid", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains(&format!("「集計-{tag}.xlsx」は Excel のファイルの名前（.xlsx）です。保存できるのは CSV のファイルだけ")),
        "{error}"
    );
    assert_eq!(made, 0, "「集計.xlsx.csv」を作らない");
}

/// 新しく保存する CSV の文字コード（GAP-47・D-120）。「新しい CSV を保存」と、CSV として保存する「新しいファイルを
/// 保存」は、選んだ文字コード（UTF-8〔BOM 付き〕・UTF-8・Shift_JIS）で書く。欄の無い保存済みの定義は、これまでどおり
/// BOM の無い UTF-8 で書く。CSV でない種類には使わない。Shift_JIS に無い文字は書かずに失敗する。
#[tokio::test]
async fn new_csv_files_are_saved_in_the_chosen_encoding() {
    use textenc::encoding_rs::{SHIFT_JIS, UTF_8};
    let Some(h) = setup().await else { return };
    let Some(runner) = runner_path() else {
        eprintln!("shiki-tabular-runner 未ビルドのためスキップ（SHIKI_TABULAR_RUNNER 参照）");
        return;
    };
    let tag = Uuid::new_v4().simple().to_string();
    let csv = format!("品目,金額\n交通費,1200\n{tag},4\u{301C}6月\n");
    let write = |id: &str, ty: &str, name: String, extra: Value| {
        let mut params = json!({ "name": name, "content": csv });
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        json!({ "id": id, "type": ty, "params": params })
    };
    let saved = h
        .save(&json!({
            "ir_version": 1, "name": format!("csv-encoding-{tag}"),
            "declared_scopes": ["csv.write", "storage.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                write("a_old", "csv.write", format!("old-{tag}"), json!({})),
                write("b_bom", "csv.write", format!("bom-{tag}"), json!({ "encoding": "utf8_bom" })),
                write("c_sjis", "csv.write", format!("sjis-{tag}"), json!({ "encoding": "shift_jis" })),
                write("d_file_csv", "storage.write", format!("file-{tag}.csv"), json!({ "encoding": "utf8_bom" })),
                write("e_file_md", "storage.write", format!("note-{tag}.md"), json!({ "encoding": "shift_jis" }))
            ],
            "edges": []
        }))
        .await;
    let refused_wf = h
        .save(&json!({
            "ir_version": 1, "name": format!("csv-encoding-refused-{tag}"),
            "declared_scopes": ["csv.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [ { "id": "w", "type": "csv.write", "params": {
                "name": format!("emoji-{tag}"), "content": "名前\n😀\n", "encoding": "shift_jis" } } ],
            "edges": []
        }))
        .await;
    let (run_id, status) = h
        .run(saved, &json!({}), h.executor_with_tabular(&runner))
        .await;
    let steps: Vec<(String, Option<Value>, Option<Value>)> = sqlx::query_as(
        "SELECT node_id, output, error FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 ORDER BY node_id",
    )
    .bind(&h.tenant)
    .bind(run_id)
    .fetch_all(&h.pool)
    .await
    .unwrap();
    let mut files = Vec::new();
    for (node, output, _) in &steps {
        let id = output
            .as_ref()
            .and_then(|o| o.get("file_id"))
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok());
        if let Some(id) = id {
            let (meta, bytes) = h
                .storage
                .read_file_internal(&h.alice, id, None)
                .await
                .unwrap();
            files.push((node.clone(), meta, bytes));
        }
    }
    let (refused_run, refused_status) = h
        .run(refused_wf, &json!({}), h.executor_with_tabular(&runner))
        .await;
    let refused_error: Option<Value> =
        sqlx::query_scalar("SELECT error FROM step_execution WHERE tenant_id = $1 AND run_id = $2")
            .bind(&h.tenant)
            .bind(refused_run)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    let emoji_saved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node WHERE tenant_id = $1 AND name LIKE $2")
            .bind(&h.tenant)
            .bind(format!("emoji-{tag}%"))
            .fetch_one(&h.pool)
            .await
            .unwrap();

    // 片付け（run の履歴・保存したファイル・ワークフロー）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for (_, meta, _) in &files {
        h.storage
            .soft_delete_file(&h.alice, meta.id, None)
            .await
            .unwrap();
    }
    for wf in [saved, refused_wf] {
        h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    }

    assert_eq!(status, RunStatus::Succeeded, "{steps:?}");
    let by_node = |node: &str| {
        files
            .iter()
            .find(|(n, _, _)| n == node)
            .map(|(_, meta, bytes)| (meta.name.clone(), bytes.clone()))
            .unwrap_or_else(|| panic!("{node} のファイルが無い: {steps:?}"))
    };
    let (old_name, old) = by_node("a_old");
    assert_eq!(old_name, format!("old-{tag}.csv"));
    assert_eq!(
        old,
        csv.as_bytes(),
        "欄の無い定義は BOM の無い UTF-8（これまでどおり）"
    );
    let bom = textenc::encode_like(&csv, UTF_8, true).unwrap();
    assert_eq!(by_node("b_bom").1, bom, "UTF-8（BOM 付き）");
    assert!(by_node("b_bom").1.starts_with(b"\xEF\xBB\xBF"));
    let wave = csv.replace('\u{301C}', "\u{FF5E}");
    let (sjis, _, _) = SHIFT_JIS.encode(&wave);
    assert_eq!(
        by_node("c_sjis").1,
        sjis.as_ref(),
        "Shift_JIS（〜 は ～ にする）"
    );
    assert_eq!(
        textenc::decode_text(&by_node("c_sjis").1).unwrap().encoding,
        SHIFT_JIS
    );
    assert_eq!(
        by_node("d_file_csv").1,
        bom,
        "CSV として保存する「新しいファイルを保存」"
    );
    assert_eq!(
        by_node("e_file_md").1,
        csv.as_bytes(),
        "CSV でない種類には文字コードの欄を使わない"
    );
    assert_eq!(refused_status, RunStatus::Failed);
    let error = refused_error.unwrap();
    assert_eq!(error["code"], "invalid", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("「😀」は Shift_JIS で書けないため、CSV を保存できませんでした。"),
        "{error}"
    );
    assert_eq!(emoji_saved, 0, "書けない文字があるときは保存しない");
}

/// 「CSV を集計」の結果（表）を「CSV を保存」に渡すと、表がそのまま CSV になる（JSON の文字のまま .csv にしない・
/// BUG-178）。項目のまとまりのリストを CSV として保存する「ファイルを保存」も CSV に組み立てる。CSV にできない値は
/// 保存せずに、欄の名前を言って失敗する。
#[tokio::test]
async fn csv_write_saves_tables_and_lists_as_csv() {
    let Some(h) = setup().await else { return };
    let Some(runner) = runner_path() else {
        eprintln!("shiki-tabular-runner 未ビルドのためスキップ（SHIKI_TABULAR_RUNNER 参照）");
        return;
    };
    let tag = Uuid::new_v4().simple().to_string();
    let src = h
        .storage
        .write_file_internal(
            &h.alice,
            None,
            &format!("src-{tag}.csv"),
            "品目,金額\n交通費,1200\n会議費,300\n交通費,800\n".as_bytes(),
            "text/csv",
            None,
        )
        .await
        .expect("csv アップロード");
    let wf = h
        .save(&json!({
            "ir_version": 1, "name": format!("csv-compose-{tag}"),
            "declared_scopes": ["csv.read", "csv.write", "storage.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "q", "type": "csv.query", "params": {
                    "file": { "$from": "input", "path": "/file" },
                    "sql": "SELECT 品目, SUM(金額) AS 合計 FROM data GROUP BY 品目 ORDER BY 合計 DESC" } },
                { "id": "save", "type": "csv.write", "params": {
                    "name": format!("合計-{tag}"),
                    "content": { "$from": "nodes.q.output" },
                    "encoding": "utf8_bom" } },
                { "id": "list", "type": "storage.write", "params": {
                    "name": format!("list-{tag}.csv"),
                    "content": { "$from": "input", "path": "/records" } } }
            ],
            "edges": [ { "from": "q", "to": "save" } ]
        }))
        .await;
    let refused_wf = h
        .save(&json!({
            "ir_version": 1, "name": format!("csv-compose-refused-{tag}"),
            "declared_scopes": ["csv.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [ { "id": "w", "type": "csv.write", "params": {
                "name": format!("number-{tag}"), "content": { "$from": "input", "path": "/n" } } } ],
            "edges": []
        }))
        .await;
    let input = json!({
        "file": src.id.to_string(),
        "records": [ { "氏名": "佐藤", "金額": 1200 }, { "氏名": "=鈴木", "メモ": "a,b" } ]
    });
    let (run_id, status) = h.run(wf, &input, h.executor_with_tabular(&runner)).await;
    let steps: Vec<(String, Option<Value>, Option<Value>)> = sqlx::query_as(
        "SELECT node_id, output, error FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 ORDER BY node_id",
    )
    .bind(&h.tenant)
    .bind(run_id)
    .fetch_all(&h.pool)
    .await
    .unwrap();
    let mut files = Vec::new();
    for (node, output, _) in &steps {
        let id = output
            .as_ref()
            .and_then(|o| o.get("file_id"))
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok());
        if let Some(id) = id {
            let (meta, bytes) = h
                .storage
                .read_file_internal(&h.alice, id, None)
                .await
                .unwrap();
            files.push((node.clone(), meta, bytes));
        }
    }
    let (refused_run, refused_status) = h
        .run(
            refused_wf,
            &json!({ "n": 3 }),
            h.executor_with_tabular(&runner),
        )
        .await;
    let refused_error: Option<Value> =
        sqlx::query_scalar("SELECT error FROM step_execution WHERE tenant_id = $1 AND run_id = $2")
            .bind(&h.tenant)
            .bind(refused_run)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    let number_saved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node WHERE tenant_id = $1 AND name LIKE $2")
            .bind(&h.tenant)
            .bind(format!("number-{tag}%"))
            .fetch_one(&h.pool)
            .await
            .unwrap();

    // 片付け（run の履歴・保存したファイル・元の CSV・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for id in files.iter().map(|(_, meta, _)| meta.id).chain([src.id]) {
        h.storage
            .soft_delete_file(&h.alice, id, None)
            .await
            .unwrap();
    }
    for id in [wf, refused_wf] {
        h.artifacts.delete(&h.alice, id, None).await.unwrap();
    }
    h.delete_fga_store().await;

    assert_eq!(status, RunStatus::Succeeded, "{steps:?}");
    let text_of = |node: &str| {
        files
            .iter()
            .find(|(n, _, _)| n == node)
            .map(|(_, meta, bytes)| (meta.name.clone(), String::from_utf8(bytes.clone()).unwrap()))
            .unwrap_or_else(|| panic!("{node} のファイルが無い: {steps:?}"))
    };
    assert_eq!(
        text_of("save"),
        (
            format!("合計-{tag}.csv"),
            "\u{FEFF}品目,合計\r\n交通費,2000\r\n会議費,300\r\n".to_string()
        ),
        "表は CSV に組み立て、選んだ文字コード（BOM 付きの UTF-8）で保存する"
    );
    assert_eq!(
        text_of("list").1,
        "メモ,氏名,金額\r\n,佐藤,1200\r\n\"a,b\",'=鈴木,\r\n",
        "項目のまとまりのリストは、名前の順の列の CSV にする（式に見える値は打ち消す）"
    );
    assert_eq!(refused_status, RunStatus::Failed);
    let error = refused_error.unwrap();
    assert_eq!(error["code"], "invalid", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("「CSV の内容」に数が入っているため、CSV にできません。"),
        "{error}"
    );
    assert_eq!(number_saved, 0, "CSV にできない値は保存しない");
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

/// script の `Shiki.http.request` は、作成者が書いた headers を本番のポートで相手に送る（以前は黙って捨てていた・
/// BUG-179）。作成者の Content-Type は Shiki が付ける値の代わりに送る。Shiki が決めるヘッダー（Host）を書いた呼び出しは、
/// 送らずに invalid で失敗する。
#[tokio::test]
async fn script_http_request_sends_the_headers_the_author_wrote() {
    let Some(h) = setup().await else { return };
    let (base, seen) = spawn_http_recorder().await;
    let script = |path: &str, headers: &str| {
        format!(
            "function main(input) {{ const r = Shiki.http.request({{ method: \"POST\", url: \"{base}/{path}\", \
             headers: {headers}, body: \"品目=交通費\" }}); return {{ status: r.status }}; }}"
        )
    };
    let ir = |name: &str, source: String| {
        json!({
            "ir_version": 1, "name": name,
            "declared_scopes": ["http.egress"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [ { "id": "sc", "type": "script.run", "params": { "source": { "inline": source } } } ],
            "edges": []
        })
    };
    let sent = h
        .save(&ir(
            "script-http-headers",
            script(
                "hdr",
                r#"{ "X-Request-Id": "req-1", "Content-Type": "text/plain; charset=utf-8" }"#,
            ),
        ))
        .await;
    let refused = h
        .save(&ir(
            "script-http-host",
            script("host", r#"{ "Host": "internal.example.com" }"#),
        ))
        .await;
    let (sent_run, sent_status) = h
        .run(
            sent,
            &json!({}),
            h.executor(None, vec!["127.0.0.1".into()], None),
        )
        .await;
    let (refused_run, refused_status) = h
        .run(
            refused,
            &json!({}),
            h.executor(None, vec!["127.0.0.1".into()], None),
        )
        .await;
    let error_of = |run: Uuid| {
        let pool = h.pool.clone();
        let tenant = h.tenant.clone();
        async move {
            sqlx::query_scalar::<_, Option<Value>>(
                "SELECT error FROM step_execution WHERE tenant_id = $1 AND run_id = $2",
            )
            .bind(tenant)
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let sent_error = error_of(sent_run).await;
    let refused_error = error_of(refused_run).await;
    let requests = seen.lock().await.clone();

    // 片付け（run の履歴・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for id in [sent, refused] {
        h.artifacts.delete(&h.alice, id, None).await.unwrap();
    }
    h.delete_fga_store().await;

    assert_eq!(sent_status, RunStatus::Succeeded, "{sent_error:?}");
    let raw = requests
        .iter()
        .find(|r| r.starts_with("POST /hdr "))
        .unwrap_or_else(|| panic!("/hdr への要求が無い: {requests:?}"));
    let lower = raw.to_ascii_lowercase();
    assert!(lower.contains("\r\nx-request-id: req-1\r\n"), "{raw}");
    assert!(
        lower.contains("\r\ncontent-type: text/plain; charset=utf-8\r\n"),
        "{raw}"
    );
    assert!(
        !lower.contains("application/json"),
        "作成者の Content-Type の代わりに付けない: {raw}"
    );
    assert!(lower.contains("\r\nidempotency-key: "), "{raw}");
    assert!(raw.ends_with("品目=交通費"), "{raw}");

    assert_eq!(refused_status, RunStatus::Failed);
    let error = refused_error.unwrap();
    assert_eq!(error["code"], "invalid", "{error}");
    assert!(
        error["message"].as_str().unwrap().starts_with(
            "Shiki.http.request の headers の「Host」は Shiki が決めるため、指定できません。"
        ),
        "{error}"
    );
    assert!(
        !requests.iter().any(|r| r.starts_with("POST /host ")),
        "Host を書いた呼び出しは送らない: {requests:?}"
    );
}

/// 「フォルダ一覧」は、上限（100 件）より多いフォルダーでは名前の順の先頭 100 件と `truncated: true` を返す（黙って
/// 切らない・BUG-180）。ちょうど上限の件数なら `truncated` は false。どちらも 1 回の読み込み（フォルダーの中身の一覧の
/// 監査が 1 行）で決める（続きを確かめる 2 回目の読み込みをしない）。
#[tokio::test]
async fn storage_list_says_when_it_cuts_the_folder() {
    let Some(h) = setup().await else { return };
    let max = workflow_engine::vocab::STORAGE_LIST_MAX_ITEMS;
    let folder = h
        .storage
        .create_folder(
            &h.alice,
            None,
            &format!("list-{}", Uuid::new_v4().simple()),
            None,
        )
        .await
        .unwrap();
    let mut files = Vec::new();
    for i in 0..=max {
        let node = h
            .storage
            .write_file_internal(
                &h.alice,
                Some(folder.id),
                &format!("f-{i:03}.txt"),
                b"x",
                "text/plain",
                None,
            )
            .await
            .unwrap();
        files.push(node.id);
    }
    let wf = h
        .save(&json!({
            "ir_version": 1, "name": "storage-list-truncated",
            "declared_scopes": ["storage.read"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [ { "id": "ls", "type": "storage.list", "params": { "folder": folder.id.to_string() } } ],
            "edges": []
        }))
        .await;
    let output_of = |run: Uuid| {
        let pool = h.pool.clone();
        let tenant = h.tenant.clone();
        async move {
            sqlx::query_scalar::<_, Option<Value>>(
                "SELECT output FROM step_execution WHERE tenant_id = $1 AND run_id = $2",
            )
            .bind(tenant)
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap()
            .unwrap_or(Value::Null)
        }
    };
    // フォルダーの中身の一覧の監査の行数（run ごとに増えた分を見る）。
    let listed = || {
        let pool = h.pool.clone();
        let tenant = h.tenant.clone();
        let folder = folder.id.to_string();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM audit_log \
                 WHERE tenant_id = $1 AND action = 'folder.children.list' AND object_id = $2",
            )
            .bind(tenant)
            .bind(folder)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let before = listed().await;
    let (over_run, over_status) = h.run(wf, &json!({}), h.executor(None, vec![], None)).await;
    let over = output_of(over_run).await;
    let over_audits = listed().await - before;
    // 1 件減らして、ちょうど上限の件数にする。
    let last = files.pop().unwrap();
    h.storage
        .soft_delete_file(&h.alice, last, None)
        .await
        .unwrap();
    let before = listed().await;
    let (exact_run, exact_status) = h.run(wf, &json!({}), h.executor(None, vec![], None)).await;
    let exact = output_of(exact_run).await;
    let exact_audits = listed().await - before;

    // 片付け（run の履歴・フォルダーと中のファイル・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for id in &files {
        h.storage
            .soft_delete_file(&h.alice, *id, None)
            .await
            .unwrap();
    }
    h.storage
        .soft_delete_folder(&h.alice, folder.id, None)
        .await
        .unwrap();
    h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    h.delete_fga_store().await;

    assert_eq!(over_status, RunStatus::Succeeded, "{over}");
    let names = |out: &Value| -> Vec<String> {
        out["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap().to_string())
            .collect()
    };
    let over_names = names(&over);
    assert_eq!(over_names.len(), max, "{over}");
    assert_eq!(over_names.first().unwrap(), "f-000.txt");
    assert_eq!(over_names.last().unwrap(), &format!("f-{:03}.txt", max - 1));
    assert_eq!(over["truncated"], true, "上限より多いと truncated: {over}");
    assert_eq!(exact_status, RunStatus::Succeeded, "{exact}");
    assert_eq!(names(&exact).len(), max);
    assert_eq!(
        exact["truncated"], false,
        "ちょうど上限なら truncated にしない: {exact}"
    );
    assert_eq!(
        (over_audits, exact_audits),
        (1, 1),
        "1 回の読み込みで決める（監査は 1 行）"
    );
}

/// 「フォルダ一覧」の続きの位置（D-131）: 結果の next_cursor を次の「フォルダ一覧」の cursor に渡すと続きを読む。
/// 続きの位置はテナントとフォルダーに束縛した不透明な文字列で、別のフォルダー・別のテナント・読めない値は invalid。
#[tokio::test]
async fn storage_list_reads_on_from_the_cursor() {
    let Some(h) = setup().await else { return };
    let max = workflow_engine::vocab::STORAGE_LIST_MAX_ITEMS;
    let make = |name: String| {
        let h = &h;
        async move {
            h.storage
                .create_folder(&h.alice, None, &name, None)
                .await
                .unwrap()
        }
    };
    let folder = make(format!("list-{}", Uuid::new_v4().simple())).await;
    let other = make(format!("other-{}", Uuid::new_v4().simple())).await;
    let mut files = Vec::new();
    for i in 0..=max {
        let node = h
            .storage
            .write_file_internal(
                &h.alice,
                Some(folder.id),
                &format!("社外秘-{i:03}.txt"),
                b"x",
                "text/plain",
                None,
            )
            .await
            .unwrap();
        files.push(node.id);
    }
    let (exec, data) = h.executor_with_data();
    let chained = h
        .save(&json!({
            "ir_version": 1, "name": "storage-list-cursor",
            "declared_scopes": ["storage.read"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "ls1", "type": "storage.list", "params": { "folder": folder.id.to_string() } },
                { "id": "ls2", "type": "storage.list", "params": {
                    "folder": folder.id.to_string(),
                    "cursor": { "$from": "nodes.ls1.output", "path": "/next_cursor" } } }
            ],
            "edges": [{ "from": "ls1", "to": "ls2" }]
        }))
        .await;
    let (run, status) = h.run(chained, &json!({}), Arc::clone(&exec)).await;
    let steps: Vec<(String, Option<Value>)> = sqlx::query_as(
        "SELECT node_id, output FROM step_execution WHERE tenant_id = $1 AND run_id = $2 ORDER BY node_id",
    )
    .bind(&h.tenant)
    .bind(run)
    .fetch_all(&h.pool)
    .await
    .unwrap();
    // 読めない続きの位置を渡す版（別のフォルダー・別のテナント・壊れた値）。
    let token = steps
        .first()
        .and_then(|s| s.1.as_ref())
        .and_then(|o| o["next_cursor"].as_str())
        .unwrap_or_default()
        .to_string();
    let scope = storage::ChildSort::default().cursor_scope(&folder.id.to_string());
    let foreign = data
        .seal_list_cursor(
            data::ListCursorKind::WorkflowFolder,
            "t-other",
            &scope,
            "n00000000-0000-0000-0000-000000000000a",
        )
        .unwrap();
    let mut refused = Vec::new();
    for (name, target, cursor) in [
        ("cursor-other-folder", other.id, token.clone()),
        ("cursor-other-tenant", folder.id, foreign),
        ("cursor-broken", folder.id, format!("{token}x")),
    ] {
        let wf = h
            .save(&json!({
                "ir_version": 1, "name": name,
                "declared_scopes": ["storage.read"],
                "triggers": [{ "kind": "interactive" }],
                "nodes": [ { "id": "ls", "type": "storage.list",
                             "params": { "folder": target.to_string(), "cursor": cursor } } ],
                "edges": []
            }))
            .await;
        let (run, status) = h.run(wf, &json!({}), Arc::clone(&exec)).await;
        let error: Option<Value> = sqlx::query_scalar(
            "SELECT error FROM step_execution WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&h.tenant)
        .bind(run)
        .fetch_one(&h.pool)
        .await
        .unwrap();
        refused.push((name, status, error.unwrap_or(Value::Null), wf));
    }

    // 片付け（run の履歴・フォルダーと中のファイル・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for id in &files {
        h.storage
            .soft_delete_file(&h.alice, *id, None)
            .await
            .unwrap();
    }
    for f in [folder.id, other.id] {
        h.storage
            .soft_delete_folder(&h.alice, f, None)
            .await
            .unwrap();
    }
    h.artifacts.delete(&h.alice, chained, None).await.unwrap();
    for (_, _, _, wf) in &refused {
        h.artifacts.delete(&h.alice, *wf, None).await.unwrap();
    }
    h.delete_fga_store().await;

    assert_eq!(status, RunStatus::Succeeded, "{steps:?}");
    let out = |id: &str| {
        steps
            .iter()
            .find(|s| s.0 == id)
            .and_then(|s| s.1.clone())
            .unwrap()
    };
    let (first, second) = (out("ls1"), out("ls2"));
    assert_eq!(first["items"].as_array().unwrap().len(), max);
    assert_eq!(first["truncated"], true);
    assert!(!token.is_empty(), "{first}");
    let hex = |s: &str| {
        s.bytes().fold(String::new(), |mut o, b| {
            use std::fmt::Write as _;
            let _ = write!(o, "{b:02x}");
            o
        })
    };
    for leak in [hex("社外秘"), files[max - 1].to_string()] {
        assert!(
            !token.contains(&leak),
            "続きの位置は中身が読めない: {token}"
        );
    }
    let rest: Vec<&str> = second["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(rest, vec![format!("社外秘-{max:03}.txt")], "続きから読む");
    assert_eq!(second["truncated"], false);
    assert_eq!(second["next_cursor"], Value::Null);
    for (name, status, error, _) in &refused {
        assert_eq!(*status, RunStatus::Failed, "{name}: {error}");
        assert_eq!(error["code"], "invalid", "{name}: {error}");
        assert!(
            error["message"]
                .as_str()
                .unwrap_or_default()
                .contains("続きの位置が読めません"),
            "{name}: {error}"
        );
    }
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

/// ノードに書いた思考の深さ（low）は、選べる組織で特定のモデルで動くときだけモデルへ届く。モデルを省略して Auto で
/// 動くときと、組織がモデルの選択を禁じているときは使わず、組織の設定に従う（チャットと揃える・D-75）。stub は
/// `echoeffort:` に受け取った思考の深さを返す。
#[tokio::test]
async fn llm_node_effort_follows_the_organization_rules() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let effort_ir = |name: &str, model: Option<&str>| {
        let mut ir = llm_ir(name, model);
        ir["nodes"][0]["params"]["prompt"] = json!("echoeffort:");
        ir["nodes"][0]["params"]["effort"] = json!("low");
        ir
    };
    let omitted = h.save(&effort_ir("effort-omitted", None)).await;
    let pinned = h
        .save(&effort_ir("effort-pinned", Some("stub-think")))
        .await;
    let sent = |wf: Uuid| {
        let executor = Arc::clone(&executor);
        let h = &h;
        async move {
            let (run_id, status) = h.run(wf, &json!({}), executor).await;
            assert_eq!(status, RunStatus::Succeeded);
            let (output, _) = h.step_output(run_id).await;
            output["text"].as_str().unwrap_or_default().to_string()
        }
    };

    // 既定が特定のモデル（選べる組織）: 省略してもノードの指定が組織の既定（medium）より優先する。
    h.set_llm_settings(llm_gateway::SettingsPatch {
        default_model: Some("stub-think".into()),
        default_effort: Some(llm_gateway::Effort::Medium),
        ..Default::default()
    })
    .await;
    assert_eq!(sent(omitted).await, "effort: low");

    // 既定が Auto: 省略は Auto の思考強度（high）で、ノードの指定を使わない。モデルを選べば使う。
    h.set_llm_settings(llm_gateway::SettingsPatch {
        default_model: Some(llm_gateway::AUTO_MODEL.into()),
        auto_model: Some(Some("stub-think".into())),
        auto_effort: Some(Some(llm_gateway::Effort::High)),
        ..Default::default()
    })
    .await;
    assert_eq!(sent(omitted).await, "effort: high");
    assert_eq!(sent(pinned).await, "effort: low");

    // 組織がモデルの選択を禁じる: 省略も既定と同じモデルの明示も、組織の既定の思考強度（medium）で動く
    // （チャットと同じ。選ぶ画面の無いチャットは組織の既定で動く）。
    h.set_llm_settings(llm_gateway::SettingsPatch {
        default_model: Some("stub-think".into()),
        allow_user_choice: Some(false),
        ..Default::default()
    })
    .await;
    assert_eq!(sent(omitted).await, "effort: medium");
    assert_eq!(sent(pinned).await, "effort: medium");

    h.cleanup_runs().await;
    for sql in [
        "DELETE FROM tenant_llm_model WHERE tenant_id = $1",
        "DELETE FROM tenant_llm_settings WHERE tenant_id = $1",
    ] {
        sqlx::query(sql)
            .bind(&h.tenant)
            .execute(&h.pool)
            .await
            .unwrap();
    }
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

    /// このテストのテナントに作った run・会計・決定キャッシュの行を消す（run を消せば step は FK で消える）。
    async fn cleanup_runs(&self) {
        self.clear_usage().await;
        sqlx::query("DELETE FROM llm_decision_cache WHERE tenant_id = $1")
            .bind(&self.tenant)
            .execute(&self.pool)
            .await
            .unwrap();
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
    assert_eq!(output["provider"], "stub", "答えた決定器の名前");
    assert_eq!(output["calibrated"], false);
    // stub の決定器は自分の確率をそのまま使い、AI の自信を引き上げない（BUG-124・D-114）。
    assert_eq!(output["raised_questions"], json!([]), "{output}");
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
    h.clear_usage().await;

    // 決定器が要求と合わない答えを返した（チョークポイントの突き合わせで弾いた）→ 送り直しても直らないので
    // 再試行しない decision_invalid_response（試すたびに #f で課金されない）。原因は利用者に出さない。
    let (run_id, status) = h
        .run(
            wf_id,
            &json!({ "body": "stub:malformed" }),
            Arc::clone(&executor),
        )
        .await;
    assert_eq!(status, RunStatus::Failed);
    let (_, err) = h.step_output(run_id).await;
    assert_eq!(err["code"], "decision_invalid_response", "{err}");
    assert_eq!(err["retryable"], false, "{err}");
    assert!(
        !err["message"]
            .as_str()
            .unwrap()
            .contains("invalid response"),
        "{err}"
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
    assert_eq!(output["provider"], "llm_decider", "{output}");
    assert_eq!(output["method"], "verbalized", "{output}");
    // stub の chat モデルは自信を 0 で答える → 一様（選択肢 2 件・はい/いいえとも 0.5）へ引き上げた質問が、質問の順で
    // step の出力に載る（BUG-124・D-114）。引き上げた答えでも同じ規則で振り分ける（0.5 はしきい値 0.8 に届かない）。
    assert_eq!(
        output["raised_questions"],
        json!(["category", "angry"]),
        "{output}"
    );
    assert_eq!(output["answers"]["category"]["confidence"], 0.5, "{output}");
    assert_eq!(output["answers"]["angry"]["confidence"], 0.5, "{output}");
    assert_eq!(output["routed"]["port"], "uncertain", "{output}");
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

// ------- Test 13: ai.decide の決定キャッシュ（T10）------------------------------

/// cache を書いた判定は、同じワークフローの 2 回目の run で答えを使い回し（決定器を呼ばず会計は 1 行だけ）、
/// 同じ IR を別のワークフローとして保存したものでは使い回さない。cache を書かなければ毎回判定する。
#[tokio::test]
async fn decide_cache_reuses_answers_within_the_workflow_only() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let step_key = |run_id: Uuid| format!("wf:{}:{run_id}:ai", h.tenant);
    let cached_ir = |name: &str| {
        let mut ir = decide_ir(name, "decide-stub");
        ir["nodes"][0]["cache"] = json!({ "ttl_sec": 3600 });
        ir
    };
    let input = json!({ "body": "billing の件で連絡しました" });

    let wf = h.save(&cached_ir("decide-cache")).await;
    let (first, status) = h.run(wf, &input, Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(h.step_output(first).await.0["cached"], false);
    let (second, status) = h.run(wf, &input, Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Succeeded);
    let (output, _) = h.step_output(second).await;
    assert_eq!(output["cached"], true, "{output}");
    assert_eq!(output["answers"]["category"]["value"], "billing");
    assert_eq!(output["routed"]["port"], "billing");
    assert_eq!(
        h.step_map(second).await["on_billing"],
        StepStatus::Succeeded
    );
    assert_eq!(
        h.usage_keys().await,
        [step_key(first)],
        "当たりは会計しない"
    );

    // 同じ IR の別のワークフローでは使い回さない（範囲はワークフローごと）。
    let other = h.save(&cached_ir("decide-cache-other")).await;
    let (third, _) = h.run(other, &input, Arc::clone(&executor)).await;
    assert_eq!(h.step_output(third).await.0["cached"], false);
    assert_eq!(h.usage_keys().await.len(), 2);

    // cache を書かない判定は毎回判定する（既定は使わない）。
    let plain = h.save(&decide_ir("decide-no-cache", "decide-stub")).await;
    for _ in 0..2 {
        let (run_id, _) = h.run(plain, &input, Arc::clone(&executor)).await;
        assert_eq!(h.step_output(run_id).await.0["cached"], false);
    }
    assert_eq!(h.usage_keys().await.len(), 4);

    // 当たった後に組織で判定用のモデルを止めると、当たりでも非再試行で失敗する（許可は毎回確かめる）。
    h.set_llm_settings(llm_gateway::SettingsPatch {
        models: vec![("decide-stub".into(), false)],
        ..Default::default()
    })
    .await;
    let (run_id, status) = h.run(wf, &input, Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Failed);
    let (_, err) = h.step_output(run_id).await;
    assert_eq!(err["code"], "model_unavailable", "{err}");
    assert_eq!(err["retryable"], false);

    // ワークフローを消すと（削除の API と同じ ArtifactStore::delete・論理削除）、その答えも同じ TX で消える
    // （migration 0114 のトリガ）。ほかのワークフローの答えは残る。
    let cached_rows = |wf: Uuid| {
        let pool = h.pool.clone();
        let tenant = h.tenant.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM llm_decision_cache WHERE tenant_id = $1 AND workflow_id = $2",
            )
            .bind(&tenant)
            .bind(wf)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(cached_rows(wf).await, 1);
    assert_eq!(cached_rows(other).await, 1);
    h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    assert_eq!(
        cached_rows(wf).await,
        0,
        "消したワークフローの答えは残さない"
    );
    assert_eq!(cached_rows(other).await, 1);
    h.artifacts.delete(&h.alice, other, None).await.unwrap();
    assert_eq!(cached_rows(other).await, 0);
    h.cleanup_runs().await;
    // 組織の AI 設定（このテストで保存したもの）も消す。
    for table in ["tenant_llm_model", "tenant_llm_settings"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&h.tenant)
            .execute(&h.pool)
            .await
            .unwrap();
    }
}

// ------- ファイルの保存の名前と種類・ファイルの ID の名前（BUG-97・D-96・D-97）-------------

/// 本番のポートで、同じ名前のファイルがあるときの扱いを確かめる（BUG-97）。
///
/// - 省略（既存の定義）は今のまま失敗にし、失敗の理由は「同じ名前のファイル」と言う（ほかの操作との重なりと
///   言わない）。
/// - 番号を付ける扱いは、ドライブの新規作成と同じ規則（`名前 (2).txt`）で空いている名前に保存する。
/// - ファイルの種類を決めなければ拡張子から決め、結果にはファイルの ID を file_id でも載せる（D-96・D-97）。
#[tokio::test]
async fn storage_write_names_and_types_through_the_production_port() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let base = format!("dup-{}", Uuid::new_v4().simple());
    let name = format!("{base}.md");
    let ir = |policy: Option<&str>| {
        let mut params = json!({ "name": name, "content": "# 週報" });
        if let Some(p) = policy {
            params["on_name_taken"] = json!(p);
        }
        json!({
            "ir_version": 1,
            "name": format!("save-{}", policy.unwrap_or("default").replace('_', "-")),
            "declared_scopes": ["storage.write"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "save", "type": "storage.write", "params": params }],
            "edges": []
        })
    };
    let step = |run_id: Uuid| {
        let pool = h.pool.clone();
        let tenant = h.tenant.clone();
        async move {
            let row: (Option<String>, Option<String>, Option<Value>) = sqlx::query_as(
                "SELECT error->>'code', error->>'message', output FROM step_execution \
                 WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'save'",
            )
            .bind(&tenant)
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            row
        }
    };
    let fail_wf = h.save(&ir(None)).await;
    let number_wf = h.save(&ir(Some("add_number"))).await;

    // 1 回目は元の名前で保存し、種類は拡張子から決める。
    let (first, status) = h.run(fail_wf, &json!({}), Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Succeeded);
    let (_, _, out) = step(first).await;
    let out = out.unwrap();
    assert_eq!(out["name"], name.as_str());
    assert_eq!(out["file_id"], out["id"]);
    let ct: Option<String> =
        sqlx::query_scalar("SELECT content_type FROM node WHERE id = $1::uuid")
            .bind(out["id"].as_str().unwrap())
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert_eq!(ct.as_deref(), Some("text/markdown"));

    // 省略は失敗にする（既存の定義の挙動）。理由は同じ名前のファイル。
    let (second, status) = h.run(fail_wf, &json!({}), Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Failed);
    let (code, message, _) = step(second).await;
    assert_eq!(
        code.as_deref(),
        Some(workflow_engine::nodes::ports::NAME_TAKEN)
    );
    assert!(message.unwrap().contains(&format!("「{name}」")));
    let failure = workflow_engine::run::failure::run_failure(second, code.as_deref(), None);
    assert_eq!(
        failure.message,
        "同じ名前のファイルがすでにあるため、保存できませんでした。"
    );

    // 番号を付ける扱いは空いている名前で保存する。番号の規則はドライブの新規作成と同じ。
    let (third, status) = h.run(number_wf, &json!({}), Arc::clone(&executor)).await;
    assert_eq!(status, RunStatus::Succeeded);
    let (_, _, out) = step(third).await;
    let numbered = storage::numbered_name(&name, 2);
    assert_eq!(out.unwrap()["name"], numbered.as_str());
    let drive = h
        .storage
        .write_file_unique_internal(&h.alice, None, &name, b"x", "text/markdown", None)
        .await
        .unwrap();
    assert_eq!(drive.name, storage::numbered_name(&name, 3));

    // 片付け（run の履歴・保存したファイル・ワークフロー）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM node WHERE org = $1 AND name LIKE $2 AND deleted_at IS NULL",
    )
    .bind(&h.org)
    .bind(format!("{base}%"))
    .fetch_all(&h.pool)
    .await
    .unwrap();
    assert_eq!(ids.len(), 3);
    for id in ids {
        h.storage
            .soft_delete_file(&h.alice, id, None)
            .await
            .unwrap();
    }
    for wf in [fail_wf, number_wf] {
        h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    }
}

/// 本番のポートで、新しい CSV を保存（csv.write）の同じ名前の扱いを確かめる（BUG-97・D-124）。
///
/// - 省略（既存の定義）は今のまま失敗にし、理由は同じ名前のファイル（.csv を付けた名前）と言う。
/// - 番号を付ける扱いは、ファイルを保存・ドライブの新規作成と同じ規則（`名前 (2).csv`）で、続きの番号を付ける。
///   番号は StorageService の書込の入口が付ける（ポートへの 1 回の要求で、空いている名前を同じ TX の中で探す）。
///   選んだ文字コードは番号を付けても変わらない（D-120）。
/// - 同じ名前へ同時に保存しても、それぞれ別の空いている名前になる。
/// - 上限（`(50)`）まで埋まっていれば name_taken で失敗し、最後に試した名前を言う。
#[tokio::test]
async fn csv_write_numbers_taken_names_through_the_production_port() {
    use workflow_engine::ir::params::NameTakenPolicy;
    use workflow_engine::NodePorts;
    let Some(h) = setup().await else { return };
    let Some(runner) = runner_path() else {
        eprintln!("shiki-tabular-runner 未ビルドのためスキップ（SHIKI_TABULAR_RUNNER 参照）");
        return;
    };
    let tag = Uuid::new_v4().simple().to_string();
    let folder = h
        .storage
        .create_folder(&h.alice, None, &format!("csv-dup-{tag}"), None)
        .await
        .unwrap();
    let folder_id = folder.id;
    let base = format!("集計-{tag}");
    let csv = "品目,金額\n交通費,1200\n";
    let write = |label: &str, name: &str, ty: &str, extra: Value| {
        let mut params = json!({ "folder": folder_id.to_string(), "name": name, "content": csv });
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        json!({
            "ir_version": 1,
            "name": format!("csv-dup-{label}"),
            "declared_scopes": [ty],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "save", "type": ty, "params": params }],
            "edges": []
        })
    };
    let step = |run_id: Uuid| {
        let pool = h.pool.clone();
        let tenant = h.tenant.clone();
        async move {
            let row: (Option<String>, Option<String>, Option<Value>) = sqlx::query_as(
                "SELECT error->>'code', error->>'message', output FROM step_execution \
                 WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'save'",
            )
            .bind(&tenant)
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            row
        }
    };
    let names_in_folder = || {
        let pool = h.pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT name FROM node WHERE parent_id = $1 AND deleted_at IS NULL",
            )
            .bind(folder_id)
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    let fail_wf = h.save(&write("fail", &base, "csv.write", json!({}))).await;
    let number_wf = h
        .save(&write(
            "number",
            &base,
            "csv.write",
            json!({ "on_name_taken": "add_number", "encoding": "utf8_bom" }),
        ))
        .await;
    let file_wf = h
        .save(&write(
            "file",
            &format!("{base}.csv"),
            "storage.write",
            json!({ "on_name_taken": "add_number" }),
        ))
        .await;
    let executor = h.executor_with_tabular(&runner);
    let file_name = format!("{base}.csv");

    // 1 回目は元の名前（.csv を付けた名前）で保存する。
    let (first, first_status) = h.run(fail_wf, &json!({}), Arc::clone(&executor)).await;
    let first_out = step(first).await.2;
    // 省略は失敗にする（既存の定義の挙動）。理由は同じ名前のファイル。
    let (second, second_status) = h.run(fail_wf, &json!({}), Arc::clone(&executor)).await;
    let (second_code, second_message, _) = step(second).await;
    // 番号を付ける扱いは「名前 (2).csv」。選んだ文字コード（BOM 付きの UTF-8）のまま。
    let (third, third_status) = h.run(number_wf, &json!({}), Arc::clone(&executor)).await;
    let third_out = step(third).await.2;
    let third_bytes = match third_out
        .as_ref()
        .and_then(|o| o["file_id"].as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => Some(
            h.storage
                .read_file_internal(&h.alice, id, None)
                .await
                .unwrap()
                .1,
        ),
        None => None,
    };
    // ファイルを保存とドライブの新規作成も、同じ規則で続きの番号を付ける。
    let (fourth, fourth_status) = h.run(file_wf, &json!({}), Arc::clone(&executor)).await;
    let fourth_out = step(fourth).await.2;
    let drive = h
        .storage
        .write_file_unique_internal(
            &h.alice,
            Some(folder_id),
            &file_name,
            b"x",
            "text/csv",
            None,
        )
        .await;

    // 同じ名前へ同時に保存しても、ポートへの 1 回の要求ごとに別の空いている名前になる。
    let tabular = Arc::new(tabular::TabularService::new(
        Arc::clone(&h.storage),
        tabular::RunnerConfig::new(
            runner.to_string_lossy().to_string(),
            Duration::from_secs(20),
        ),
        tabular::Quotas::default(),
    ));
    let mut ports = Arc::into_inner(h.ports(None, None)).unwrap();
    ports.tabular = Some(tabular);
    let ec = workflow_engine::ExecCtx {
        tenant_id: h.tenant.clone(),
        org: h.org.clone(),
        principal: "alice".into(),
        principal_kind: "user".into(),
        trace_id: None,
        run_id: None,
        step_path: None,
    };
    let req = || workflow_engine::CsvWriteReq {
        parent_id: Some(folder_id),
        name: file_name.clone(),
        csv_bytes: csv.as_bytes().to_vec(),
        on_name_taken: NameTakenPolicy::AddNumber,
    };
    let concurrent = futures::future::join_all((0..4).map(|_| ports.csv_write(&ec, req()))).await;

    // 上限（(50)）まで埋めると、番号を付ける扱いも失敗し、最後に試した名前を言う。
    let have = names_in_folder().await.len();
    for _ in have..storage::MAX_NUMBERED_NAME as usize {
        h.storage
            .write_file_unique_internal(
                &h.alice,
                Some(folder_id),
                &file_name,
                b"x",
                "text/csv",
                None,
            )
            .await
            .unwrap();
    }
    let (full, full_status) = h.run(number_wf, &json!({}), Arc::clone(&executor)).await;
    let (full_code, full_message, _) = step(full).await;
    let names = names_in_folder().await;

    // 片付け（run の履歴・保存したファイル・フォルダー・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM node WHERE parent_id = $1 AND deleted_at IS NULL")
            .bind(folder_id)
            .fetch_all(&h.pool)
            .await
            .unwrap();
    for id in ids {
        h.storage
            .soft_delete_file(&h.alice, id, None)
            .await
            .unwrap();
    }
    h.storage
        .soft_delete_folder(&h.alice, folder_id, None)
        .await
        .unwrap();
    for wf in [fail_wf, number_wf, file_wf] {
        h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    }
    h.delete_fga_store().await;

    assert_eq!(first_status, RunStatus::Succeeded);
    assert_eq!(first_out.unwrap()["name"], file_name.as_str());
    assert_eq!(second_status, RunStatus::Failed);
    assert_eq!(
        second_code.as_deref(),
        Some(workflow_engine::nodes::ports::NAME_TAKEN)
    );
    let second_message = second_message.unwrap();
    assert!(
        second_message.contains(&format!("同じ名前のファイル「{file_name}」")),
        "{second_message}"
    );
    assert_eq!(third_status, RunStatus::Succeeded, "{third_out:?}");
    let third_out = third_out.unwrap();
    assert_eq!(
        third_out["name"],
        storage::numbered_name(&file_name, 2).as_str()
    );
    assert_eq!(third_out["file_id"], third_out["node_id"]);
    assert_eq!(
        third_bytes.unwrap(),
        textenc::encode_like(csv, textenc::encoding_rs::UTF_8, true).unwrap(),
        "番号を付けても選んだ文字コードのまま"
    );
    assert_eq!(fourth_status, RunStatus::Succeeded);
    assert_eq!(
        fourth_out.unwrap()["name"],
        storage::numbered_name(&file_name, 3).as_str()
    );
    assert_eq!(drive.unwrap().name, storage::numbered_name(&file_name, 4));
    let mut got: Vec<String> = concurrent
        .into_iter()
        .map(|r| r.unwrap()["name"].as_str().unwrap().to_string())
        .collect();
    got.sort();
    let mut want: Vec<String> = (5..=8)
        .map(|n| storage::numbered_name(&file_name, n))
        .collect();
    want.sort();
    assert_eq!(got, want, "同時の保存が同じ名前を取り合った");
    assert_eq!(full_status, RunStatus::Failed);
    assert_eq!(
        full_code.as_deref(),
        Some(workflow_engine::nodes::ports::NAME_TAKEN)
    );
    let full_message = full_message.unwrap();
    assert!(
        full_message.contains(&format!(
            "「{file_name}」と、番号を付けた「{}」まで",
            storage::numbered_name(&file_name, storage::MAX_NUMBERED_NAME)
        )),
        "{full_message}"
    );
    assert_eq!(
        names.len(),
        storage::MAX_NUMBERED_NAME as usize,
        "上限を超えて保存した"
    );
}

/// 本番のポートで、ファイルを読むの結果に読んだバージョンとファイルの ID（file_id）が載る（D-96）。
#[tokio::test]
async fn storage_read_returns_the_version_it_read() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let node = h
        .storage
        .write_file_internal(
            &h.alice,
            None,
            &format!("v-{}.txt", Uuid::new_v4().simple()),
            b"a",
            "text/plain",
            None,
        )
        .await
        .unwrap();
    let ir = json!({
        "ir_version": 1, "name": "read-version",
        "declared_scopes": ["storage.read"],
        "triggers": [{ "kind": "interactive" }],
        "nodes": [{ "id": "rd", "type": "storage.read", "params": { "file": node.id.to_string() } }],
        "edges": []
    });
    let wf = h.save(&ir).await;
    let (run_id, status) = h.run(wf, &json!({}), executor).await;
    let out: Option<Value> = sqlx::query_scalar(
        "SELECT output FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
    )
    .bind(&h.tenant)
    .bind(run_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    h.storage
        .soft_delete_file(&h.alice, node.id, None)
        .await
        .unwrap();
    h.artifacts.delete(&h.alice, wf, None).await.unwrap();

    assert_eq!(status, RunStatus::Succeeded);
    let out = out.unwrap();
    assert_eq!(out["version"], node.version);
    assert_eq!(out["file_id"], node.id.to_string());
    assert_eq!(out["id"], node.id.to_string());
}

/// 別のワークフローを実行の相手が無いときと、見えないときを同じに扱う（BUG-107）。
///
/// - 保存時の材料（`workflow_start_targets`）は、無い名前も見えない相手も入れない（V4 が同じ誤りで止める）。
/// - 実行時のポートは、どちらも同じ code・同じ文言の、やり直さない失敗にする（権限の誤りを一時障害として
///   繰り返さない）。
#[tokio::test]
async fn workflow_start_treats_missing_and_unreadable_targets_alike() {
    let Some(h) = setup().await else { return };
    let executor = h.executor(None, vec![], None);
    let suffix = Uuid::new_v4().simple().to_string();
    let child_name = format!("child-{}", &suffix[..12]);
    let missing_name = format!("missing-{}", &suffix[..12]);
    // alice のワークフロー（bob には共有していない）。
    let (child_ir, _) = workflow_engine::complete_declared_scopes(&json!({
        "ir_version": 1, "name": child_name,
        "triggers": [{ "kind": "interactive" }],
        "nodes": [{ "id": "a", "type": "storage.list", "params": {} }],
        "edges": []
    }));
    let child = h.save(&child_ir).await;
    let bob = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "bob".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(h.tenant.clone()),
        },
        h.org.clone(),
        h.tenant.clone(),
    );
    let parent = |name: &str| {
        json!({
            "ir_version": 1, "name": format!("p-{}", &name[..name.len().min(40)]),
            "declared_scopes": ["workflow.start"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "start", "type": "workflow.start", "params": { "name": name } }],
            "edges": []
        })
    };

    // 保存時の材料: alice には見え、bob には無い名前と同じく見えない。
    let both = json!({ "nodes": [
        { "id": "s1", "type": "workflow.start", "params": { "name": child_name } },
        { "id": "s2", "type": "workflow.start", "params": { "name": missing_name } },
        { "id": "s3", "type": "workflow.start", "params": { "name": "" } }
    ] });
    let for_alice = api::workflow_catalog::workflow_start_targets(&h.artifacts, &h.alice, &both)
        .await
        .unwrap();
    let for_bob = api::workflow_catalog::workflow_start_targets(&h.artifacts, &bob, &both)
        .await
        .unwrap();
    assert_eq!(for_alice, Some([child_name.clone()].into_iter().collect()));
    assert_eq!(for_bob, Some(std::collections::BTreeSet::new()));
    // workflow.start が無い IR は照合しない。
    assert_eq!(
        api::workflow_catalog::workflow_start_targets(&h.artifacts, &bob, &json!({ "nodes": [] }))
            .await
            .unwrap(),
        None
    );
    // V4: 見えない相手は、無い名前と同じ誤り（欄の下に出す path つき）。
    let catalog = Catalog {
        workflows: for_bob,
        ..Catalog::default()
    };
    let (ir, _) = workflow_engine::complete_declared_scopes(&parent(&child_name));
    let errors = workflow_engine::validate(&ir, &catalog).unwrap_err();
    let (ir2, _) = workflow_engine::complete_declared_scopes(&parent(&missing_name));
    let errors2 = workflow_engine::validate(&ir2, &catalog).unwrap_err();
    let shape = |e: &[workflow_engine::ValidationError]| {
        e.iter()
            .map(|e| (e.code.clone(), e.path.clone(), e.message.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&errors), shape(&errors2));
    assert_eq!(errors[0].code, "ir.unknown_workflow");
    assert_eq!(errors[0].path.as_deref(), Some("/params/name"));

    // 実行時: bob の実行は、見えない相手も無い名前も、同じやり直さない失敗になる。
    let mut outcomes = Vec::new();
    let mut parents = Vec::new();
    for name in [&child_name, &missing_name] {
        let (wf, _) = h
            .workflows
            .create(&bob, &parent(name), &Catalog::default(), None)
            .await
            .expect("bob の親を保存");
        parents.push(wf);
        let run_id = h
            .launcher
            .start_interactive(&bob, wf, &json!({}))
            .await
            .unwrap()
            .unwrap();
        let worker = workflow_engine::WorkflowWorker::new(
            h.runs.clone(),
            Arc::clone(&executor),
            WorkerConfig::default(),
        )
        .scoped_to_tenant(&h.tenant);
        while worker.claim_and_run_once("w1").await.unwrap() {}
        let row: (Option<String>, Option<String>, Option<bool>, i32) = sqlx::query_as(
            "SELECT error->>'code', error->>'message', (error->>'retryable')::bool, attempt \
             FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'start'",
        )
        .bind(&h.tenant)
        .bind(run_id)
        .fetch_one(&h.pool)
        .await
        .unwrap();
        outcomes.push(row);
    }
    let child_runs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&h.tenant)
    .bind(child)
    .fetch_one(&h.pool)
    .await
    .unwrap();

    // 片付け（run の履歴・ワークフロー）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    h.artifacts.delete(&h.alice, child, None).await.unwrap();
    for wf in parents {
        h.artifacts.delete(&bob, wf, None).await.unwrap();
    }

    assert_eq!(child_runs, 0, "見えない相手を実行した");
    assert_eq!(outcomes[0].0.as_deref(), Some("not_found"));
    assert_eq!(
        outcomes[0].2,
        Some(false),
        "権限の誤りをやり直せる失敗にした"
    );
    assert_eq!(outcomes[0].3, 1);
    assert_eq!(
        (&outcomes[0].0, &outcomes[0].1, outcomes[0].2),
        (&outcomes[1].0, &outcomes[1].1, outcomes[1].2),
        "見えない相手と無い名前で誤りが違う"
    );
    assert!(
        outcomes[0].1.as_deref().unwrap().starts_with(
            "実行するワークフローが見つかりません。削除されたか、見る権限がありません。"
        ),
        "{:?}",
        outcomes[0].1
    );
}

/// パスの数字をステータスコードとして返すローカル HTTP サーバ（`/s/404` は 404）。返り値は base URL。
async fn spawn_http_status() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let status: u16 = head
                    .split_whitespace()
                    .nth(1)
                    .and_then(|p| p.strip_prefix("/s/"))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(200);
                let body = br#"{"error":"nope"}"#;
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(body).await;
            });
        }
    });
    format!("http://{addr}")
}

/// 本番のポートで、相手の誤りを失敗にする扱いを確かめる（BUG-99）。省略（既存の定義）は今のまま成功として
/// status を返し、fail_on_error_status なら 4xx はやり直さない失敗、5xx はやり直せる失敗にする。
#[tokio::test]
async fn http_error_statuses_through_the_production_port() {
    let Some(h) = setup().await else { return };
    let base = spawn_http_status().await;
    let executor = h.executor(None, vec!["127.0.0.1".into()], None);
    let mut outcomes = Vec::new();
    let mut workflows = Vec::new();
    for (i, (path, fail)) in [
        ("s/404", None),
        ("s/404", Some(true)),
        ("s/503", Some(true)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut params = json!({ "method": "POST", "url": format!("{base}/{path}") });
        if let Some(f) = fail {
            params["fail_on_error_status"] = json!(f);
        }
        let wf = h
            .save(&json!({
                "ir_version": 1, "name": format!("http-status-{i}"),
                "declared_scopes": ["http.egress"],
                "triggers": [{ "kind": "interactive" }],
                "nodes": [{ "id": "send", "type": "http.request", "params": params }],
                "edges": []
            }))
            .await;
        workflows.push(wf);
        let (run_id, status) = h.run(wf, &json!({}), Arc::clone(&executor)).await;
        let row: (Option<String>, Option<bool>, Option<Value>) = sqlx::query_as(
            "SELECT error->>'code', (error->>'retryable')::bool, output FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'send'",
        )
        .bind(&h.tenant)
        .bind(run_id)
        .fetch_one(&h.pool)
        .await
        .unwrap();
        outcomes.push((status, row));
    }
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for wf in workflows {
        h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    }

    let (status, (code, _, output)) = &outcomes[0];
    assert_eq!((*status, code.as_deref()), (RunStatus::Succeeded, None));
    assert_eq!(output.as_ref().unwrap()["status"], 404);
    let (status, (code, retryable, _)) = &outcomes[1];
    assert_eq!(*status, RunStatus::Failed);
    assert_eq!(
        (code.as_deref(), *retryable),
        (Some("http_error_status"), Some(false))
    );
    // 5xx はやり直せる失敗（試す回数が 1 なので実行は失敗で終わる）。
    let (status, (code, retryable, _)) = &outcomes[2];
    assert_eq!(*status, RunStatus::Failed);
    assert_eq!(
        (code.as_deref(), *retryable),
        (Some("http_retryable_status"), Some(true))
    );
}

// ------- BUG-181: 外部への送信は名前解決の後のアドレスを確かめる（本番のポート）-----------------------

/// 決めた名前だけを決めたアドレスに解決する（OS の DNS を使わない・引いた名前を数える）。
struct MapResolver {
    map: std::collections::HashMap<String, std::net::SocketAddr>,
    lookups: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl sandbox_client::net_guard::HostResolver for MapResolver {
    async fn lookup(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, &'static str> {
        self.lookups.lock().unwrap().push(host.to_string());
        self.map
            .get(host)
            .map(|a| vec![std::net::SocketAddr::new(a.ip(), port)])
            .ok_or("ホストを解決できませんでした")
    }
}

/// 許可（http_allowlist）・内部の送り先（http_internal_hosts）・名前解決を決めてワークフローを実行し、実行の状態と、
/// 送るブロック（send）の誤りを返す。
async fn run_with_http(
    h: &Harness,
    wf: Uuid,
    (allow, internal): (&str, &[&str]),
    resolver: Arc<MapResolver>,
) -> (RunStatus, (Option<String>, Option<bool>)) {
    let allowlist = vec![allow.to_string()];
    let internal: Vec<String> = internal.iter().map(|h| (*h).to_string()).collect();
    let http = api::workflow_runtime::HttpEgress::new(&internal).with_resolver(resolver as _);
    let executor = h.executor_with_http(None, allowlist, None, http);
    let (run, status) = h.run(wf, &json!({}), executor).await;
    let error = sqlx::query_as::<_, (Option<String>, Option<bool>)>(
        "SELECT error->>'code', (error->>'retryable')::bool FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'send'",
    )
    .bind(&h.tenant)
    .bind(run)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    (status, error)
}

/// 「外部 API 呼び出し」と script の `Shiki.http.request` は、本番のポートで、名前解決の後のアドレスが内部の
/// ネットワークなら送らない（BUG-181）。
/// - 管理者の許可（http_allowlist）に書いただけでは、ワイルドカードでもそのままのホストでも、内部の IP に解決される
///   名前へは送らない（許可は公開の SaaS を書く場所でもある・D-135）。
/// - 管理者が内部の送り先（http_internal_hosts）にも書くと送れる。名前は 1 回だけ引き、確かめたアドレスに送る
///   （`.invalid` は OS の DNS では引けない）。
/// - リンクローカル・クラウドのメタデータは、内部の送り先に書いても送らない。
/// 断った失敗は、送り先のアドレスの code で、やり直さない。
#[tokio::test]
async fn http_to_internal_addresses_is_refused_through_the_production_port() {
    let Some(h) = setup().await else { return };
    let (base, seen) = spawn_http_recorder().await;
    let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();
    let resolver = || {
        Arc::new(MapResolver {
            map: [
                ("intranet.invalid", ([127, 0, 0, 1], port).into()),
                ("metadata.invalid", ([169, 254, 169, 254], 80).into()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
            lookups: std::sync::Mutex::new(Vec::new()),
        })
    };
    // ブロックと script の 2 つの経路を、別々のワークフローで送る（片方の失敗で、もう片方が実行されないことがない）。
    let node = |host: &str| {
        json!({ "id": "send", "type": "http.request", "params": {
            "method": "POST", "url": format!("http://{host}:{port}/node"), "body": "x" } })
    };
    let script = |host: &str| {
        json!({ "id": "send", "type": "script.run", "params": { "source": { "inline": format!(
            "function main(input) {{ return Shiki.http.request({{ method: \"POST\", \
             url: \"http://{host}:{port}/script\", body: \"y\" }}).status; }}") } } })
    };
    let ir = |name: &str, node: Value| {
        json!({
            "ir_version": 1, "name": name,
            "declared_scopes": ["http.egress"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [node],
            "edges": [],
        })
    };
    let mut workflows = Vec::new();
    for (name, n) in [
        ("http-internal-node", node("intranet.invalid")),
        ("http-internal-script", script("intranet.invalid")),
        ("http-metadata-node", node("metadata.invalid")),
        ("http-metadata-script", script("metadata.invalid")),
    ] {
        workflows.push(h.save(&ir(name, n)).await);
    }
    // ① 許可に書いただけ（ワイルドカード・そのままのホスト）: 内部の IP に解決される名前へは送らない。
    let wildcard_node = run_with_http(&h, workflows[0], ("*.invalid", &[]), resolver()).await;
    let wildcard_script = run_with_http(&h, workflows[1], ("*.invalid", &[]), resolver()).await;
    let exact = ("intranet.invalid", &[][..]);
    let exact_node = run_with_http(&h, workflows[0], exact, resolver()).await;
    let exact_script = run_with_http(&h, workflows[1], exact, resolver()).await;
    let seen_after_refusal = seen.lock().await.len();
    // ② 内部の送り先にも書くと送れる（名前は送るたびに 1 回だけ引く）。
    let r2 = resolver();
    let both = ("intranet.invalid", &["intranet.invalid"][..]);
    let explicit_node = run_with_http(&h, workflows[0], both, Arc::clone(&r2)).await;
    let explicit_script = run_with_http(&h, workflows[1], both, Arc::clone(&r2)).await;
    let requests = seen.lock().await.clone();
    // ③ リンクローカル・メタデータは、内部の送り先に書いても送らない。
    let meta = ("metadata.invalid", &["metadata.invalid"][..]);
    let meta_node = run_with_http(&h, workflows[2], meta, resolver()).await;
    let meta_script = run_with_http(&h, workflows[3], meta, resolver()).await;

    // 片付け（run の履歴・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    for id in workflows {
        h.artifacts.delete(&h.alice, id, None).await.unwrap();
    }
    h.delete_fga_store().await;

    let refused = |code: &str| (RunStatus::Failed, (Some(code.to_string()), Some(false)));
    assert_eq!(wildcard_node, refused("destination_internal"));
    assert_eq!(wildcard_script, refused("destination_internal"));
    assert_eq!(exact_node, refused("destination_internal"));
    assert_eq!(exact_script, refused("destination_internal"));
    assert_eq!(seen_after_refusal, 0, "内部へ送っていない");

    assert_eq!(explicit_node, (RunStatus::Succeeded, (None, None)));
    assert_eq!(explicit_script, (RunStatus::Succeeded, (None, None)));
    assert!(
        requests.iter().any(|r| r.starts_with("POST /node ")),
        "{requests:?}"
    );
    assert!(
        requests.iter().any(|r| r.starts_with("POST /script ")),
        "{requests:?}"
    );
    assert_eq!(
        *r2.lookups.lock().unwrap(),
        ["intranet.invalid", "intranet.invalid"],
        "送るたびに 1 回だけ引く"
    );

    assert_eq!(meta_node, refused("destination_link_local"));
    assert_eq!(meta_script, refused("destination_link_local"));
}

// ------- GAP-19: 「外部 API 呼び出し」の作成者のヘッダー（本番のポート）-----------------------

/// 「外部 API 呼び出し」の「ヘッダー」は、本番のポートで相手に届く（値の差し込み・作成者の Content-Type）。送っては
/// いけないヘッダーを書いた定義は保存できない（GAP-19）。
#[tokio::test]
async fn http_request_node_sends_author_headers_through_the_production_port() {
    let Some(h) = setup().await else { return };
    let (base, seen) = spawn_http_recorder().await;
    let ir = |name: &str, headers: Value| {
        json!({
            "ir_version": 1, "name": name,
            "declared_scopes": ["http.egress"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "send", "type": "http.request", "params": {
                "method": "POST", "url": format!("{base}/hdr"), "body": "品目=交通費",
                "headers": headers } }],
            "edges": [],
        })
    };
    let wf = h
        .save(&ir(
            "http-node-headers",
            json!({
                "X-Request-Id": { "$template": "req-{id}", "vars": { "id": { "$from": "input", "path": "/id" } } },
                "Content-Type": "text/plain; charset=utf-8"
            }),
        ))
        .await;
    let refused = h
        .workflows
        .create(
            &h.alice,
            &ir("http-node-host", json!({ "Host": "internal.example.com" })),
            &Catalog::default(),
            None,
        )
        .await;
    let (run, status) = h
        .run(
            wf,
            &json!({ "id": 7 }),
            h.executor(None, vec!["127.0.0.1".into()], None),
        )
        .await;
    let error: Option<Value> =
        sqlx::query_scalar("SELECT error FROM step_execution WHERE tenant_id = $1 AND run_id = $2")
            .bind(&h.tenant)
            .bind(run)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    let requests = seen.lock().await.clone();

    // 片付け（run の履歴・ワークフロー・OpenFGA のストア）。
    h.runs.purge_tenant_history(&h.tenant).await.unwrap();
    h.artifacts.delete(&h.alice, wf, None).await.unwrap();
    if let Ok((id, _)) = &refused {
        h.artifacts.delete(&h.alice, *id, None).await.unwrap();
    }
    h.delete_fga_store().await;

    assert_eq!(status, RunStatus::Succeeded, "{error:?}");
    let raw = requests
        .iter()
        .find(|r| r.starts_with("POST /hdr "))
        .unwrap_or_else(|| panic!("/hdr への要求が無い: {requests:?}"));
    let lower = raw.to_ascii_lowercase();
    assert!(lower.contains("\r\nx-request-id: req-7\r\n"), "{raw}");
    assert!(
        lower.contains("\r\ncontent-type: text/plain; charset=utf-8\r\n"),
        "{raw}"
    );
    assert!(lower.contains("\r\nidempotency-key: "), "{raw}");
    let err = format!("{:?}", refused.expect_err("Host は保存できない"));
    assert!(
        err.contains("ヘッダー「Host」は Shiki が決めるため、指定できません。"),
        "{err}"
    );
}
