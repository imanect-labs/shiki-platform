//! llm.invoke のポート（`ports.rs` から分割）: モデルの解決 → gateway の stream → 会計。
//!
//! 構造化出力（GAP-13）の検証と修復の順序はエンジンが決める（`workflow_engine::nodes::llm_structured`）。
//! ここは 1 回の呼び出しを組み、`{ text, model, stop_reason }` を返し、会計のキーを選ぶ:
//! - 使えた呼び出しは step の冪等キー ＋ `call_suffix`（修復は `#c1`）。
//! - 使えなかった呼び出し（途中で切れた・出力の形があって上限で切れた・拒否・形に合わない）で使用量の
//!   あるものは `{step のキー}#f{uuid}`。step のキーで記録すると、再試行や run の再開で後から成功した分が
//!   `ON CONFLICT DO NOTHING` で消える（engine.md §7.2）。形に合うかはエンジンと同じ検査
//!   （`workflow_engine::ir::output_schema::validate_output`）で見る。

use futures::StreamExt;
use llm_gateway::{
    Effort, GenerateRequest, GenerationRecord, LlmError, Message, ResponseFormat, Role, StopReason,
    StreamDelta, Usage,
};
use serde_json::{json, Value};
use uuid::Uuid;
use workflow_engine::ir::output_schema::validate_output;
use workflow_engine::ir::params::LlmEffort;
use workflow_engine::{ExecCtx, LlmInvokeReq, PortError};

use super::auth::auth_ctx;
use super::llm_error::structured_port_error;
use super::llm_model::resolve_for_workflow;
use super::ports::ProdNodePorts;

/// プロバイダへ送る出力の形の名前（OpenAI 互換の `json_schema.name`・Anthropic は使わない）。
const SCHEMA_NAME: &str = "output";

/// IR の思考の深さを gateway の中立形へ写す。
const fn gateway_effort(e: LlmEffort) -> Effort {
    match e {
        LlmEffort::Low => Effort::Low,
        LlmEffort::Medium => Effort::Medium,
        LlmEffort::High => Effort::High,
    }
}

/// 生成の要求を組む（`model` は解決済みの論理名・`resolved_effort` は組織の既定から決めた思考の深さ）。
///
/// ノードに書いた思考の深さは組織の既定より優先する（対応しないモデルでは gateway が落とす）。修復の依頼が
/// あれば、会話を `[user: prompt][assistant: 直前の出力][user: 依頼]` にする（user で終える・prefill にしない）。
fn build_request(
    req: &LlmInvokeReq,
    model: &str,
    resolved_effort: Option<Effort>,
) -> GenerateRequest {
    let mut messages = vec![Message::text(Role::User, req.prompt.clone())];
    if let Some(r) = &req.repair {
        messages.push(Message::text(Role::Assistant, r.previous_output.clone()));
        messages.push(Message::text(Role::User, r.instruction.clone()));
    }
    let mut greq = GenerateRequest::new(messages);
    greq.model = Some(model.to_string());
    greq.effort = req.effort.map(gateway_effort).or(resolved_effort);
    greq.system.clone_from(&req.system);
    greq.max_tokens = req.max_tokens;
    greq.response_format = req
        .output_schema
        .clone()
        .map(|schema| ResponseFormat::JsonSchema {
            name: SCHEMA_NAME.to_string(),
            schema,
        });
    greq
}

/// この呼び出しの結果を step が使えないか（会計を `#f{uuid}` で付けるか）。
fn unusable(
    req: &LlmInvokeReq,
    streamed: &Result<(), LlmError>,
    stop: Option<StopReason>,
    text: &str,
) -> bool {
    if streamed.is_err() {
        return true;
    }
    let Some(schema) = &req.output_schema else {
        return false;
    };
    matches!(stop, Some(StopReason::MaxTokens | StopReason::Refusal))
        || validate_output(schema, text).is_err()
}

