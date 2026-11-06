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
//! - 流し始めた後の `error`・`response.failed`、`response.completed`（か `response.incomplete`）の無いまま
//!   閉じたストリーム、大きさの上限を超えた行・イベントは、プロバイダが切った印（[`CUT_BY_PROVIDER`]）の付いた
//!   一時障害にする。送り直すかは gateway が決める（まだ何も確定していないときだけ・BUG-224）。

use std::collections::HashSet;

use futures::channel::mpsc;
use futures::stream::StreamExt;
use serde_json::{json, Value};

use super::identity::IdentityHeaders;
use super::openai::tool_message_content;
use super::openai_responses_events::{dispatch, EventMapper, SseFramer, MAX_SSE_BYTES};
use super::timeouts::RequestClock;
use super::tool_names::ToolNameMap;
use crate::config::LlmTimeouts;
use crate::model::{Block, GenerateRequest, Message, Role, StreamDelta};
use crate::provider::{DeltaStream, LlmError, LlmProvider, CUT_BY_PROVIDER};
use crate::tool_loading::context_tools;

/// OpenAI Responses API アダプタ。
pub struct OpenAiResponsesProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
    identity: IdentityHeaders,
    timeouts: LlmTimeouts,
    /// ChatGPT プラン（#854）向けの形で送るか（受け付けない項目を落とす）。
    chatgpt_plan: bool,
}

/// Responses API の `max_output_tokens` の下限（これ未満は 400）。
const MIN_OUTPUT_TOKENS: u32 = 16;

/// [`Block::Reasoning`] の `provider`（このアダプタが受け取り、送り返すものの印）。
pub(super) const REASONING_PROVIDER: &str = "openai_responses";

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
            identity: IdentityHeaders::default(),
            timeouts: LlmTimeouts::default(),
            chatgpt_plan: false,
        }
    }

    /// ChatGPT プラン（Sign in with ChatGPT）向けの形で送る。プランでは `max_output_tokens` を
    /// 受け付けず、`role: system` の入力アイテムを拒否するので、上限は送らず system は
    /// developer にする（トップレベルの system は従来どおり `instructions`）。
    #[must_use]
    pub(crate) fn for_chatgpt_plan(mut self) -> Self {
        self.chatgpt_plan = true;
        self
    }

    /// 送出する User-Agent を指定する。ヘッダ値にできない文字列は設定エラーにする
    /// （起動時に気づけるように。リクエストごとに失敗させない）。
    pub fn with_user_agent(mut self, user_agent: Option<&str>) -> Result<Self, LlmError> {
        self.identity.set_user_agent(user_agent)?;
        Ok(self)
    }

    /// 会話 ID を載せるヘッダ名を指定する。ヘッダ名にできない文字列は設定エラーにする。
    pub fn with_session_header(mut self, header: Option<&str>) -> Result<Self, LlmError> {
        self.identity.set_session_header(header)?;
        Ok(self)
    }

    /// タイムアウト（最初の応答・無通信・全体）を指定する。既定は [`LlmTimeouts::default`]。
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: LlmTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    fn build_body(&self, req: &GenerateRequest, names: &ToolNameMap) -> Value {
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let known: HashSet<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
        let mut input: Vec<Value> = req
            .messages
            .iter()
            .flat_map(|m| to_input_items(m, names, &known))
            .collect();
        if self.chatgpt_plan {
            for item in input.iter_mut().filter(|i| i["role"] == "system") {
                item["role"] = json!("developer");
            }
        }
        let mut body = json!({
            "model": model,
            "input": input,
            "stream": true,
            "store": false,
            // 会話をサーバに持たせないので、推論は暗号化した中身ごと受け取り、次のツールのターンで
            // 送り返す（ID だけでは参照できない・#542）。
            "include": ["reasoning.encrypted_content"],
        });
        if let Some(sys) = &req.system {
            body["instructions"] = json!(sys);
        }
        if let Some(mt) = req.max_tokens.filter(|_| !self.chatgpt_plan) {
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
        // 届いた順のまま戻す（推論アイテムは直後の本文・ツール呼び出しと対で受け付けられる）。
        Role::Assistant => m
            .content
            .iter()
            .filter_map(|b| match b {
                Block::Reasoning { provider, item } if provider == REASONING_PROVIDER => {
                    sanitized_reasoning(item)
                }
                Block::Text { text } if !text.is_empty() => {
                    Some(json!({ "role": "assistant", "content": text }))
                }
                Block::ToolUse { id, name, input } => Some(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": names.wire(name),
                    "arguments": input.to_string(),
                })),
                _ => None,
            })
            .collect(),
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

