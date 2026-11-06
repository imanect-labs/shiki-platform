//! OpenAI Responses API の SSE の読み取り（openai_responses.rs から分割）。
//!
//! SSE をイベント単位に組み立て（[`SseFramer`]）、型付きイベントを中立形の差分へ写す（[`EventMapper`]）。
//! I/O から切り離して単体で確かめる。
//!
//! 流し始めた後の `error`・`response.failed` は、[`CUT_BY_PROVIDER`] の付いた [`LlmError::Unavailable`] にする
//! （Anthropic の `event: error`・OpenAI 互換の素の誤りの JSON と同じ扱い・BUG-224）。送り直すかは gateway が決める
//! （まだ何も確定していないときだけ・`retry::resume_unsettled`）。

use std::collections::HashMap;

use futures::channel::mpsc;
use serde_json::{json, Value};

use super::openai_responses::REASONING_PROVIDER;
use super::tool_names::ToolNameMap;
use crate::model::{StopReason, StreamDelta};
use crate::provider::{LlmError, CUT_BY_PROVIDER};

/// SSE の 1 行・1 イベントの上限。`response.completed` は応答全文を含むので大きめに取る。
/// 改行・空行を送らない相手でメモリを使い尽くさないため（超えたらストリームをエラーで終える）。
pub(super) const MAX_SSE_BYTES: usize = 8 * 1024 * 1024;

/// SSE の型付きイベントを中立デルタへ写す状態機械（I/O から切り離して単体検証する）。
#[derive(Default)]
pub(super) struct EventMapper {
    /// 出力アイテム ID → (call_id, wire 名, 累積した引数)。
    calls: HashMap<String, (String, String, String)>,
    saw_tool_call: bool,
    finished: bool,
}

impl EventMapper {
    /// 1 イベントを写す。`Done` を出したら以降は何も出さない。
    pub(super) fn feed(
        &mut self,
        v: &Value,
        names: &ToolNameMap,
    ) -> Vec<Result<StreamDelta, LlmError>> {
        if self.finished {
            return Vec::new();
        }
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        let str_of = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
        match kind {
            // 拒否文（安全上の理由など）も本文として見せる（捨てると空の応答になる）。
            "response.output_text.delta" | "response.refusal.delta" => {
                text_delta(str_of("delta"), false)
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                text_delta(str_of("delta"), true)
            }
            "response.output_item.added" => {
                let item = &v["item"];
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return Vec::new();
                }
                let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let wire = item.get("name").and_then(Value::as_str).unwrap_or_default();
                if call_id.is_empty() || wire.is_empty() {
                    return Vec::new();
                }
                self.calls.insert(
                    item_id.to_string(),
                    (call_id.to_string(), wire.to_string(), String::new()),
                );
                vec![Ok(StreamDelta::ToolUseStart {
                    id: call_id.to_string(),
                    name: names.local(wire),
                })]
            }
            "response.function_call_arguments.delta" => {
                if let Some(call) = self.calls.get_mut(str_of("item_id")) {
                    call.2.push_str(str_of("delta"));
                }
                Vec::new()
            }
            "response.output_item.done" => {
                let item = &v["item"];
                if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                    return reasoning_item(item).into_iter().map(Ok).collect();
                }
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return Vec::new();
                }
                let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let Some((call_id, _, streamed)) = self.calls.remove(item_id) else {
                    return Vec::new();
                };
                // 完了アイテムの arguments が正本。無ければ差分を累積したものを使う。
                let args = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .filter(|a| !a.is_empty())
                    .unwrap_or(&streamed);
                // 壊れた引数を `{}` にしてツールを実行しない。生成のやり直し（gateway の再試行）に回す。
                let input: Value = match serde_json::from_str(args.trim()) {
                    Ok(v) => v,
                    Err(e) => {
                        self.finished = true;
                        return vec![Err(LlmError::Unavailable(format!(
                            "responses: invalid tool arguments: {e}"
                        )))];
                    }
                };
                self.saw_tool_call = true;
                vec![Ok(StreamDelta::ToolUseStop { id: call_id, input })]
            }
            "response.completed" | "response.incomplete" => {
                self.finished = true;
                let resp = &v["response"];
                let usage = super::usage::responses(&resp["usage"]);
                let stop_reason = if kind == "response.incomplete" {
                    match resp["incomplete_details"]["reason"].as_str() {
                        Some("max_output_tokens") => StopReason::MaxTokens,
                        _ => StopReason::Other,
                    }
                } else if self.saw_tool_call {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                };
                vec![Ok(StreamDelta::Done { stop_reason, usage })]
            }
            "response.failed" | "error" => {
                self.finished = true;
                // 誤りの本文（message には要求の ID などが入る）は文言に入れず、誤りの種類の語だけを載せる（BUG-210）。
                // 誤りの種類によらずプロバイダが切った印を付ける（送り直すかは gateway が決める・BUG-224）。
                let error = v
                    .pointer("/response/error")
                    .filter(|e| e.is_object())
                    .unwrap_or(v);
                let why = crate::provider::midstream_failure("responses", &error.to_string());
                vec![Err(LlmError::Unavailable(format!(
                    "{CUT_BY_PROVIDER}{why}"
                )))]
            }
            _ => Vec::new(),
        }
    }
}