/// 会計の冪等キー（[`unusable`] の呼び出しは呼び出しごとに一意）。
fn accounting_key(req: &LlmInvokeReq, unusable: bool) -> String {
    if unusable {
        format!("{}#f{}", req.idempotency_key, Uuid::new_v4().simple())
    } else {
        format!(
            "{}{}",
            req.idempotency_key,
            req.call_suffix.as_deref().unwrap_or_default()
        )
    }
}

const fn has_usage(u: &Usage) -> bool {
    u.prompt_tokens > 0 || u.completion_tokens > 0
}

fn preview(s: &str) -> String {
    let flat: String = s.chars().take(200).collect();
    flat.replace('\n', " ")
}

/// llm.invoke の 1 回の呼び出し（[`workflow_engine::NodePorts::llm_invoke`] の本体）。
pub(super) async fn invoke(
    ports: &ProdNodePorts,
    ec: &ExecCtx,
    req: LlmInvokeReq,
) -> Result<Value, PortError> {
    let ctx = auth_ctx(ec);
    // モデルは実行のたびに実行主体の組織の設定で決める（省略は組織の既定・BUG-12）。解決後の名前を
    // 生成・会計・出力に同じ値で使う（gateway の既定に任せると会計の名前とずれる）。
    let choice = resolve_for_workflow(&ports.gateway, ec, &ctx, req.model.as_deref()).await?;
    let greq = build_request(&req, &choice.model, choice.effort);
    let structured = req.output_schema.is_some();
    // Langfuse の所要時間に使う（未指定だと 0 秒と表示される）。
    let started_at = chrono::Utc::now();
    let mut stream = ports
        .gateway
        .stream(&ctx, greq)
        .await
        .map_err(|e| structured_port_error(ec, &e, structured))?;
    let mut text = String::new();
    let mut usage = Usage::default();
    let mut stop = None;
    // 途中で切れても、受け取った使用量は会計に残してから返す。
    let streamed = loop {
        match stream.next().await {
            Some(Ok(StreamDelta::TextDelta { text: t })) => text.push_str(&t),
            Some(Ok(StreamDelta::Done {
                stop_reason,
                usage: u,
            })) => {
                usage = u;
                stop = Some(stop_reason);
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => break Err(e),
            None => break Ok(()),
        }
    };
    let model = choice.model;
    let unusable = unusable(&req, &streamed, stop, &text);
    if !unusable || has_usage(&usage) {
        // 生成記録（trace_id で監査 ↔ Langfuse ↔ OTel を束ねる・本文プレビューは短く）。
        ports
            .gateway
            .record_generation(
                &ctx,
                &GenerationRecord {
                    idempotency_key: accounting_key(&req, unusable),
                    model: model.clone(),
                    usage,
                    trace_id: ec.trace_id.clone(),
                    input_preview: preview(&req.prompt),
                    output_preview: preview(&text),
                    app_id: None,
                    source: llm_gateway::UsageSource::Workflow,
                    // **false 固定。** 1 つの run に llm.invoke ノードが複数あると、
                    // 全ノードが run レベルの同じ trace_id を共有する。true にすると
                    // 後続ノードのプロンプトが trace input を上書きして、
                    // 最初のノードの入力が消える。ここからは「run 全体の入力」を
                    // 知る術が無いので、trace input は送らない（generation 側には残る）。
                    first_in_trace: false,
                    started_at: Some(started_at),
                },
            )
            .await;
    }
    streamed.map_err(|e| structured_port_error(ec, &e, structured))?;
    let mut out = json!({ "text": text, "model": model });
    if let Some(stop) = stop.and_then(|s| serde_json::to_value(s).ok()) {
        out["stop_reason"] = stop;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_gateway::Block;
    use workflow_engine::LlmRepair;

    fn req() -> LlmInvokeReq {
        LlmInvokeReq {
            model: None,
            system: Some("sys".into()),
            prompt: "分類して".into(),
            max_tokens: Some(2048),
            idempotency_key: "wf:t:r:ai".into(),
            effort: None,
            output_schema: None,
            repair: None,
            call_suffix: None,
        }
    }

    fn texts(g: &GenerateRequest) -> Vec<(Role, String)> {
        g.messages
            .iter()
            .map(|m| {
                let t = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        Block::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                (m.role, t)
            })
            .collect()
    }

    #[test]
    fn plain_request_is_unchanged() {
        let g = build_request(&req(), "fast", Some(Effort::Medium));
        assert_eq!(g.model.as_deref(), Some("fast"));
        assert_eq!(g.effort, Some(Effort::Medium), "組織の既定");
        assert_eq!(g.system.as_deref(), Some("sys"));
        assert_eq!(g.max_tokens, Some(2048));
        assert!(g.response_format.is_none());
        assert_eq!(texts(&g), vec![(Role::User, "分類して".to_string())]);
    }

    /// ノードの思考の深さは組織の既定より優先し、出力の形は response_format で求める。
    #[test]
    fn structured_request_carries_effort_and_schema() {
        let schema = json!({ "type": "object", "additionalProperties": false });
        let r = LlmInvokeReq {
            effort: Some(LlmEffort::Low),
            output_schema: Some(schema.clone()),
            ..req()
        };
        let g = build_request(&r, "fast", Some(Effort::High));
        assert_eq!(g.effort, Some(Effort::Low));
        assert_eq!(
            g.response_format,
            Some(ResponseFormat::JsonSchema {
                name: "output".into(),
                schema
            })
        );
        // 組織の既定が無いモデル（思考に対応しない）でもノードの指定は送る（gateway が落とす）。
        assert_eq!(build_request(&r, "fast", None).effort, Some(Effort::Low));
    }

    /// 修復は [user: prompt][assistant: 直前の出力][user: 依頼] の会話で、user で終える。
    #[test]
    fn repair_request_ends_with_the_user() {
        let r = LlmInvokeReq {
            repair: Some(LlmRepair {
                previous_output: "{\"x\":1}".into(),
                instruction: "直してください".into(),
            }),
            ..req()
        };
        let g = build_request(&r, "fast", None);
        assert_eq!(
            texts(&g),
            vec![
                (Role::User, "分類して".to_string()),
                (Role::Assistant, "{\"x\":1}".to_string()),
                (Role::User, "直してください".to_string()),
            ]
        );
    }

    /// 使えた呼び出しは step のキー（＋接尾辞）、使えなかった呼び出しは呼び出しごとに一意な #f キー。
    #[test]
    fn accounting_keys_follow_the_outcome() {
        let schema = json!({ "type": "object", "required": ["a"], "additionalProperties": false,
                             "properties": { "a": { "type": "string" } } });
        let s = LlmInvokeReq {
            output_schema: Some(schema),
            ..req()
        };
        let ok: Result<(), LlmError> = Ok(());
        let end = Some(StopReason::EndTurn);
        assert!(!unusable(&s, &ok, end, r#"{"a":"x"}"#));
        assert!(unusable(&s, &ok, end, r#"{"b":"x"}"#), "形に合わない");
        assert!(unusable(&s, &ok, Some(StopReason::Refusal), r#"{"a":"x"}"#));
        assert!(unusable(
            &s,
            &ok,
            Some(StopReason::MaxTokens),
            r#"{"a":"x"}"#
        ));
        let cut: Result<(), LlmError> = Err(LlmError::Unavailable("reset".into()));
        assert!(unusable(&s, &cut, None, ""));
        // 自由な本文は、上限・拒否でも従来どおり使えた呼び出し（途中で切れたときだけ使えない）。
        assert!(!unusable(&req(), &ok, Some(StopReason::MaxTokens), "途中"));
        assert!(unusable(&req(), &cut, None, ""));

        assert_eq!(accounting_key(&req(), false), "wf:t:r:ai");
        let repair = LlmInvokeReq {
            call_suffix: Some("#c1".into()),
            ..req()
        };
        assert_eq!(accounting_key(&repair, false), "wf:t:r:ai#c1");
        let a = accounting_key(&repair, true);
        let b = accounting_key(&repair, true);
        assert!(a.starts_with("wf:t:r:ai#f"), "{a}");
        assert_ne!(a, b, "失敗の会計は呼び出しごとに一意");
    }
}
