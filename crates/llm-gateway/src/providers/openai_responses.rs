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

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::channel::mpsc;
use futures::stream::StreamExt;
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{json, Value};

use super::openai::tool_message_content;
use super::openai_names::ToolNameMap;
use crate::model::{Block, GenerateRequest, Message, Role, StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};
use crate::tool_loading::context_tools;

/// OpenAI Responses API アダプタ。
pub struct OpenAiResponsesProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
    user_agent: Option<HeaderValue>,
    session_header: Option<HeaderName>,
}

/// Responses API の `max_output_tokens` の下限（これ未満は 400）。
const MIN_OUTPUT_TOKENS: u32 = 16;

/// SSE の 1 行・1 イベントの上限。`response.completed` は応答全文を含むので大きめに取る。
/// 改行・空行を送らない相手でメモリを使い尽くさないため（超えたらストリームをエラーで終える）。
const MAX_SSE_BYTES: usize = 8 * 1024 * 1024;

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

    /// 送出する User-Agent を指定する。ヘッダ値にできない文字列は設定エラーにする
    /// （起動時に気づけるように。リクエストごとに失敗させない）。
    pub fn with_user_agent(mut self, user_agent: Option<&str>) -> Result<Self, LlmError> {
        self.user_agent = user_agent
            .map(|ua| {
                HeaderValue::from_str(ua.trim())
                    .map_err(|e| LlmError::Config(format!("invalid llm.user_agent: {e}")))
            })
            .transpose()?;
        Ok(self)
    }

    /// 会話 ID を載せるヘッダ名を指定する。ヘッダ名にできない文字列は設定エラーにする。
    pub fn with_session_header(mut self, header: Option<&str>) -> Result<Self, LlmError> {
        self.session_header = header
            .map(|h| {
                HeaderName::from_bytes(h.trim().as_bytes())
                    .map_err(|e| LlmError::Config(format!("invalid llm.session_header: {e}")))
            })
            .transpose()?;
        Ok(self)
    }

    fn build_body(&self, req: &GenerateRequest, names: &ToolNameMap) -> Value {
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let known: HashSet<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
        let input: Vec<Value> = req
            .messages
            .iter()
            .flat_map(|m| to_input_items(m, names, &known))
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
            // Chat Completions では通る小さな上限も、Responses では下限未満だと 400 になる。
            body["max_output_tokens"] = json!(mt.max(MIN_OUTPUT_TOKENS));
        }
        // temperature は送らない。このアダプタの対象（Responses 専用の推論モデル）は受け付けず、
        // skill のモデル既定に温度があるだけで生成が 400 で失敗するため（中立 API では任意項目）。
        if let Some(effort) = req.effort {
            body["reasoning"] = json!({ "effort": effort.as_str(), "summary": "auto" });
        }
        if let Some(sid) = &req.session_id {
            body["prompt_cache_key"] = json!(sid);
        }
        // 遅延ツールは読み込まれた分だけ載せる（Chat Completions アダプタと同じ・tool_loading）。
        let tools = context_tools(&req.tools, &req.messages);
        if !tools.is_empty() {
            body["tools"] = json!(tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "name": names.wire(&t.name),
                    "description": t.description,
                    "parameters": t.input_schema,
                    // Responses は既定で strict（任意プロパティも必須に書き換える）。
                    // Chat Completions アダプタと同じく、スキーマどおりの意味で渡す。
                    "strict": false,
                }))
                .collect::<Vec<_>>());
        }
        body
    }
}