/// 受け取った推論アイテムを、送り返せる形（type・id・encrypted_content・summary）にして返す。
/// 暗号化された中身が無いものは送り返しても参照できない（`store: false`）ので捨てる。
fn reasoning_item(item: &Value) -> Option<StreamDelta> {
    let encrypted = item.get("encrypted_content").and_then(Value::as_str)?;
    if encrypted.is_empty() {
        return None;
    }
    Some(StreamDelta::ReasoningItem {
        provider: REASONING_PROVIDER.to_string(),
        item: json!({
            "type": "reasoning",
            "id": item.get("id").cloned().unwrap_or(Value::Null),
            "encrypted_content": encrypted,
            "summary": item.get("summary").cloned().unwrap_or_else(|| json!([])),
        }),
    })
}

/// SSE のイベント単位の組み立て（WHATWG: 空行で確定し、同じイベント内の `data:` 行は LF で連結する）。
#[derive(Default)]
pub(super) struct SseFramer {
    data: Vec<String>,
    /// 確定前の data の合計バイト数。
    size: usize,
}

impl SseFramer {
    /// 1 行を受け取り、イベントが確定したらその data を返す。
    pub(super) fn push_line(&mut self, line: &str) -> Option<String> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            self.size = 0;
            return (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"));
        }
        if let Some(v) = line.strip_prefix("data:") {
            let v = v.strip_prefix(' ').unwrap_or(v);
            self.size += v.len() + 1;
            self.data.push(v.to_string());
        }
        None
    }

    /// 確定前のイベントが上限を超えたか。
    pub(super) fn overflowed(&self) -> bool {
        self.size > MAX_SSE_BYTES
    }
}

/// 確定した 1 イベントを写して送る。完了・失敗まで来たら true。
pub(super) fn dispatch(
    payload: &str,
    mapper: &mut EventMapper,
    names: &ToolNameMap,
    tx: &mpsc::UnboundedSender<Result<StreamDelta, LlmError>>,
) -> bool {
    if let Ok(v) = serde_json::from_str::<Value>(payload) {
        for out in mapper.feed(&v, names) {
            let _ = tx.unbounded_send(out);
        }
    }
    mapper.finished
}

