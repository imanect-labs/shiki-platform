//! Anthropic Messages API 直結アダプタ（中立 content-block → Anthropic ブロック）。
//!
//! PIT-9 の中立正規形は Claude の tool_use / thinking を一級市民として持つため、Anthropic への
//! 写しは素直（最小公倍数で削らない）。既定モデルは Claude（`claude-opus-4-8`）前提。`effort` は
//! `output_config.effort` へ、思考は adaptive thinking へ翻訳する（budget_tokens は使わない）。
//!
//! 本アダプタは実装するが検証経路は openai-compat（human 指示）。ここではメッセージ変換の
//! 単体テストのみ行い、実サーバ結線はコードレビュー範囲とする。

use std::collections::HashSet;
use std::time::Duration;

use futures::channel::mpsc;
use futures::stream::StreamExt;
use serde_json::{json, Value};

use crate::model::{Block, GenerateRequest, Message, Role, StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic 直結アダプタ。
pub struct AnthropicProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    default_model: String,
}

impl AnthropicProvider {
    pub fn new(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Self {
        AnthropicProvider {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            default_model: default_model.into(),
        }
    }

    fn build_body(&self, req: &GenerateRequest) -> Value {
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let known: HashSet<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| to_anthropic_message(m, &known))
            .collect();
        let mut body = json!({
            "model": model,
            "max_tokens": req.max_tokens.unwrap_or(4096),
            "messages": messages,
            "stream": true,
        });
        if let Some(sys) = &req.system {
            body["system"] = json!(sys);
        }
        if !req.tools.is_empty() {
            // 全ツールが遅延だと API は 400 を返す（最低 1 つは非遅延が要る）。その場合は
            // 遅延を無視して全部載せる（提示を欠くより文脈が膨らむ方が安全）。
            let defer = req.tools.iter().any(|t| !t.defer_loading);
            body["tools"] = json!(req
                .tools
                .iter()
                .map(|t| {
                    let mut v = json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    });
                    if defer && t.defer_loading {
                        v["defer_loading"] = json!(true);
                    }
                    v
                })
                .collect::<Vec<_>>());
        }
        if let Some(effort) = req.effort {
            // adaptive thinking + effort（Claude 4.6+）。budget_tokens は使わない。
            body["thinking"] = json!({ "type": "adaptive" });
            body["output_config"] = json!({ "effort": effort.as_str() });
        }
        body
    }
}

/// 中立メッセージ 1 件を Anthropic message へ写す（tool 結果は user ロールの tool_result ブロック）。
///
/// `known` はこのリクエストで提示するツール名（`tool_reference` の参照先が在るかの判定に使う）。
fn to_anthropic_message(m: &Message, known: &HashSet<&str>) -> Value {
    let blocks: Vec<Value> = m
        .content
        .iter()
        .filter_map(|b| to_anthropic_block(b, known))
        .collect();
    let role = match m.role {
        // system はトップレベル（build_body 側）へ回すため空にする。
        Role::System => return json!({ "role": "user", "content": [] }),
        Role::Assistant => "assistant",
        // user と tool 結果はどちらも user ロール（Anthropic は tool_result を user ブロックに置く）。
        Role::User | Role::Tool => "user",
    };
    json!({ "role": role, "content": blocks })
}

/// tool_result の content。読み込み参照があれば `tool_reference` ブロック列にする
/// （API がそれを遅延ツールの完全な定義へ展開する）。本文はその後ろに text ブロックで添える
/// （ループが観測に書き足す残り予算・催促を落とさない）。
///
/// 参照先が 1 つでも `tools` に無ければ（ツールを外した着地ターン等）、API は 400 を返す。
/// その場合は本文（テキスト）だけを送る。
fn tool_result_content(content: &str, references: &[String], known: &HashSet<&str>) -> Value {
    if references.is_empty() || !references.iter().all(|r| known.contains(r.as_str())) {
        return json!(content);
    }
    let mut blocks: Vec<Value> = references
        .iter()
        .map(|r| json!({ "type": "tool_reference", "tool_name": r }))
        .collect();
    if !content.is_empty() {
        blocks.push(json!({ "type": "text", "text": content }));
    }
    json!(blocks)
}