/// 中立メッセージ 1 件を Responses の入力アイテム（複数になり得る）へ写す。
/// 思考ブロックは送らない（`store: false` では前ターンの推論アイテムを参照できないため）。
/// `known` はこのリクエストで提示するツール名（読み込み参照の表示に使う）。
fn to_input_items(m: &Message, names: &ToolNameMap, known: &HashSet<&str>) -> Vec<Value> {
    match m.role {
        Role::Tool => m
            .content
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    tool_references,
                    ..
                } => Some(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": tool_message_content(content, tool_references, names, known),
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

/// SSE のイベント単位の組み立て（WHATWG: 空行で確定し、同じイベント内の `data:` 行は LF で連結する）。
#[derive(Default)]
struct SseFramer {
    data: Vec<String>,
    /// 確定前の data の合計バイト数。
    size: usize,
}

impl SseFramer {
    /// 1 行を受け取り、イベントが確定したらその data を返す。
    fn push_line(&mut self, line: &str) -> Option<String> {
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
    fn overflowed(&self) -> bool {
        self.size > MAX_SSE_BYTES
    }
}

/// 確定した 1 イベントを写して送る。完了・失敗まで来たら true。
fn dispatch(
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
            builder = builder.header(reqwest::header::USER_AGENT, ua.clone());
        }
        if let Some(header) = &self.session_header {
            // ID が無い（またはヘッダ値にできない）呼び出しには、呼び出しごとの ID を付ける。
            let sid = req
                .session_id
                .as_deref()
                .and_then(|s| HeaderValue::from_str(s).ok())
                .unwrap_or_else(|| {
                    HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
                        .unwrap_or_else(|_| HeaderValue::from_static("shiki"))
                });
            builder = builder.header(header.clone(), sid);
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
            let mut framer = SseFramer::default();
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
                    let payload = framer.push_line(&String::from_utf8_lossy(&line));
                    if framer.overflowed() {
                        break;
                    }
                    let Some(payload) = payload else {
                        continue;
                    };
                    if dispatch(&payload, &mut mapper, &names, &tx) {
                        return;
                    }
                }
                if buf.len() > MAX_SSE_BYTES || framer.overflowed() {
                    let _ = tx.unbounded_send(Err(LlmError::Unavailable(
                        "responses: SSE line or event exceeded the size limit".into(),
                    )));
                    return;
                }
            }
            // 末尾の空行が無いまま閉じた最後のイベントも捨てない。
            let tail = framer.push_line(&String::from_utf8_lossy(&buf));
            for payload in tail.into_iter().chain(framer.push_line("")) {
                if dispatch(&payload, &mut mapper, &names, &tx) {
                    return;
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
        req.tools = vec![ToolDef::new(
            "document.edit",
            "d",
            json!({"type": "object"}),
        )];
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
        assert_eq!(body["tools"][0]["strict"], false);
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn body_clamps_small_max_tokens_and_drops_temperature() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.max_tokens = Some(10);
        req.temperature = Some(0.3);
        let body = provider().build_body(&req, &ToolNameMap::new(std::iter::empty::<&str>()));
        assert_eq!(body["max_output_tokens"], 16);
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn invalid_identity_settings_are_config_errors() {
        assert!(matches!(
            provider().with_session_header(Some("x opencode session")),
            Err(LlmError::Config(_))
        ));
        assert!(matches!(
            provider().with_user_agent(Some("shiki/1.0\nx-injected: 1")),
            Err(LlmError::Config(_))
        ));
        // 前後の空白は落として受け付ける（.env の書き方の揺れ）。
        assert!(provider()
            .with_session_header(Some(" x-opencode-session "))
            .is_ok());
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
        let known = HashSet::from(["doc_search"]);
        let items = to_input_items(&assistant, &names, &known);
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
                tool_references: Vec::new(),
            }],
        };
        assert_eq!(
            to_input_items(&tool, &names, &known),
            vec![json!({"type": "function_call_output", "call_id": "call_1", "output": "結果"})]
        );
    }

    /// 遅延ツールは読み込まれた分だけ載り、読み込んだツール結果には呼び出し名の注記が付く
    /// （Chat Completions アダプタと同じ扱い・tool_loading）。
    #[test]
    fn deferred_tools_are_sent_only_after_they_are_loaded() {
        let mut csv = ToolDef::new("csv.query", "d", json!({"type": "object"}));
        csv.defer_loading = true;
        let mut office = ToolDef::new("office.edit", "d", json!({"type": "object"}));
        office.defer_loading = true;
        let search = ToolDef::new("tool_search", "d", json!({"type": "object"}));
        let names = ToolNameMap::new(["tool_search", "csv.query", "office.edit"].into_iter());

        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.tools = vec![search, csv, office];
        let body = provider().build_body(&req, &names);
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["tools"][0]["name"], "tool_search");

        req.messages.push(Message {
            role: Role::Tool,
            content: vec![Block::ToolResult {
                tool_use_id: "call_1".into(),
                content: "見つかりました".into(),
                is_error: false,
                tool_references: vec!["csv.query".into()],
            }],
        });
        let body = provider().build_body(&req, &names);
        let sent: Vec<&str> = body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(sent, ["tool_search", "csv_query"]);
        let output = body["input"][1]["output"].as_str().unwrap_or_default();
        assert!(output.starts_with("見つかりました"), "{output}");
        assert!(output.contains("csv_query"), "{output}");
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
        let out = feed_all(&[
            json!({"type": "response.failed", "response": {"error": {"message": "boom"}}}),
        ]);
        match &out[0] {
            Err(LlmError::Unavailable(m)) => assert!(m.contains("boom")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
