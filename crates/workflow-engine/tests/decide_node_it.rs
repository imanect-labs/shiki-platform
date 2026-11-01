//! 判定のノード（`ai.decide`・FEAT-1）の executor（fake ポート・DB 不要）。
//!
//! 検証: ポートへ渡す要求（モデル・解決した材料・質問・step の冪等キー）・答えによる出口の選び方（値で・
//! 確からしさで・しきい値ちょうど・届かない）・出力の形・監査に載せるもの（出口とその質問の確からしさだけ）・
//! 材料の型と大きさ・質問と合わない応答・ポートの誤り・既定のポート（未構成）・スコープの宣言が空でも動くこと。

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
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmDecideReq,
    LlmInvokeReq, NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor, NodeResult,
};

/// llm_decide に仕込んだ応答を順に返し、受け取った要求を残すポート。
#[derive(Default)]
struct ScriptedDecider {
    replies: Mutex<VecDeque<Result<Value, PortError>>>,
    reqs: Mutex<Vec<LlmDecideReq>>,
}

impl ScriptedDecider {
    fn new(replies: Vec<Result<Value, PortError>>) -> Arc<Self> {
        Arc::new(ScriptedDecider {
            replies: Mutex::new(replies.into()),
            reqs: Mutex::default(),
        })
    }

    fn reqs(&self) -> Vec<LlmDecideReq> {
        self.reqs.lock().unwrap().clone()
    }
}

fn unused() -> PortError {
    PortError::invalid("この試験では使わない")
}

#[async_trait]
impl NodePorts for ScriptedDecider {
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
    async fn llm_invoke(&self, _: &ExecCtx, _: LlmInvokeReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn llm_decide(&self, ec: &ExecCtx, req: LlmDecideReq) -> Result<Value, PortError> {
        assert_eq!(ec.step_path.as_deref(), Some("judge"));
        self.reqs.lock().unwrap().push(req);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(PortError::invalid("仕込んだ応答が尽きた")))
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

/// llm_decide を実装しないポート（トレイトの既定＝未構成）。
struct NoDecider;

#[async_trait]
impl NodePorts for NoDecider {
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

#[derive(Default)]
struct Audit(Mutex<Vec<(String, Value)>>);
impl CapabilityAudit for Audit {
    fn record(&self, _: &str, api: &str, _: bool, meta: &Value) {
        self.0.lock().unwrap().push((api.to_string(), meta.clone()));
    }
}

impl Audit {
    fn rows(&self) -> Vec<(String, Value)> {
        self.0.lock().unwrap().clone()
    }
}

fn executor(ports: Arc<dyn NodePorts>, audit: &Arc<Audit>) -> CapabilityNodeExecutor {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    CapabilityNodeExecutor::new(
        ports,
        EffectJournal::new(pool),
        Arc::clone(audit) as Arc<dyn CapabilityAudit>,
    )
}

/// 前のノード `create` の出力に問い合わせの本文がある step（スコープの宣言は空）。
fn ctx() -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        run_started_at: chrono::Utc::now(),
        step_path: "judge".into(),
        idempotency_key: "wf:t1:0:judge".into(),
        attempt: 1,
        principal: "alice".into(),
        principal_kind: "user".into(),
        input: json!({}),
        trigger: json!({}),
        node_outputs: json!({ "create": { "data": { "body": "請求書の金額が違います。返金してください。" } } }),
        each: None,
        trace_id: None,
        scope_ceiling: vec![],
        cancel: workflow_engine::run::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
    }
}

fn params(route: Value) -> Value {
    let mut p = json!({
        "model": "decide-stub",
        "state": { "$from": "nodes.create.output", "path": "/data" },
        "questions": [
            { "key": "category", "type": "choice", "instructions": "問い合わせの種類はどれか",
              "options": [ { "key": "billing", "description": "請求・返金" }, { "key": "bug", "description": "不具合" } ] },
            { "key": "priority", "type": "score", "instructions": "対応の緊急度", "levels": ["低", "中", "高"] },
            { "key": "angry", "type": "boolean", "instructions": "顧客は強い不満を示しているか" }
        ]
    });
    if !route.is_null() {
        p["route"] = route;
    }
    p
}

/// 判定の応答（category の答えと確からしさを変えられる）。
fn reply(category: &str, confidence: f64) -> Value {
    let other = if category == "billing" {
        "bug"
    } else {
        "billing"
    };
    json!({
        "resolved_model": "claude-haiku-5-5+llm_decider/v1/k1/low",
        "answers": {
            "category": { "type": "choice", "value": category, "confidence": confidence,
                          "probabilities": { category: confidence, other: 1.0 - confidence } },
            "priority": { "type": "score", "value": 1.1, "level_index": 1, "confidence": 0.6,
                          "probabilities": [0.2, 0.6, 0.2] },
            "angry": { "type": "boolean", "value": true, "probability": 0.75, "confidence": 0.75 }
        },
        "usage": { "prompt_tokens": 120, "completion_tokens": 30 },
        "calibrated": false, "method": "verbalized", "samples": 1,
        "provider": "llm_decider"
    })
}

async fn run(ports: Arc<dyn NodePorts>, audit: &Arc<Audit>, p: &Value) -> NodeResult {
    executor(ports, audit).execute("ai.decide", p, &ctx()).await
}

/// step 属性 `cache` を書いたノードだけ、ポートへこのワークフローと期限を渡す。当たったかは出力と監査の
/// `cached` に出る（材料・ほかの答えは監査に載せないまま）。
#[tokio::test]
async fn the_step_cache_reaches_the_port_and_a_hit_is_reported() {
    let mut hit = reply("billing", 0.9);
    hit["cached"] = json!(true);
    let ports = ScriptedDecider::new(vec![Ok(hit)]);
    let audit = Arc::new(Audit::default());
    let p = params(json!({ "question": "category", "min_confidence": 0.8 }));
    let workflow_id = Uuid::new_v4();
    let ctx = NodeContext {
        workflow_id,
        cache: Some(workflow_engine::ir::StepCache { ttl_sec: 3600 }),
        ..ctx()
    };
    let res = executor(Arc::clone(&ports) as _, &audit)
        .execute("ai.decide", &p, &ctx)
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.taken_ports, ["billing"]);
    assert_eq!(res.output["cached"], true);
    assert_eq!(
        ports.reqs()[0].cache,
        Some(workflow_engine::DecideCache {
            workflow_id,
            ttl_sec: 3600
        })
    );
    let rows = audit.rows();
    assert_eq!(rows[0].1["cached"], true);
    assert!(!rows[0].1.to_string().contains("請求書"));
    // 決定器の名前を返さないポート（既定の実装・古いポート）では null。
    let mut bare = reply("billing", 0.9);
    bare.as_object_mut().unwrap().remove("provider");
    let res = run(ScriptedDecider::new(vec![Ok(bare)]) as _, &audit, &p).await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["provider"], serde_json::Value::Null);
}