fn to_anthropic_block(b: &Block, known: &HashSet<&str>) -> Option<Value> {
    match b {
        Block::Text { text } => Some(json!({ "type": "text", "text": text })),
        Block::Thinking { .. } => None, // 再送する thinking はここでは扱わない（Phase 3 では省略）
        Block::ToolUse { id, name, input } => Some(json!({
            "type": "tool_use", "id": id, "name": name, "input": input,
        })),
        Block::ToolResult {
            tool_use_id,
            content,
            is_error,
            tool_references,
        } => Some(json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": tool_result_content(content, tool_references, known),
            "is_error": is_error,
        })),
    }
}

fn map_stop_reason(sr: Option<&str>) -> StopReason {
    match sr {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("end_turn" | "stop_sequence") => StopReason::EndTurn,
        _ => StopReason::Other,
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    #[allow(clippy::too_many_lines)] // SSE イベント種別ごとの分岐で行数が伸びる（分割は可読性を損なう）。
    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        let url = format!("{}/v1/messages", self.base_url);
        let body = self.build_body(req);
        let resp = self
            .http
            .post(&url)
            .timeout(Duration::from_mins(5))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Unavailable(format!("anthropic request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if status.is_client_error() {
                // 429 は「直せば通る」ものではない（openai 側と同じ理由・provider.rs 参照）。
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    return Err(LlmError::RateLimited(crate::provider::rate_limit_message(
                        &text,
                    )));
                }
                return Err(LlmError::BadRequest(format!("anthropic {status}: {text}")));
            }
            return Err(LlmError::Unavailable(format!("anthropic {status}: {text}")));
        }

        let (tx, rx) = mpsc::unbounded::<Result<StreamDelta, LlmError>>();
        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            // index → (id, name, accumulated json)
            let mut tools: std::collections::BTreeMap<i64, (String, String, String)> =
                std::collections::BTreeMap::new();
            let mut usage = Usage::default();
            let mut stop = StopReason::EndTurn;

            while let Some(chunk) = byte_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ =
                            tx.unbounded_send(Err(LlmError::Unavailable(format!("stream: {e}"))));
                        return;
                    }
                };
                buf.extend_from_slice(&chunk);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim();
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let Ok(v): Result<Value, _> = serde_json::from_str(data.trim()) else {
                        continue;
                    };
                    match v.get("type").and_then(Value::as_str) {
                        Some("message_start") => {
                            if let Some(u) = v.pointer("/message/usage") {
                                usage.prompt_tokens =
                                    u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                            }
                        }
                        Some("content_block_start") => {
                            let idx = v.get("index").and_then(Value::as_i64).unwrap_or(0);
                            if let Some(cb) = v.get("content_block") {
                                if cb.get("type").and_then(Value::as_str) == Some("tool_use") {
                                    let id = cb
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string();
                                    let name = cb
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string();
                                    tools.insert(idx, (id.clone(), name.clone(), String::new()));
                                    let _ = tx
                                        .unbounded_send(Ok(StreamDelta::ToolUseStart { id, name }));
                                }
                            }
                        }
                        Some("content_block_delta") => {
                            let idx = v.get("index").and_then(Value::as_i64).unwrap_or(0);
                            if let Some(delta) = v.get("delta") {
                                match delta.get("type").and_then(Value::as_str) {
                                    Some("text_delta") => {
                                        if let Some(t) = delta.get("text").and_then(Value::as_str) {
                                            let _ = tx.unbounded_send(Ok(StreamDelta::TextDelta {
                                                text: t.to_string(),
                                            }));
                                        }
                                    }
                                    Some("thinking_delta") => {
                                        if let Some(t) =
                                            delta.get("thinking").and_then(Value::as_str)
                                        {
                                            let _ =
                                                tx.unbounded_send(Ok(StreamDelta::ThinkingDelta {
                                                    text: t.to_string(),
                                                }));
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let Some(pj) =
                                            delta.get("partial_json").and_then(Value::as_str)
                                        {
                                            if let Some(e) = tools.get_mut(&idx) {
                                                e.2.push_str(pj);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Some("content_block_stop") => {
                            let idx = v.get("index").and_then(Value::as_i64).unwrap_or(0);
                            if let Some((id, _name, args)) = tools.remove(&idx) {
                                let input: Value =
                                    serde_json::from_str(args.trim()).unwrap_or(json!({}));
                                let _ =
                                    tx.unbounded_send(Ok(StreamDelta::ToolUseStop { id, input }));
                            }
                        }
                        Some("message_delta") => {
                            if let Some(sr) =
                                v.pointer("/delta/stop_reason").and_then(Value::as_str)
                            {
                                stop = map_stop_reason(Some(sr));
                            }
                            if let Some(ot) =
                                v.pointer("/usage/output_tokens").and_then(Value::as_u64)
                            {
                                usage.completion_tokens = ot;
                            }
                        }
                        Some("message_stop") => {
                            let _ = tx.unbounded_send(Ok(StreamDelta::Done {
                                stop_reason: stop,
                                usage,
                            }));
                            return;
                        }
                        _ => {}
                    }
                }
            }
            let _ = tx.unbounded_send(Ok(StreamDelta::Done {
                stop_reason: stop,
                usage,
            }));
        });

        Ok(rx.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolDef;

    #[test]
    fn tool_result_maps_to_user_tool_result_block() {
        let m = Message {
            role: Role::Tool,
            content: vec![Block::tool_result("t1", "r", false)],
        };
        let out = to_anthropic_message(&m, &HashSet::new());
        assert_eq!(out["role"], "user");
        assert_eq!(out["content"][0]["type"], "tool_result");
        assert_eq!(out["content"][0]["tool_use_id"], "t1");
    }

    #[test]
    fn effort_maps_to_output_config_and_adaptive_thinking() {
        let http = reqwest::Client::new();
        let p = AnthropicProvider::new(http, "https://api.anthropic.com", "k", "claude-opus-4-8");
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.effort = Some(crate::model::Effort::High);
        let body = p.build_body(&req);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["max_tokens"], 4096);
    }

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new(
            reqwest::Client::new(),
            "https://api.anthropic.com",
            "k",
            "claude-opus-4-8",
        )
    }

    fn deferred(name: &str) -> ToolDef {
        let mut d = ToolDef::new(name, "d", json!({"type": "object"}));
        d.defer_loading = true;
        d
    }

    fn loaded(names: &[&str]) -> Message {
        Message {
            role: Role::Tool,
            content: vec![Block::ToolResult {
                tool_use_id: "t1".into(),
                content: "読み込みました".into(),
                is_error: false,
                tool_references: names.iter().map(|s| (*s).to_string()).collect(),
            }],
        }
    }

    #[test]
    fn deferred_tools_carry_defer_loading_and_references_become_tool_reference_blocks() {
        let mut req = GenerateRequest::new(vec![
            Message::text(Role::User, "hi"),
            loaded(&["csv.query"]),
        ]);
        req.tools = vec![
            ToolDef::new("tool_search", "s", json!({"type": "object"})),
            deferred("csv.query"),
        ];
        let body = provider().build_body(&req);
        assert!(body["tools"][0].get("defer_loading").is_none());
        assert_eq!(body["tools"][1]["defer_loading"], true);
        let content = &body["messages"][1]["content"][0]["content"];
        assert_eq!(
            content,
            &json!([
                { "type": "tool_reference", "tool_name": "csv.query" },
                { "type": "text", "text": "読み込みました" },
            ])
        );
    }

    #[test]
    fn reference_to_tool_absent_from_request_falls_back_to_text() {
        // 着地ターン（tools 空）で参照を送ると API が 400 を返すため、本文へ落とす。
        let req = GenerateRequest::new(vec![
            Message::text(Role::User, "hi"),
            loaded(&["csv.query"]),
        ]);
        let body = provider().build_body(&req);
        assert_eq!(
            body["messages"][1]["content"][0]["content"],
            "読み込みました"
        );
    }

    #[test]
    fn all_deferred_request_drops_defer_flags() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.tools = vec![deferred("a"), deferred("b")];
        let body = provider().build_body(&req);
        assert!(body["tools"][0].get("defer_loading").is_none());
        assert!(body["tools"][1].get("defer_loading").is_none());
    }
}