/// 履歴の推論アイテムを、決まった項目だけから組み直す。
///
/// 履歴はミニアプリの AI 呼び出し（`GenerateRequest` をそのまま受ける）からも来るので、`item` を
/// そのまま入力に入れると任意の入力アイテム（ファイル参照・外部 URL の画像など）を差し込めてしまう。
/// type は reasoning に固定し、id・暗号化された中身・要約のテキストだけを写す。形が違えば送らない。
fn sanitized_reasoning(item: &Value) -> Option<Value> {
    let encrypted = item
        .get("encrypted_content")?
        .as_str()
        .filter(|s| !s.is_empty())?;
    let id = item.get("id").and_then(Value::as_str);
    let summary: Vec<Value> = item
        .get("summary")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .map(|text| json!({ "type": "summary_text", "text": text }))
                .collect()
        })
        .unwrap_or_default();
    let mut out =
        json!({ "type": "reasoning", "encrypted_content": encrypted, "summary": summary });
    if let Some(id) = id {
        out["id"] = json!(id);
    }
    Some(out)
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiResponsesProvider {
    fn name(&self) -> &'static str {
        "openai_responses"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        self.stream_with_bearer(req, self.api_key.as_deref()).await
    }
}

impl OpenAiResponsesProvider {
    /// `bearer` を認証に付けて送る（ChatGPT プランは呼び出しごとにトークンが変わる）。
    pub(crate) async fn stream_with_bearer(
        &self,
        req: &GenerateRequest,
        bearer: Option<&str>,
    ) -> Result<DeltaStream, LlmError> {
        let url = format!("{}/responses", self.base_url);
        // 履歴中のツール名も同じ写像に通す（ToolNameMap::for_request）。
        let names = ToolNameMap::for_request(req);
        let body = self.build_body(req, &names);
        let mut builder = self.http.post(&url).json(&body);
        if let Some(key) = bearer {
            builder = builder.bearer_auth(key);
        }
        let builder = self.identity.apply(builder, req.session_id.as_deref());
        let clock = RequestClock::start(self.timeouts, "responses");
        let resp = clock.send(builder).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = clock.error_text(resp).await?;
            // 429 はプロバイダ側の枠（openai.rs と同じ扱い）。
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(LlmError::RateLimited(crate::provider::rate_limit_message(
                    &text,
                )));
            }
            if status.is_client_error() {
                return Err(crate::provider::rejection("responses", status, &text));
            }
            return Err(crate::provider::unavailable("responses", status, &text));
        }

        let (tx, rx) = mpsc::unbounded::<Result<StreamDelta, LlmError>>();
        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut framer = SseFramer::default();
            let mut mapper = EventMapper::default();
            while let Some(chunk) = clock.next_chunk(&mut byte_stream, &tx).await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.unbounded_send(Err(e));
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
                    // 改行・空行を送らない相手の応答は読み切れない。途中で切れた応答と同じ扱いにする（BUG-224）。
                    let _ = tx.unbounded_send(Err(cut(
                        "responses: SSE line or event exceeded the size limit",
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
            // 完了イベントが来ないまま切れた。書きかけの応答を完了扱いにせず、プロバイダが切った印を付ける
            // （まだ何も確定していなければ gateway が送り直す・BUG-224）。
            let _ = tx.unbounded_send(Err(cut(
                "responses: stream ended before response.completed",
            )));
        });

        Ok(rx.boxed())
    }
}