/// AI の自信を引き上げた質問（BUG-124・D-114）は、出力の `raised_questions` に質問の順で載る（作る人が実行の詳細で
/// 見る）。引き上げた答えも同じ規則で振り分ける。監査には載せない（質問のキーは `questions` にある）。
#[tokio::test]
async fn raised_questions_reach_the_output_in_question_order() {
    let mut raised = reply("bug", 0.5);
    raised["raised_questions"] = json!(["angry", "category"]);
    let ports = ScriptedDecider::new(vec![Ok(raised)]);
    let audit = Arc::new(Audit::default());
    let p = params(json!({ "question": "category", "min_confidence": 0.8 }));
    let res = run(ports as _, &audit, &p).await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.taken_ports, ["uncertain"]);
    assert_eq!(res.output["raised_questions"], json!(["category", "angry"]));
    assert!(audit.rows()[0].1.get("raised_questions").is_none());
    // 質問に無いキーを返すポートは、質問と合わない応答として止める（後続へ渡さない）。
    let mut wrong = reply("bug", 0.5);
    wrong["raised_questions"] = json!(["refund"]);
    let res = run(ScriptedDecider::new(vec![Ok(wrong)]) as _, &audit, &p).await;
    assert!(!res.ok);
    assert_eq!(code(&res), ("decision_invalid_response".into(), false));
}

fn code(r: &NodeResult) -> (String, bool) {
    let e = r.error.as_ref().unwrap();
    (e.code.clone(), e.retryable)
}

