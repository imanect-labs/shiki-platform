//! llm.invoke の構造化出力（`output_schema`・GAP-13）の executor の順序（fake ポート・DB 不要）。
//!
//! 検証: 出力の data・ポートへ渡る閉じたスキーマと思考の深さ・1 回だけの修復（会話・会計の接尾辞）・
//! 直しても合わないときの llm_output_invalid・上限と拒否は直さずに失敗・自由な本文から停止理由を外す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::too_many_lines
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;
use workflow_engine::ir::params::LlmEffort;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor, NodeResult,
};

/// llm_invoke に仕込んだ応答を順に返し、受け取った要求を残すポート（ほかの能力は使わない）。
#[derive(Default)]
struct ScriptedLlm {
    replies: Mutex<VecDeque<Value>>,
    reqs: Mutex<Vec<LlmInvokeReq>>,
}

impl ScriptedLlm {
    fn new(replies: Vec<Value>) -> Arc<Self> {
        Arc::new(ScriptedLlm {
            replies: Mutex::new(replies.into()),
            reqs: Mutex::default(),
        })
    }

    fn reqs(&self) -> Vec<LlmInvokeReq> {
        self.reqs.lock().unwrap().clone()
    }
}

fn unused() -> PortError {
    PortError::invalid("この試験では使わない")
}

