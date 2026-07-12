//! CapabilityNodeExecutor の未被覆ノード種別（rag.search/llm.invoke/agent.invoke/storage.list）と
//! ポートエラー写像・不正 params・scope 天井外を fake ポートで検証する（DB 不要・純ロジック）。
//! executor_it.rs が happy path 中心・500 行近いため、error/追加ノードは本ファイルに分離する。
//! journal を要するノード（workflow.start/csv.patch/csv.write）は実 DB が必要なので e2e で検証する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor,
};

/// 監査を捨てる no-op。
struct NoAudit;
impl CapabilityAudit for NoAudit {
    fn record(&self, _tenant_id: &str, _api: &str, _allowed: bool, _meta: &Value) {}
}

/// 成功既定・`fail=true` で全能力が upstream エラーを返す fake ポート。
struct FakePorts {
    fail: bool,
}
impl FakePorts {
    fn ok() -> Arc<Self> {
        Arc::new(FakePorts { fail: false })
    }
    fn erroring() -> Arc<Self> {
        Arc::new(FakePorts { fail: true })
    }
    fn gate<T>(&self, ok: T) -> Result<T, PortError> {
        if self.fail {
            Err(PortError::new("upstream", "boom", true))
        } else {
            Ok(ok)
        }
    }
}

#[async_trait]
impl NodePorts for FakePorts {
    async fn storage_write(&self, _c: &ExecCtx, _r: StorageWriteReq) -> Result<Value, PortError> {
        self.gate(json!({ "ok": true }))
    }
    async fn storage_read(&self, _c: &ExecCtx, _f: Uuid) -> Result<Value, PortError> {
        self.gate(json!({ "text": "x" }))
    }
    async fn storage_list(&self, _c: &ExecCtx, parent: Option<Uuid>) -> Result<Value, PortError> {
        self.gate(json!({ "parent": parent.map(|p| p.to_string()), "items": [] }))
    }
    async fn rag_search(
        &self,
        _c: &ExecCtx,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Value, PortError> {
        self.gate(json!({ "query": query, "top_k": top_k, "results": [] }))
    }
    async fn llm_invoke(&self, _c: &ExecCtx, req: LlmInvokeReq) -> Result<Value, PortError> {
        self.gate(json!({ "text": format!("echo:{}", req.prompt) }))
    }
    async fn agent_invoke(&self, _c: &ExecCtx, _r: AgentInvokeReq) -> Result<Value, PortError> {
        self.gate(json!({ "stdout": "done" }))
    }
    async fn http_send(&self, _c: &ExecCtx, _r: HttpSendReq) -> Result<HttpSendResp, PortError> {
        if self.fail {
            return Err(PortError::new("upstream", "boom", true));
        }
        Ok(HttpSendResp {
            status: 200,
            body: b"{}".to_vec(),
        })
    }
    async fn resolve_secret(
        &self,
        _c: &ExecCtx,
        _n: &str,
    ) -> Result<ResolvedSecretView, PortError> {
        self.gate(ResolvedSecretView {
            plaintext: b"t".to_vec(),
            allowed_hosts: vec![],
        })
    }
    async fn workflow_start(
        &self,
        _c: &ExecCtx,
        name: &str,
        _input: &Value,
    ) -> Result<Value, PortError> {
        self.gate(json!({ "run_id": Uuid::nil().to_string(), "name": name }))
    }
    async fn csv_query(&self, _c: &ExecCtx, _f: Uuid, _sql: &str) -> Result<Value, PortError> {
        self.gate(json!({ "columns": [], "rows": [] }))
    }
    async fn csv_patch(&self, _c: &ExecCtx, _r: CsvPatchReq) -> Result<Value, PortError> {
        self.gate(json!({ "version": 1 }))
    }
    async fn csv_write(&self, _c: &ExecCtx, _r: CsvWriteReq) -> Result<Value, PortError> {
        self.gate(json!({ "version": 0 }))
    }
}

fn lazy_journal() -> EffectJournal {
    EffectJournal::new(
        PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
            .unwrap(),
    )
}

fn exec(ports: Arc<FakePorts>) -> CapabilityNodeExecutor {
    CapabilityNodeExecutor::new(ports, lazy_journal(), Arc::new(NoAudit))
}

fn ctx() -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        step_path: "n1".into(),
        idempotency_key: "wf:t1:0:n1".into(),
        attempt: 1,
        principal: "wf".into(),
        principal_kind: "workflow".into(),
        input: json!({}),
        trigger: json!({}),
        node_outputs: Value::Null,
        each: None,
        trace_id: Some("trace-1".into()),
        // すべての能力スコープを天井に含める（scope ceiling ゲートを通す）。
        scope_ceiling: vec![
            "storage.read".into(),
            "rag.query".into(),
            "llm.invoke".into(),
            "agent.invoke".into(),
            "workflow.start".into(),
        ],
    }
}