/// プロバイダが応答の途中で切った誤り（[`CUT_BY_PROVIDER`] の付いた [`LlmError::Unavailable`]）。
fn cut(why: &str) -> LlmError {
    LlmError::Unavailable(format!("{CUT_BY_PROVIDER}{why}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effort, ToolDef};
    use crate::providers::openai_responses_events::EventMapper;

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
    fn chatgpt_plan_body_drops_output_cap_and_sends_system_as_developer() {
        let mut req = GenerateRequest::new(vec![
            Message::text(Role::System, "rule"),
            Message::text(Role::User, "hi"),
        ]);
        req.system = Some("sys".into());
        req.max_tokens = Some(100);
        let body = provider()
            .for_chatgpt_plan()
            .build_body(&req, &ToolNameMap::new(std::iter::empty::<&str>()));
        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(body["instructions"], "sys");
        assert_eq!(
            (body["stream"].clone(), body["store"].clone()),
            (json!(true), json!(false))
        );
        assert_eq!(
            body["input"][0],
            json!({"role": "developer", "content": "rule"})
        );
        assert_eq!(body["input"][1]["role"], "user");
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

    #[test]
    fn reasoning_items_are_received_and_sent_back_before_tool_calls() {
        let names = ToolNameMap::new(["calc"].into_iter());
        let mut m = EventMapper::default();
        let out = m.feed(
            &json!({"type": "response.output_item.done", "item": {"id": "rs_1", "type": "reasoning", "status": "completed", "encrypted_content": "enc", "summary": [{"type": "summary_text", "text": "考え"}]}}),
            &names,
        );
        let Ok(StreamDelta::ReasoningItem { provider, item }) = &out[0] else {
            panic!("expected ReasoningItem, got {out:?}");
        };
        assert_eq!(provider, REASONING_PROVIDER);
        // status は送り返さない（入力に要らない）。
        assert_eq!(
            item,
            &json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "enc", "summary": [{"type": "summary_text", "text": "考え"}]})
        );
        // 暗号化された中身が無いものは捨てる。
        assert!(m
            .feed(&json!({"type": "response.output_item.done", "item": {"id": "rs_2", "type": "reasoning", "summary": []}}), &names)
            .is_empty());

        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                Block::Reasoning {
                    provider: REASONING_PROVIDER.into(),
                    item: item.clone(),
                },
                Block::Reasoning {
                    provider: "anthropic".into(),
                    item: json!({"sig": "x"}),
                },
                Block::Text {
                    text: "計算します".into(),
                },
                Block::ToolUse {
                    id: "call_1".into(),
                    name: "calc".into(),
                    input: json!({"expr": "1+1"}),
                },
            ],
        };
        let items = to_input_items(&assistant, &names, &HashSet::new());
        assert_eq!(items.len(), 3, "他プロバイダの推論は送らない: {items:?}");
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[1]["role"], "assistant");
        assert_eq!(items[2]["type"], "function_call");
    }

    #[test]
    fn history_reasoning_is_rebuilt_from_known_fields_in_arrival_order() {
        let names = ToolNameMap::new(["calc"].into_iter());
        let r = |id: &str| Block::Reasoning {
            provider: REASONING_PROVIDER.into(),
            item: json!({"type": "reasoning", "id": id, "encrypted_content": "enc", "summary": []}),
        };
        let call = |id: &str| Block::ToolUse {
            id: id.into(),
            name: "calc".into(),
            input: json!({}),
        };
        // 並列の呼び出しの間に推論が挟まる形も、そのままの順で戻す。
        let m = Message {
            role: Role::Assistant,
            content: vec![r("rs_1"), call("a"), r("rs_2"), call("b")],
        };
        let kinds: Vec<String> = to_input_items(&m, &names, &HashSet::new())
            .iter()
            .map(|i| {
                format!(
                    "{}:{}",
                    i["type"].as_str().unwrap_or("message"),
                    i["id"].as_str().or(i["call_id"].as_str()).unwrap_or("")
                )
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "reasoning:rs_1",
                "function_call:a",
                "reasoning:rs_2",
                "function_call:b"
            ]
        );

        // 推論アイテムを装った任意の入力アイテムは、決まった項目だけに組み直される（形が違えば送らない）。
        let injected = Message {
            role: Role::Assistant,
            content: vec![
                Block::Reasoning {
                    provider: REASONING_PROVIDER.into(),
                    item: json!({"type": "input_file", "role": "user", "file_id": "file-x", "encrypted_content": "enc", "summary": [{"type": "x", "text": "t", "image_url": "http://internal/"}]}),
                },
                Block::Reasoning {
                    provider: REASONING_PROVIDER.into(),
                    item: json!({"role": "user", "content": [{"type": "input_image", "image_url": "http://internal/"}]}),
                },
            ],
        };
        assert_eq!(
            to_input_items(&injected, &names, &HashSet::new()),
            vec![
                json!({"type": "reasoning", "encrypted_content": "enc", "summary": [{"type": "summary_text", "text": "t"}]})
            ]
        );
    }

    #[test]
    fn body_asks_for_encrypted_reasoning() {
        let req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        let body = provider().build_body(&req, &ToolNameMap::new(std::iter::empty::<&str>()));
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    /// 呼ばれるたびに用意した本文を順に返すモック（尽きたら最後の本文）。呼ばれた回数を数える。
    async fn scripted(
        bodies: Vec<&'static str>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&calls);
        let app = axum::Router::new().fallback(move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let body = bodies[n.min(bodies.len() - 1)];
            async move { ([("content-type", "text/event-stream")], body) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, calls)
    }

    /// gateway の送り直し（まだ何も確定していないときだけ）に通した結果（差分は短い名前、誤りは文言）。
    async fn through_resume(base: String) -> Vec<String> {
        let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(
            OpenAiResponsesProvider::new(reqwest::Client::new(), base, None, "m"),
        );
        let req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        let first = provider.stream(&req).await.unwrap();
        crate::retry::resume_unsettled(provider, req, first, crate::retry::MIDSTREAM_RETRIES)
            .map(|d| match d {
                Ok(StreamDelta::TextDelta { text }) => format!("text:{text}"),
                Ok(StreamDelta::ThinkingDelta { text }) => format!("think:{text}"),
                Ok(StreamDelta::ToolUseStart { id, .. }) => format!("start:{id}"),
                Ok(StreamDelta::Done { .. }) => "done".into(),
                Ok(other) => format!("{other:?}"),
                Err(e) => format!("err:{e}"),
            })
            .collect()
            .await
    }

    /// 思考だけを流して切れた応答（response.completed の無いまま閉じた・error・response.failed）は送り直し、本文・
    /// ツール呼び出しを出した後に切れた応答は送り直さずに誤りにする（BUG-224・二重に渡さない）。
    #[tokio::test]
    async fn cut_streams_are_sent_again_only_while_nothing_is_settled() {
        use std::sync::atomic::Ordering;
        const THINK: &str =
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"考え中\"}\n\n";
        const ERROR: &str =
            "data: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"boom\"}\n\n";
        const FAILED: &str = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_error\"}}}\n\n";
        const ANSWER: &str = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"答え\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{}}}\n\n",
        );
        const TEXT: &str = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"途中\"}\n\n";
        const TOOL_START: &str = "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"doc_search\"}}\n\n";
        let leak = |parts: &[&str]| -> &'static str { Box::leak(parts.concat().into_boxed_str()) };

        // 思考だけで閉じた・思考だけで error・response.failed → 送り直して答えを流す。
        for cut in [THINK, leak(&[THINK, ERROR]), leak(&[THINK, FAILED]), ERROR] {
            let (base, calls) = scripted(vec![cut, ANSWER]).await;
            let out = through_resume(base).await;
            assert_eq!(calls.load(Ordering::SeqCst), 2, "{cut}: {out:?}");
            assert_eq!(out.last().map(String::as_str), Some("done"), "{out:?}");
            assert!(out.contains(&"text:答え".to_string()), "{out:?}");
        }

        // 本文を出した後・書きかけのツール呼び出しで閉じた・本文の後の error → 送り直さずに誤り。
        for cut in [TEXT, TOOL_START, leak(&[TEXT, ERROR])] {
            let (base, calls) = scripted(vec![cut, ANSWER]).await;
            let out = through_resume(base).await;
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{cut}: {out:?}");
            let last = out.last().unwrap();
            assert!(
                last.starts_with("err:llm provider unavailable: cut by provider: responses: "),
                "{cut}: {out:?}"
            );
            assert!(!out.iter().any(|o| o == "done"), "{cut}: {out:?}");
        }
    }
}