#[async_trait]
impl NodePorts for ScriptedLlm {
    async fn storage_write(&self, _: &ExecCtx, _: StorageWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn storage_read(&self, _: &ExecCtx, _: Uuid) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn storage_list(
        &self,
        _: &ExecCtx,
        _: workflow_engine::StorageListReq,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn rag_search(&self, _: &ExecCtx, _: &str, _: Option<u32>) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn llm_invoke(&self, _: &ExecCtx, req: LlmInvokeReq) -> Result<Value, PortError> {
        self.reqs.lock().unwrap().push(req);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| PortError::invalid("仕込んだ応答が尽きた"))
    }
    async fn agent_invoke(
        &self,
        _: &ExecCtx,
        _: AgentInvokeReq,
        _: Arc<dyn workflow_engine::AgentToolGateway>,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn http_send(&self, _: &ExecCtx, _: HttpSendReq) -> Result<HttpSendResp, PortError> {
        Err(unused())
    }
    async fn resolve_secret(&self, _: &ExecCtx, _: &str) -> Result<ResolvedSecretView, PortError> {
        Err(unused())
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
    async fn csv_query(
        &self,
        _: &ExecCtx,
        _: workflow_engine::CsvQueryReq,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
}

#[derive(Default)]
struct Audit(Mutex<Vec<(String, Value)>>);
impl CapabilityAudit for Audit {
    fn record(&self, _: &str, api: &str, _: bool, meta: &Value) {
        self.0.lock().unwrap().push((api.to_string(), meta.clone()));
    }
}

fn executor(ports: &Arc<ScriptedLlm>, audit: &Arc<Audit>) -> CapabilityNodeExecutor {
    // 記録しない journal（DB を使わない・llm.invoke も journal を通る・GAP-28）。
    CapabilityNodeExecutor::new(
        Arc::clone(ports) as Arc<dyn NodePorts>,
        EffectJournal::unrecorded(),
        Arc::clone(audit) as Arc<dyn CapabilityAudit>,
    )
}

fn ctx() -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        run_started_at: chrono::Utc::now(),
        run_scheduled_at: None,
        step_path: "ai".into(),
        idempotency_key: "wf:t1:0:ai".into(),
        attempt: 1,
        principal: "alice".into(),
        principal_kind: "user".into(),
        input: json!({}),
        trigger: json!({}),
        node_outputs: Value::Null,
        each: None,
        trace_id: None,
        scope_ceiling: vec![],
        cancel: workflow_engine::run::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
    }
}

fn params() -> Value {
    json!({
        "prompt": "問い合わせを分類して",
        "effort": "low",
        "output_schema": {
            "type": "object",
            "required": ["category"],
            "properties": {
                "category": { "type": "string", "enum": ["billing", "bug"] },
                "note": { "type": "string", "maxLength": 5 }
            }
        }
    })
}

fn reply(text: &str, stop: &str) -> Value {
    json!({ "text": text, "model": "m1", "stop_reason": stop })
}

async fn run(ports: &Arc<ScriptedLlm>, audit: &Arc<Audit>, p: &Value) -> NodeResult {
    executor(ports, audit)
        .execute("llm.invoke", p, &ctx())
        .await
}

fn code(r: &NodeResult) -> String {
    r.error.as_ref().unwrap().code.clone()
}

/// 形に合う出力は data に載り、ポートには閉じたスキーマと思考の深さが渡る（停止理由は出力に載せない）。
#[tokio::test]
async fn valid_output_is_returned_as_data() {
    let ports = ScriptedLlm::new(vec![reply(r#"{"category":"bug"}"#, "end_turn")]);
    let audit = Arc::new(Audit::default());
    let res = run(&ports, &audit, &params()).await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(
        res.output,
        json!({ "text": r#"{"category":"bug"}"#, "model": "m1", "data": { "category": "bug" } })
    );
    let reqs = ports.reqs();
    assert_eq!(reqs.len(), 1);
    let schema = reqs[0].output_schema.as_ref().unwrap();
    assert_eq!(schema["additionalProperties"], false, "閉じた形で渡す");
    assert_eq!(
        schema["properties"]["note"]["maxLength"], 5,
        "制約は落とさない"
    );
    assert_eq!(reqs[0].effort, Some(LlmEffort::Low));
    assert!(reqs[0].repair.is_none() && reqs[0].call_suffix.is_none());
    assert_eq!(reqs[0].idempotency_key, "wf:t1:0:ai");
    let audits = audit.0.lock().unwrap();
    assert_eq!(audits[0].0, "llm.invoke");
    assert_eq!(audits[0].1["repaired"], false);
    assert!(
        !audits[0].1.to_string().contains("分類して"),
        "プロンプトを監査に載せない"
    );
}

/// 形に合わない出力は 1 回だけ直させる（[user][assistant: 直前の出力][user: 直す点]・会計は #c1）。
#[tokio::test]
async fn invalid_output_is_repaired_once() {
    let ports = ScriptedLlm::new(vec![
        reply(r#"{"category":"refund","extra":1}"#, "end_turn"),
        reply("```json\n{\"category\":\"billing\"}\n```", "end_turn"),
    ]);
    let audit = Arc::new(Audit::default());
    let res = run(&ports, &audit, &params()).await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["data"], json!({ "category": "billing" }));
    let reqs = ports.reqs();
    assert_eq!(reqs.len(), 2);
    let repair = reqs[1].repair.as_ref().expect("修復の依頼");
    assert_eq!(repair.previous_output, r#"{"category":"refund","extra":1}"#);
    assert!(
        repair
            .instruction
            .contains("- category: 選択肢にない値です"),
        "{}",
        repair.instruction
    );
    assert!(repair.instruction.contains("- extra: 受け付けない項目です"));
    assert_eq!(reqs[1].call_suffix.as_deref(), Some("#c1"));
    assert_eq!(
        reqs[1].idempotency_key, "wf:t1:0:ai",
        "step のキーはそのまま"
    );
    assert_eq!(reqs[1].prompt, reqs[0].prompt);
    assert_eq!(reqs[1].output_schema, reqs[0].output_schema);
    assert_eq!(audit.0.lock().unwrap()[0].1["repaired"], true);
}

/// 直しても合わなければ llm_output_invalid（再試行しない）。3 回目は呼ばない。
#[tokio::test]
async fn output_that_stays_invalid_fails_without_retry() {
    let ports = ScriptedLlm::new(vec![
        reply("はい、分類しました", "end_turn"),
        reply(r#"{"category":"bug","note":"とても長いメモ"}"#, "end_turn"),
        reply(r#"{"category":"bug"}"#, "end_turn"),
    ]);
    let audit = Arc::new(Audit::default());
    let res = run(&ports, &audit, &params()).await;
    assert!(!res.ok);
    let err = res.error.unwrap();
    assert_eq!(err.code, "llm_output_invalid");
    assert!(!err.retryable);
    assert!(
        err.message.contains("note: 5 文字までです"),
        "{}",
        err.message
    );
    assert_eq!(ports.reqs().len(), 2);
    assert!(audit.0.lock().unwrap().is_empty(), "成功の監査は書かない");
}

/// 上限で切れた・拒否された出力は直さずに失敗する（修復の呼び出しが切れても同じ）。
#[tokio::test]
async fn truncated_and_refused_outputs_are_not_repaired() {
    for (stop, want) in [
        ("max_tokens", "llm_output_truncated"),
        ("refusal", "llm_refused"),
    ] {
        let ports = ScriptedLlm::new(vec![reply(r#"{"category":"bug"}"#, stop)]);
        let res = run(&ports, &Arc::new(Audit::default()), &params()).await;
        assert_eq!(code(&res), want, "{stop}");
        assert!(!res.error.as_ref().unwrap().retryable);
        assert_eq!(ports.reqs().len(), 1, "{stop}: 直させない");
    }
    let ports = ScriptedLlm::new(vec![
        reply("not json", "end_turn"),
        reply("{\"cate", "max_tokens"),
    ]);
    let res = run(&ports, &Arc::new(Audit::default()), &params()).await;
    assert_eq!(code(&res), "llm_output_truncated");
}

/// 停止理由を返さないポート（既存の fake）は自然終了とみなす。自由な本文の出力からは停止理由を外す。
#[tokio::test]
async fn plain_output_keeps_its_shape() {
    let ports = ScriptedLlm::new(vec![
        json!({ "text": r#"{"category":"bug"}"#, "model": "m1" }),
    ]);
    let res = run(&ports, &Arc::new(Audit::default()), &params()).await;
    assert!(res.ok, "{:?}", res.error);
    let ports = ScriptedLlm::new(vec![reply("途中まで", "max_tokens")]);
    let res = run(
        &ports,
        &Arc::new(Audit::default()),
        &json!({ "prompt": "要約して" }),
    )
    .await;
    assert!(res.ok, "自由な本文は上限で切れても従来どおり成功");
    assert_eq!(res.output, json!({ "text": "途中まで", "model": "m1" }));
    let reqs = ports.reqs();
    assert!(reqs[0].output_schema.is_none() && reqs[0].effort.is_none());
}

/// 保存を通らない形の出力の形は、実行時にも送らずに失敗する（多層防御）。
#[tokio::test]
async fn malformed_schema_is_rejected_at_run_time() {
    let ports = ScriptedLlm::new(vec![]);
    let p = json!({ "prompt": "x", "output_schema": { "type": "object", "additionalProperties": true } });
    let res = run(&ports, &Arc::new(Audit::default()), &p).await;
    assert_eq!(code(&res), "bad_params");
    assert!(ports.reqs().is_empty());
}
