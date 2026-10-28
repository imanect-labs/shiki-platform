//! 打ち切られた script の `Shiki.*` 呼び出しの単体テスト（実 ScriptEngine・fake ポート・DB 不要・BUG-96）。
//!
//! script は `spawn_blocking` の中で走るので、ワーカーが step を時間の上限で打ち切って（フューチャを捨てて）も
//! 止まらない。止める合図の後の `Shiki.*` 呼び出しを断らないと、失敗にした step の外部への送信が後から起き、
//! やり直しの送信と二重になる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// 読み取りに時間がかかり、外部への送信を数えるポート（ほかの能力は使わない）。
#[derive(Default)]
struct SlowReadPorts {
    sent: Mutex<Vec<String>>,
}

fn unused() -> PortError {
    PortError::unavailable("このテストでは使わない")
}

#[async_trait]
impl NodePorts for SlowReadPorts {
    async fn storage_write(&self, _: &ExecCtx, _: StorageWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn storage_read(&self, _: &ExecCtx, file_id: Uuid) -> Result<Value, PortError> {
        tokio::time::sleep(Duration::from_millis(600)).await;
        Ok(json!({ "id": file_id.to_string(), "text": "経費の申請" }))
    }
    async fn storage_list(&self, _: &ExecCtx, _: Option<Uuid>) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn rag_search(&self, _: &ExecCtx, _: &str, _: Option<u32>) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn llm_invoke(&self, _: &ExecCtx, _: LlmInvokeReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn agent_invoke(
        &self,
        _: &ExecCtx,
        _: AgentInvokeReq,
        _: Arc<dyn workflow_engine::AgentToolGateway>,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn http_send(&self, _: &ExecCtx, req: HttpSendReq) -> Result<HttpSendResp, PortError> {
        self.sent.lock().unwrap().push(req.url);
        Ok(HttpSendResp {
            status: 200,
            body: br#"{"ok":true}"#.to_vec(),
        })
    }
    async fn resolve_secret(&self, _: &ExecCtx, _: &str) -> Result<ResolvedSecretView, PortError> {
        Ok(ResolvedSecretView {
            plaintext: b"token".to_vec(),
            allowed_hosts: vec!["api.example.com".into()],
        })
    }
    async fn workflow_start(&self, _: &ExecCtx, _: &str, _: &Value) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn skill_resolve(
        &self,
        _: &ExecCtx,
        _: &str,
        _: &str,
    ) -> Result<workflow_engine::nodes::ResolvedSkillView, PortError> {
        Err(unused())
    }
    async fn csv_query(&self, _: &ExecCtx, _: Uuid, _: &str) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
}

struct NoAudit;
impl CapabilityAudit for NoAudit {
    fn record(&self, _: &str, _: &str, _: bool, _: &Value) {}
}

fn ctx(input: Value) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        step_path: "n1".into(),
        idempotency_key: "wf:t1:0:n1".into(),
        attempt: 1,
        principal: "wf".into(),
        principal_kind: "workflow".into(),
        input: input.clone(),
        trigger: input,
        node_outputs: Value::Null,
        each: None,
        trace_id: None,
        scope_ceiling: vec!["storage.read".into(), "http.egress".into()],
        cancel: workflow_engine::run::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
    }
}

fn executor(ports: Arc<SlowReadPorts>) -> CapabilityNodeExecutor {
    let journal = EffectJournal::new(
        PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
            .unwrap(),
    );
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    CapabilityNodeExecutor::new(ports as Arc<dyn NodePorts>, journal, Arc::new(NoAudit))
        .with_script_engine(engine, script_runtime::engine::Limits::default())
}

/// ファイルを読んでから外部へ送る script。
const READ_THEN_SEND: &str = r#"function main(input) {
  Shiki.storage.read(input.id);
  return Shiki.http.request({
    method: "POST",
    url: "https://api.example.com/hook",
    secret: { name: "tok" },
    body: { q: 1 },
  });
}"#;

/// 打ち切られて止める合図が立った後は、script が走り続けても外部へ送らない（ワーカーの打ち切りと同じ手順）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn script_cut_by_the_step_time_limit_sends_nothing_afterwards() {
    let ports = Arc::new(SlowReadPorts::default());
    let exec = executor(Arc::clone(&ports));
    let c = ctx(json!({ "id": Uuid::new_v4().to_string() }));
    let params = json!({ "source": { "inline": READ_THEN_SEND } });
    // ワーカーと同じく、時間の上限でフューチャを捨ててから止める合図を立てる（読み取りの途中で切る）。
    let cut = tokio::time::timeout(
        Duration::from_millis(150),
        exec.execute("script.run", &params, &c),
    )
    .await;
    assert!(cut.is_err(), "読み取りの途中で打ち切られる");
    c.cancel.cancel();
    // 読み取り（600 ミリ秒）が終わって script が続きを走らせても、送信は起きない。
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        ports.sent.lock().unwrap().is_empty(),
        "打ち切った step の送信が後から起きた: {:?}",
        ports.sent.lock().unwrap()
    );
}

/// 打ち切られなければ、同じ script は読んでから送る（上のテストが送らないのは合図のため）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn script_not_cut_sends_once() {
    let ports = Arc::new(SlowReadPorts::default());
    let exec = executor(Arc::clone(&ports));
    let c = ctx(json!({ "id": Uuid::new_v4().to_string() }));
    let res = exec
        .execute(
            "script.run",
            &json!({ "source": { "inline": READ_THEN_SEND } }),
            &c,
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(
        *ports.sent.lock().unwrap(),
        ["https://api.example.com/hook"]
    );
}
