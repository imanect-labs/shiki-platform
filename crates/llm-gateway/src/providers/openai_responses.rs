//! OpenAI Responses API プロバイダ（`/responses`・SSE ストリーミング）。
//!
//! Chat Completions を話さないモデル（OpenCode Go の GPT 系など）のためのアダプタ。中立
//! [`Message`] を Responses の入力アイテム（message / function_call / function_call_output）へ写し、
//! SSE の型付きイベントを中立 [`StreamDelta`] へ戻す。
//!
//! - 会話の状態はサーバに持たせない（`store: false`・毎回全履歴を送る）。他のアダプタと同じく、
//!   履歴の正本は Shiki 側にある。
//! - ツール引数は openai-compat と同じく累積し、呼び出しの完了時に [`StreamDelta::ToolUseStop`]
//!   （完全な入力 JSON）を 1 回出す。
//! - 会話 ID（[`GenerateRequest::session_id`]）は、設定したヘッダ名で送り、`prompt_cache_key`
//!   にも使う。ヘッダ名を設定しているのに ID が無い呼び出しには、呼び出しごとの ID を付ける
//!   （ヘッダ必須のプロバイダで落ちないように）。

use std::collections::HashMap;
use std::time::Duration;

use futures::channel::mpsc;
use futures::stream::StreamExt;
use serde_json::{json, Value};

use super::openai_names::ToolNameMap;
use crate::model::{Block, GenerateRequest, Message, Role, StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};

/// OpenAI Responses API アダプタ。
pub struct OpenAiResponsesProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
    user_agent: Option<String>,
    session_header: Option<String>,
}

impl OpenAiResponsesProvider {
    /// `base_url` は `/responses` の親（例 `https://opencode.ai/inference/go/v1`）。
    pub fn new(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: impl Into<String>,
    ) -> Self {
        OpenAiResponsesProvider {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            default_model: default_model.into(),
            user_agent: None,
            session_header: None,
        }
    }

    /// 送出する User-Agent を指定する。
    #[must_use]
    pub fn with_user_agent(mut self, user_agent: Option<String>) -> Self {
        self.user_agent = user_agent;
        self
    }

    /// 会話 ID を載せるヘッダ名を指定する。
    #[must_use]
    pub fn with_session_header(mut self, header: Option<String>) -> Self {
        self.session_header = header;
        self
    }

    fn build_body(&self, req: &GenerateRequest, names: &ToolNameMap) -> Value {
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let input: Vec<Value> = req
            .messages
            .iter()
            .flat_map(|m| to_input_items(m, names))
            .collect();
        let mut body = json!({
            "model": model,
            "input": input,
            "stream": true,
            "store": false,
        });
        if let Some(sys) = &req.system {
            body["instructions"] = json!(sys);
        }
        if let Some(mt) = req.max_tokens {
            body["max_output_tokens"] = json!(mt);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(effort) = req.effort {
            body["reasoning"] = json!({ "effort": effort.as_str(), "summary": "auto" });
        }
        if let Some(sid) = &req.session_id {
            body["prompt_cache_key"] = json!(sid);
        }
        if !req.tools.is_empty() {
            body["tools"] = json!(req
                .tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "name": names.wire(&t.name),
                    "description": t.description,
                    "parameters": t.input_schema,
                }))
                .collect::<Vec<_>>());
        }
        body
    }
}

/// 中立メッセージ 1 件を Responses の入力アイテム（複数になり得る）へ写す。
/// 思考ブロックは送らない（`store: false` では前ターンの推論アイテムを参照できないため）。
fn to_input_items(m: &Message, names: &ToolNameMap) -> Vec<Value> {
    match m.role {
        Role::Tool => m
            .content
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => Some(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                })),
                _ => None,
            })
            .collect(),
        Role::System => vec![json!({ "role": "system", "content": join_text(&m.content) })],
        Role::User => vec![json!({ "role": "user", "content": join_text(&m.content) })],
        Role::Assistant => {
            let mut items = Vec::new();
            let text = join_text(&m.content);
            if !text.is_empty() {
                items.push(json!({ "role": "assistant", "content": text }));
            }
            for b in &m.content {
                if let Block::ToolUse { id, name, input } = b {
                    items.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": names.wire(name),
                        "arguments": input.to_string(),
                    }));
                }
            }
            items
        }
    }
}

fn join_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// SSE の型付きイベントを中立デルタへ写す状態機械（I/O から切り離して単体検証する）。
#[derive(Default)]
struct EventMapper {
    /// 出力アイテム ID → (call_id, wire 名, 累積した引数)。
    calls: HashMap<String, (String, String, String)>,
    saw_tool_call: bool,
    finished: bool,
}