/// 値で振り分けると、確かな答えの選択肢の出口へ進む。ポートには解決した材料・質問・step のキーが渡る。
#[tokio::test]
async fn routes_by_value_to_the_chosen_option() {
    let ports = ScriptedDecider::new(vec![Ok(reply("billing", 0.9))]);
    let audit = Arc::new(Audit::default());
    let p = params(json!({ "question": "category", "min_confidence": 0.8 }));
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.taken_ports, ["billing"]);
    assert_eq!(
        res.output,
        json!({
            "answers": {
                "category": { "type": "choice", "value": "billing", "label": "請求・返金", "confidence": 0.9,
                              "probabilities": { "billing": 0.9, "bug": 1.0 - 0.9 } },
                "priority": { "type": "score", "value": 1.1, "level": "中", "level_index": 1,
                              "confidence": 0.6, "probabilities": [0.2, 0.6, 0.2] },
                "angry": { "type": "boolean", "value": true, "probability": 0.75, "confidence": 0.75 }
            },
            "model": "decide-stub",
            "resolved_model": "claude-haiku-5-5+llm_decider/v1/k1/low",
            "provider": "llm_decider",
            "calibrated": false, "method": "verbalized", "samples": 1, "cached": false,
            "routed": { "question": "category", "port": "billing" },
            "raised_questions": []
        })
    );
    let reqs = ports.reqs();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].model, "decide-stub");
    assert_eq!(
        reqs[0].state,
        json!({ "body": "請求書の金額が違います。返金してください。" })
    );
    let keys: Vec<&str> = reqs[0].questions.iter().map(|q| q.key.as_str()).collect();
    assert_eq!(keys, ["category", "priority", "angry"]);
    assert_eq!(reqs[0].idempotency_key, "wf:t1:0:judge");
    assert_eq!(
        reqs[0].cache, None,
        "cache を書かなければ決定キャッシュは使わない"
    );

    // 監査には出口と、振り分けに使った質問の確からしさだけ（材料・ほかの答えは載せない・H5）。
    let rows = audit.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "ai.decide");
    assert_eq!(
        rows[0].1,
        json!({
            "model": "decide-stub",
            "resolved_model": "claude-haiku-5-5+llm_decider/v1/k1/low",
            "provider": "llm_decider",
            "method": "verbalized", "samples": 1, "calibrated": false, "cached": false,
            "questions": ["category", "priority", "angry"],
            "port": "billing",
            "routed": { "question": "category", "confidence": 0.9 }
        })
    );
    let text = rows[0].1.to_string();
    for leak in ["請求書", "返金", "level", "probabilit", "0.75"] {
        assert!(!text.contains(leak), "{leak} が監査に載った: {text}");
    }
}

/// しきい値ちょうどは答えに従い、届かなければ uncertain（較正されていなくても同じ規則・H4）。
#[tokio::test]
async fn the_threshold_is_inclusive_and_below_it_goes_to_uncertain() {
    let p = params(json!({ "question": "category", "min_confidence": 0.8 }));
    for (conf, want) in [(0.8, "bug"), (0.7999, "uncertain")] {
        let ports = ScriptedDecider::new(vec![Ok(reply("bug", conf))]);
        let audit = Arc::new(Audit::default());
        let res = run(ports as _, &audit, &p).await;
        assert!(res.ok, "{:?}", res.error);
        assert_eq!(res.taken_ports, [want], "{conf}");
        assert_eq!(res.output["routed"]["port"], want);
        assert_eq!(audit.rows()[0].1["port"], want);
    }
    // はい/いいえで振り分けると true / false。
    let p = params(json!({ "question": "angry", "by": "value", "min_confidence": 0.7 }));
    let ports = ScriptedDecider::new(vec![Ok(reply("bug", 0.9))]);
    let res = run(ports as _, &Arc::new(Audit::default()), &p).await;
    assert_eq!(res.taken_ports, ["true"]);
}

/// 確からしさで振り分けると、答えによらず確かなら out・届かなければ uncertain。振り分けが無ければ out。
#[tokio::test]
async fn routes_by_confidence_and_without_a_route() {
    let p = params(json!({ "question": "category", "by": "confidence", "min_confidence": 0.85 }));
    for (conf, want) in [(0.9, "out"), (0.8, "uncertain")] {
        let ports = ScriptedDecider::new(vec![Ok(reply("billing", conf))]);
        let res = run(ports as _, &Arc::new(Audit::default()), &p).await;
        assert_eq!(res.taken_ports, [want], "{conf}");
    }
    let ports = ScriptedDecider::new(vec![Ok(reply("billing", 0.1))]);
    let audit = Arc::new(Audit::default());
    let res = run(ports as _, &audit, &params(Value::Null)).await;
    assert!(res.ok);
    assert_eq!(res.taken_ports, ["out"]);
    assert_eq!(res.output["routed"], Value::Null);
    assert_eq!(audit.rows()[0].1["port"], "out");
    assert_eq!(audit.rows()[0].1["routed"], Value::Null);
}