fn text_delta(text: &str, thinking: bool) -> Vec<Result<StreamDelta, LlmError>> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = text.to_string();
    vec![Ok(if thinking {
        StreamDelta::ThinkingDelta { text }
    } else {
        StreamDelta::TextDelta { text }
    })]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(events: &[Value]) -> Vec<Result<StreamDelta, LlmError>> {
        let names = ToolNameMap::new(["document.edit"].into_iter());
        let mut m = EventMapper::default();
        events.iter().flat_map(|e| m.feed(e, &names)).collect()
    }

    #[test]
    fn mapper_streams_text_and_thinking_then_done() {
        let out = feed_all(&[
            json!({"type": "response.created"}),
            json!({"type": "response.reasoning_summary_text.delta", "delta": "考え"}),
            json!({"type": "response.output_text.delta", "delta": "東京は"}),
            json!({"type": "response.output_text.delta", "delta": ""}),
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 9, "output_tokens": 3}}}),
            json!({"type": "response.output_text.delta", "delta": "after"}),
        ]);
        let out: Vec<StreamDelta> = out.into_iter().map(Result::unwrap).collect();
        assert_eq!(
            out,
            vec![
                StreamDelta::ThinkingDelta {
                    text: "考え".into()
                },
                StreamDelta::TextDelta {
                    text: "東京は".into()
                },
                StreamDelta::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: crate::model::Usage {
                        prompt_tokens: 9,
                        completion_tokens: 3,
                        ..Default::default()
                    },
                },
            ]
        );
    }

    #[test]
    fn mapper_accumulates_function_call_and_maps_name_back() {
        let out = feed_all(&[
            json!({"type": "response.output_item.added", "item": {"id": "fc_1", "type": "function_call", "call_id": "call_9", "name": "document_edit", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"path\":"}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "\"a.md\"}"}),
            json!({"type": "response.output_item.done", "item": {"id": "fc_1", "type": "function_call", "call_id": "call_9", "name": "document_edit"}}),
            json!({"type": "response.completed", "response": {"usage": {}}}),
        ]);
        let out: Vec<StreamDelta> = out.into_iter().map(Result::unwrap).collect();
        assert_eq!(
            out[0],
            StreamDelta::ToolUseStart {
                id: "call_9".into(),
                name: "document.edit".into()
            }
        );
        assert_eq!(
            out[1],
            StreamDelta::ToolUseStop {
                id: "call_9".into(),
                input: json!({"path": "a.md"})
            }
        );
        assert!(matches!(
            out[2],
            StreamDelta::Done {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[test]
    fn framer_joins_multiline_data_and_dispatches_on_blank_line() {
        let mut f = SseFramer::default();
        assert_eq!(f.push_line("event: response.output_text.delta\n"), None);
        assert_eq!(f.push_line("data: {\"type\":\n"), None);
        assert_eq!(f.push_line("data: \"x\"}\r\n"), None);
        assert_eq!(f.push_line("\n").as_deref(), Some("{\"type\":\n\"x\"}"));
        assert_eq!(f.push_line("\n"), None);
    }

    #[test]
    fn framer_reports_overflow_of_an_unterminated_event() {
        let mut f = SseFramer::default();
        let chunk = format!("data: {}", "x".repeat(1024 * 1024));
        for _ in 0..8 {
            assert_eq!(f.push_line(&chunk), None);
        }
        assert!(f.overflowed());
        // 空行で確定すれば数え直す。
        assert!(f.push_line("").is_some());
        assert!(!f.overflowed());
    }

    #[test]
    fn mapper_maps_refusal_to_text_and_rejects_broken_tool_arguments() {
        let out =
            feed_all(&[json!({"type": "response.refusal.delta", "delta": "お答えできません"})]);
        assert!(
            matches!(&out[0], Ok(StreamDelta::TextDelta { text }) if text == "お答えできません")
        );
        let out = feed_all(&[
            json!({"type": "response.output_item.added", "item": {"id": "fc_1", "type": "function_call", "call_id": "c", "name": "document_edit"}}),
            json!({"type": "response.output_item.done", "item": {"id": "fc_1", "type": "function_call", "call_id": "c", "arguments": "{\"path\": "}}),
            json!({"type": "response.completed", "response": {"usage": {}}}),
        ]);
        assert!(
            matches!(out.last(), Some(Err(LlmError::Unavailable(m))) if m.contains("invalid tool arguments"))
        );
        assert!(!out
            .iter()
            .any(|d| matches!(d, Ok(StreamDelta::ToolUseStop { .. }))));
    }

    #[test]
    fn mapper_reports_incomplete_and_failed() {
        let out = feed_all(&[
            json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}, "usage": {"output_tokens": 5}}}),
        ]);
        assert!(matches!(
            out[0],
            Ok(StreamDelta::Done {
                stop_reason: StopReason::MaxTokens,
                ..
            })
        ));
        // 失敗の本文（message の要求の ID など）は文言に入れず、誤りの種類の語だけを載せる（BUG-210）。
        let out = feed_all(&[
            json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "boom req_abc123"}}}),
        ]);
        // どちらもプロバイダが切った印を付ける（まだ何も確定していなければ gateway が送り直す・BUG-224）。
        match &out[0] {
            Err(LlmError::Unavailable(m)) => assert_eq!(
                m,
                &format!(
                    "{CUT_BY_PROVIDER}responses: プロバイダが応答の途中で失敗しました（server_error）"
                )
            ),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        let out = feed_all(&[
            json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down org-xyz"}),
        ]);
        match &out[0] {
            Err(LlmError::Unavailable(m)) => {
                assert!(!m.contains("org-xyz") && !m.contains("slow down"), "{m}");
                assert!(m.contains("rate_limit_exceeded"), "{m}");
                assert!(m.starts_with(CUT_BY_PROVIDER), "{m}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