impl EventMapper {
    /// 1 イベントを写す。`Done` を出したら以降は何も出さない。
    fn feed(&mut self, v: &Value, names: &ToolNameMap) -> Vec<Result<StreamDelta, LlmError>> {
        if self.finished {
            return Vec::new();
        }
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        let str_of = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
        match kind {
            "response.output_text.delta" => text_delta(str_of("delta"), false),
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
                let input: Value = serde_json::from_str(args.trim()).unwrap_or(json!({}));
                self.saw_tool_call = true;
                vec![Ok(StreamDelta::ToolUseStop { id: call_id, input })]
            }
            "response.completed" | "response.incomplete" => {
                self.finished = true;
                let resp = &v["response"];
                let usage = Usage {
                    prompt_tokens: resp["usage"]["input_tokens"].as_u64().unwrap_or(0),
                    completion_tokens: resp["usage"]["output_tokens"].as_u64().unwrap_or(0),
                };
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
                let msg = v["response"]["error"]["message"]
                    .as_str()
                    .or_else(|| v["error"]["message"].as_str())
                    .or_else(|| v["message"].as_str())
                    .unwrap_or("response failed");
                vec![Err(LlmError::Unavailable(format!("responses: {msg}")))]
            }
            _ => Vec::new(),
        }
    }
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

#[async_trait::async_trait]
impl LlmProvider for OpenAiResponsesProvider {
    fn name(&self) -> &'static str {
        "openai_responses"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        let url = format!("{}/responses", self.base_url);
        // 履歴中のツール名も同じ写像に通す（openai-compat と同じ理由・openai.rs 参照）。
        let names = ToolNameMap::new(req.tools.iter().map(|t| t.name.as_str()).chain(
            req.messages.iter().flat_map(|m| {
                m.content.iter().filter_map(|b| match b {
                    Block::ToolUse { name, .. } => Some(name.as_str()),
                    _ => None,
                })
            }),
        ));
        let body = self.build_body(req, &names);
        let mut builder = self
            .http
            .post(&url)
            .timeout(Duration::from_mins(5))
            .json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        if let Some(ua) = &self.user_agent {
            builder = builder.header(reqwest::header::USER_AGENT, ua);
        }
        if let Some(header) = &self.session_header {
            let sid = req
                .session_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            builder = builder.header(header.as_str(), sid);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| LlmError::Unavailable(format!("responses request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            // 429 はプロバイダ側の枠（openai.rs と同じ扱い）。
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(LlmError::RateLimited(crate::provider::rate_limit_message(
                    &text,
                )));
            }
            if status.is_client_error() {
                return Err(LlmError::BadRequest(format!("responses {status}: {text}")));
            }
            return Err(LlmError::Unavailable(format!("responses {status}: {text}")));
        }

        let (tx, rx) = mpsc::unbounded::<Result<StreamDelta, LlmError>>();
        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut mapper = EventMapper::default();
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
                    let Some(data) = line.trim().strip_prefix("data:") else {
                        continue;
                    };
                    let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
                        continue;
                    };
                    for out in mapper.feed(&v, &names) {
                        let _ = tx.unbounded_send(out);
                    }
                    if mapper.finished {
                        return;
                    }
                }
            }
            // 完了イベントが来ないまま切れた。書きかけの応答を完了扱いにしない。
            let _ = tx.unbounded_send(Err(LlmError::Unavailable(
                "responses: stream ended before response.completed".into(),
            )));
        });

        Ok(rx.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effort, ToolDef};

    fn provider() -> OpenAiResponsesProvider {
        OpenAiResponsesProvider::new(reqwest::Client::new(), "http://x/v1/", None, "gpt-x")
    }

    #[test]
    fn body_maps_system_effort_session_and_tools() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.system = Some("sys".into());
        req.effort = Some(Effort::Low);
        req.max_tokens = Some(100);
        req.session_id = Some("thread-1".into());
        req.tools = vec![ToolDef {
            name: "document.edit".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
        }];
        let names = ToolNameMap::new(["document.edit"].into_iter());
        let body = provider().build_body(&req, &names);
        assert_eq!(body["model"], "gpt-x");
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 100);
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["prompt_cache_key"], "thread-1");
        assert_eq!(body["input"][0], json!({"role": "user", "content": "hi"}));
        // Responses のツール定義は function の中身が平ら（name が直下）。
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "document_edit");
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn history_maps_tool_calls_and_results_to_items() {
        let names = ToolNameMap::new(["doc_search"].into_iter());
        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                Block::Thinking { text: "t".into() },
                Block::Text {
                    text: "調べます".into(),
                },
                Block::ToolUse {
                    id: "call_1".into(),
                    name: "doc_search".into(),
                    input: json!({"q": "経費"}),
                },
            ],
        };
        let items = to_input_items(&assistant, &names);
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0],
            json!({"role": "assistant", "content": "調べます"})
        );
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["call_id"], "call_1");
        assert_eq!(items[1]["arguments"], "{\"q\":\"経費\"}");

        let tool = Message {
            role: Role::Tool,
            content: vec![Block::ToolResult {
                tool_use_id: "call_1".into(),
                content: "結果".into(),
                is_error: false,
            }],
        };
        assert_eq!(
            to_input_items(&tool, &names),
            vec![json!({"type": "function_call_output", "call_id": "call_1", "output": "結果"})]
        );
    }

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
                    usage: Usage {
                        prompt_tokens: 9,
                        completion_tokens: 3
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
        let out = feed_all(&[
            json!({"type": "response.failed", "response": {"error": {"message": "boom"}}}),
        ]);
        match &out[0] {
            Err(LlmError::Unavailable(m)) => assert!(m.contains("boom")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