/// 材料は文字・オブジェクト・配列だけで、32 KiB まで（ポートを呼ばずに permanent で失敗する）。
#[tokio::test]
async fn the_state_must_be_text_or_structured_and_small() {
    let ports = ScriptedDecider::new(vec![]);
    let audit = Arc::new(Audit::default());
    let mut p = params(Value::Null);
    p["state"] = json!({ "$from": "nodes.create.output", "path": "/data/missing", "default": 3 });
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert_eq!(code(&res), ("invalid".into(), false));
    p["state"] = json!("x".repeat(32 * 1024 + 1));
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert_eq!(code(&res), ("decision_state_too_large".into(), false));
    // 解決できない参照は値式の誤り。
    p["state"] = json!({ "$from": "nodes.create.output", "path": "/nope" });
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert_eq!(code(&res).0, "expr_resolve_error");
    assert!(ports.reqs().is_empty(), "ポートを呼ばない");
    assert!(audit.rows().is_empty());
}

/// 質問と合わない応答は後続へ渡さず permanent で失敗する。ポートの誤りはそのまま返す。
#[tokio::test]
async fn mismatching_replies_and_port_errors_fail_the_step() {
    let mut wrong = reply("billing", 0.9);
    wrong["answers"]["category"]["value"] = json!("refund");
    let ports = ScriptedDecider::new(vec![
        Ok(wrong),
        Err(PortError::unavailable("一時障害")),
        Err(PortError::new("llm_refused", "拒否", false)),
    ]);
    let audit = Arc::new(Audit::default());
    let p = params(json!({ "question": "category", "min_confidence": 0.8 }));
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert_eq!(code(&res), ("decision_invalid_response".into(), false));
    assert!(!res.error.as_ref().unwrap().message.contains("refund"));
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert_eq!(code(&res), ("unavailable".into(), true));
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert_eq!(code(&res), ("llm_refused".into(), false));
    assert!(audit.rows().is_empty(), "失敗は監査に成功として載せない");
}

/// 保存を通らない経路の IR でも、params の誤りはポートを呼ばずに止める（多層防御）。
#[tokio::test]
async fn invalid_params_are_rejected_before_the_port() {
    let ports = ScriptedDecider::new(vec![]);
    let audit = Arc::new(Audit::default());
    let p = params(json!({ "question": "priority", "min_confidence": 0.8 }));
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert_eq!(code(&res), ("bad_params".into(), false));
    assert!(res
        .error
        .unwrap()
        .message
        .contains("/params/route/question"));
    assert!(ports.reqs().is_empty());
}

/// llm_decide を結線しないポート（既定の実装）は「未構成」で permanent に失敗する（H1）。
#[tokio::test]
async fn the_default_port_is_unsupported() {
    let audit = Arc::new(Audit::default());
    let res = run(Arc::new(NoDecider), &audit, &params(Value::Null)).await;
    assert_eq!(code(&res), ("unsupported".into(), false));
    assert!(res.error.unwrap().message.contains("ai.decide"));
}

/// 出口を分ける判定のしきい値が空なら、AI を呼ばずにやり直さない誤りで失敗する（実行の前に readiness が止める
/// が、通らない経路の多層防御・既定を置かない・D-88）。
#[tokio::test]
async fn a_blank_threshold_fails_without_calling_the_decider() {
    let ports = ScriptedDecider::new(vec![Ok(reply("billing", 0.9))]);
    let audit = Arc::new(Audit::default());
    let p = params(json!({ "question": "category" }));
    let res = run(Arc::clone(&ports) as _, &audit, &p).await;
    assert!(!res.ok);
    assert_eq!(code(&res), ("bad_params".to_string(), false));
    assert!(
        res.error
            .as_ref()
            .unwrap()
            .message
            .contains("しきい値（AI の自信）"),
        "{:?}",
        res.error
    );
    assert!(ports.reqs().is_empty(), "AI を呼ばない");
}

/// 質問の「AI に尋ねること」が空なら、AI を呼ばずにやり直さない誤りで失敗する（実行の前に readiness が止める
/// が、通らない経路の多層防御・空のままでも保存はできる・D-108）。
#[tokio::test]
async fn a_blank_question_fails_without_calling_the_decider() {
    let ports = ScriptedDecider::new(vec![Ok(reply("billing", 0.9))]);
    let audit = Arc::new(Audit::default());
    for blank in ["", "   "] {
        let mut p = params(Value::Null);
        p["questions"][1]["instructions"] = json!(blank);
        let res = run(Arc::clone(&ports) as _, &audit, &p).await;
        assert!(!res.ok);
        assert_eq!(code(&res), ("bad_params".to_string(), false));
        assert_eq!(
            res.error.as_ref().unwrap().message,
            "「AI に尋ねること」が空です。入れるまで、このブロックは実行できません。"
        );
    }
    assert!(ports.reqs().is_empty(), "AI を呼ばない");
}