// ---- 成功パス（未被覆ノードの本体を通す） ----

#[tokio::test]
async fn rag_llm_agent_storage_list_workflow_start_dispatch() {
    let exec = exec(FakePorts::ok());
    let c = ctx();

    let rag = exec
        .execute("rag.search", &json!({ "query": "find", "top_k": 5 }), &c)
        .await;
    assert!(rag.ok, "{:?}", rag.error);
    assert_eq!(rag.output["query"], json!("find"));

    let llm = exec
        .execute("llm.invoke", &json!({ "prompt": "hi" }), &c)
        .await;
    assert!(llm.ok, "{:?}", llm.error);
    assert_eq!(llm.output["text"], json!("echo:hi"));

    let agent = exec
        .execute("agent.invoke", &json!({ "instruction": "go" }), &c)
        .await;
    assert!(agent.ok, "{:?}", agent.error);

    let list = exec.execute("storage.list", &json!({}), &c).await;
    assert!(list.ok, "{:?}", list.error);
    // workflow.start は cross-TX journal（実 DB 必須）なので e2e で検証する。
}

// ---- ポートエラーは NodeResult::fail に写る（code/retryable 保存） ----

#[tokio::test]
async fn port_error_maps_to_node_failure() {
    let exec = exec(FakePorts::erroring());
    let c = ctx();
    // journal 不要ノードのみ（workflow.start/csv.* は cross-TX journal で実 DB が要る）。
    for nt in ["rag.search", "llm.invoke", "agent.invoke", "storage.list"] {
        let params = match nt {
            "rag.search" => json!({ "query": "q" }),
            "llm.invoke" => json!({ "prompt": "p" }),
            "agent.invoke" => json!({ "instruction": "i" }),
            _ => json!({}),
        };
        let r = exec.execute(nt, &params, &c).await;
        assert!(!r.ok, "{nt} は失敗するはず");
        let e = r.error.as_ref().unwrap();
        assert_eq!(e.code, "upstream", "{nt}");
        assert!(e.retryable, "{nt} は retryable を保持");
    }
}

// ---- パラメータ不正・値解決不能は失敗する ----

#[tokio::test]
async fn bad_params_and_unresolvable_values_fail() {
    let exec = exec(FakePorts::ok());
    let c = ctx();

    // prompt 欠落（必須）→ パース失敗。
    let miss = exec
        .execute("llm.invoke", &json!({ "model": "m" }), &c)
        .await;
    assert!(!miss.ok);

    // query が解決不能な $from → invalid。
    let unresolvable = exec
        .execute(
            "rag.search",
            &json!({ "query": { "$from": "nodes.missing.output", "path": "/x" } }),
            &c,
        )
        .await;
    assert!(!unresolvable.ok);
}

// ---- scope 天井外は out_of_scope（監査あり・別経路） ----

#[tokio::test]
async fn out_of_scope_is_denied() {
    let exec = exec(FakePorts::ok());
    let mut c = ctx();
    c.scope_ceiling = vec![]; // 何も許可しない。
    let r = exec
        .execute("rag.search", &json!({ "query": "q" }), &c)
        .await;
    assert!(!r.ok);
    assert_eq!(r.error.as_ref().unwrap().code, "out_of_scope");
}
