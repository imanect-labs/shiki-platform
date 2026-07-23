//! OpenAI 互換プロバイダ（`/chat/completions`・SSE ストリーミング）。第一級・検証経路。
//!
//! APIキーで動く openai-compat（vLLM / OpenAI / LiteLLM Proxy 等）を賄う。中立 [`Block`] を
//! OpenAI messages へ写し、SSE `data:` チャンクを中立 [`StreamDelta`] へ戻す。ツール引数の
//! 逐次ストリーミングは Phase 3 では不要のため、各ツール呼び出しは累積して完了時に
//! [`StreamDelta::ToolUseStop`]（完全な入力 JSON）を 1 回出す。

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use futures::channel::mpsc;
use futures::stream::StreamExt;
use serde_json::{json, Value};

use crate::model::{Block, GenerateRequest, Message, Role, StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};

/// OpenAI の function.name 制約（`^[a-zA-Z0-9_-]{1,64}$`）へ写した wire 名。
///
/// このリポジトリのツール名にはドット入り（`document.edit` 等）があり、寛容なプロバイダは
/// 通すが DeepSeek 等は 400 で拒否する。送信時に許容外文字を `_` へ写し 64 文字へ切り詰め、
/// 受信時は [`ToolNameMap`] で元の名前へ逆写しする。
fn sanitize_wire_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// ツール名の 双方向写像（ローカル名 ⇄ wire 名）。リクエストの tools から構築し、
/// サニタイズ衝突（`a.b` と `a_b` の併存等）は接尾辞で一意化して往復可能に保つ。
struct ToolNameMap {
    to_wire: HashMap<String, String>,
    to_local: HashMap<String, String>,
}

impl ToolNameMap {
    fn new(names: impl Iterator<Item = impl AsRef<str>>) -> Self {
        let mut to_wire = HashMap::new();
        let mut to_local: HashMap<String, String> = HashMap::new();
        for name in names {
            let local = name.as_ref().to_string();
            if to_wire.contains_key(&local) {
                continue;
            }
            let mut wire = sanitize_wire_name(&local);
            let mut n = 2;
            while to_local.contains_key(&wire) {
                wire = format!("{}_{n}", sanitize_wire_name(&local));
                n += 1;
            }
            to_local.insert(wire.clone(), local.clone());
            to_wire.insert(local, wire);
        }
        ToolNameMap { to_wire, to_local }
    }

    fn wire(&self, local: &str) -> String {
        self.to_wire
            .get(local)
            .cloned()
            .unwrap_or_else(|| sanitize_wire_name(local))
    }

    fn local(&self, wire: &str) -> String {
        self.to_local
            .get(wire)
            .cloned()
            .unwrap_or_else(|| wire.to_string())
    }
}

/// OpenAI 互換アダプタ。
pub struct OpenAiProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
}

impl OpenAiProvider {
    /// `base_url` は `/chat/completions` の親（例 `http://vllm:8000/v1`）。
    pub fn new(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: impl Into<String>,
    ) -> Self {
        OpenAiProvider {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            default_model: default_model.into(),
        }
    }

    fn build_body(&self, req: &GenerateRequest, names: &ToolNameMap) -> Value {
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let mut messages = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(json!({ "role": "system", "content": sys }));
        }
        for m in &req.messages {
            messages.extend(to_openai_messages(m, names));
        }
        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if let Some(mt) = req.max_tokens {
            body["max_tokens"] = json!(mt);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = json!(req
                .tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "function": {
                        "name": names.wire(&t.name),
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                }))
                .collect::<Vec<_>>());
        }
        body
    }
}

/// 中立メッセージ 1 件を OpenAI messages（複数になり得る）へ写す。
/// 履歴中のツール名も wire 名へ写す（厳格プロバイダは履歴の tool_calls 名も検証する）。
fn to_openai_messages(m: &Message, names: &ToolNameMap) -> Vec<Value> {
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
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content,
                })),
                _ => None,
            })
            .collect(),
        Role::System => vec![json!({
            "role": "system",
            "content": join_text(&m.content),
        })],
        Role::User => vec![json!({
            "role": "user",
            "content": join_text(&m.content),
        })],
        Role::Assistant => {
            let text = join_text(&m.content);
            let tool_calls: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, name, input } => Some(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": names.wire(name), "arguments": input.to_string() },
                    })),
                    _ => None,
                })
                .collect();
            let mut msg = json!({ "role": "assistant" });
            if !text.is_empty() {
                msg["content"] = json!(text);
            }
            if !tool_calls.is_empty() {
                msg["tool_calls"] = json!(tool_calls);
            }
            vec![msg]
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

/// 累積中のツール呼び出し（index → (id, name, arguments 文字列)）。
#[derive(Default)]
struct ToolAcc {
    calls: BTreeMap<i64, (String, String, String)>,
}

impl ToolAcc {
    fn ingest(&mut self, tool_calls: &Value) {
        let Some(arr) = tool_calls.as_array() else {
            return;
        };
        for tc in arr {
            let idx = tc.get("index").and_then(Value::as_i64).unwrap_or(0);
            let entry = self.calls.entry(idx).or_default();
            if let Some(id) = tc.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    entry.0 = id.to_string();
                }
            }
            if let Some(f) = tc.get("function") {
                if let Some(name) = f.get("name").and_then(Value::as_str) {
                    if !name.is_empty() {
                        entry.1 = name.to_string();
                    }
                }
                if let Some(args) = f.get("arguments").and_then(Value::as_str) {
                    entry.2.push_str(args);
                }
            }
        }
    }

    /// 完了イベント（ToolUseStop 列）へ落とす。
    fn drain_stops(&mut self) -> Vec<StreamDelta> {
        std::mem::take(&mut self.calls)
            .into_values()
            .filter(|(id, name, _)| !id.is_empty() && !name.is_empty())
            .map(|(id, name, args)| {
                let input: Value = serde_json::from_str(args.trim()).unwrap_or(json!({}));
                let _ = name; // name は id で参照済み（tool_use 側で保持）
                StreamDelta::ToolUseStop { id, input }
            })
            .collect()
    }

    fn starts(&self) -> Vec<StreamDelta> {
        self.calls
            .values()
            .filter(|(id, name, _)| !id.is_empty() && !name.is_empty())
            .map(|(id, name, _)| StreamDelta::ToolUseStart {
                id: id.clone(),
                name: name.clone(),
            })
            .collect()
    }
}

fn map_finish_reason(fr: Option<&str>) -> StopReason {
    match fr {
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("stop") => StopReason::EndTurn,
        _ => StopReason::Other,
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        let url = format!("{}/chat/completions", self.base_url);
        let names = ToolNameMap::new(req.tools.iter().map(|t| &t.name));
        let body = self.build_body(req, &names);
        let mut builder = self
            .http
            .post(&url)
            .timeout(Duration::from_mins(5))
            .json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| LlmError::Unavailable(format!("openai request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if status.is_client_error() {
                return Err(LlmError::BadRequest(format!("openai {status}: {text}")));
            }
            return Err(LlmError::Unavailable(format!("openai {status}: {text}")));
        }

        let (tx, rx) = mpsc::unbounded::<Result<StreamDelta, LlmError>>();
        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut acc = ToolAcc::default();
            let mut started: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut usage = Usage::default();
            let mut stop = StopReason::EndTurn;
            let mut finished = false;

            'outer: while let Some(chunk) = byte_stream.next().await {
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
                    let data = data.trim();
                    if data == "[DONE]" {
                        finished = true;
                        break 'outer;
                    }
                    let Ok(v): Result<Value, _> = serde_json::from_str(data) else {
                        continue;
                    };
                    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                        usage.prompt_tokens =
                            u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
                        usage.completion_tokens = u
                            .get("completion_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                    }
                    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
                        continue;
                    };
                    if let Some(delta) = choice.get("delta") {
                        if let Some(text) = delta.get("content").and_then(Value::as_str) {
                            if !text.is_empty() {
                                let _ = tx.unbounded_send(Ok(StreamDelta::TextDelta {
                                    text: text.to_string(),
                                }));
                            }
                        }
                        // vLLM / 一部モデルは reasoning_content で思考を返す。
                        if let Some(think) = delta.get("reasoning_content").and_then(Value::as_str)
                        {
                            if !think.is_empty() {
                                let _ = tx.unbounded_send(Ok(StreamDelta::ThinkingDelta {
                                    text: think.to_string(),
                                }));
                            }
                        }
                        if let Some(tc) = delta.get("tool_calls") {
                            acc.ingest(tc);
                            // 新規に name/id が確定したツールへ Start を 1 回出す。
                            // wire 名（サニタイズ済み）を元のローカル名へ逆写しして返す。
                            for s in acc.starts() {
                                if let StreamDelta::ToolUseStart { id, name } = s {
                                    if started.insert(id.clone()) {
                                        let _ = tx.unbounded_send(Ok(StreamDelta::ToolUseStart {
                                            id,
                                            name: names.local(&name),
                                        }));
                                    }
                                }
                            }
                        }
                    }
                    if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
                        stop = map_finish_reason(Some(fr));
                    }
                }
            }
            let _ = finished;
            for s in acc.drain_stops() {
                let _ = tx.unbounded_send(Ok(s));
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

    #[test]
    fn assistant_tool_use_maps_to_openai_tool_calls() {
        let m = Message {
            role: Role::Assistant,
            content: vec![Block::ToolUse {
                id: "t1".into(),
                name: "doc_search".into(),
                input: json!({"query": "x"}),
            }],
        };
        let names = ToolNameMap::new(["doc_search"].into_iter());
        let out = to_openai_messages(&m, &names);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["tool_calls"][0]["id"], "t1");
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "doc_search");
    }

    #[test]
    fn dotted_tool_names_are_sanitized_and_round_trip() {
        // ドット入り名は wire では `_` へ写り、履歴の tool_calls も同じ wire 名になる。
        let names = ToolNameMap::new(["document.edit", "csv.write", "fs_write"].into_iter());
        assert_eq!(names.wire("document.edit"), "document_edit");
        assert_eq!(names.wire("csv.write"), "csv_write");
        assert_eq!(names.wire("fs_write"), "fs_write");
        // 逆写しは wire → ローカルを厳密に復元する。
        assert_eq!(names.local("document_edit"), "document.edit");
        assert_eq!(names.local("csv_write"), "csv.write");
        // 履歴メッセージのツール名も wire 名で送られる。
        let m = Message {
            role: Role::Assistant,
            content: vec![Block::ToolUse {
                id: "t9".into(),
                name: "document.edit".into(),
                input: json!({"path": "a.md"}),
            }],
        };
        let out = to_openai_messages(&m, &names);
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "document_edit");
    }

    #[test]
    fn sanitize_collision_is_disambiguated() {
        // `a.b` と `a_b` が併存してもサニタイズ後に衝突せず往復可能。
        let names = ToolNameMap::new(["a.b", "a_b"].into_iter());
        let w1 = names.wire("a.b");
        let w2 = names.wire("a_b");
        assert_ne!(w1, w2);
        assert_eq!(names.local(&w1), "a.b");
        assert_eq!(names.local(&w2), "a_b");
    }

    #[test]
    fn tool_result_maps_to_tool_role() {
        let m = Message {
            role: Role::Tool,
            content: vec![Block::ToolResult {
                tool_use_id: "t1".into(),
                content: "result".into(),
                is_error: false,
            }],
        };
        let names = ToolNameMap::new(std::iter::empty::<&str>());
        let out = to_openai_messages(&m, &names);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["tool_call_id"], "t1");
    }

    #[test]
    fn tool_acc_accumulates_streamed_arguments() {
        let mut acc = ToolAcc::default();
        acc.ingest(
            &json!([{"index":0,"id":"t1","function":{"name":"doc_search","arguments":"{\"q"}}]),
        );
        acc.ingest(&json!([{"index":0,"function":{"arguments":"uery\":\"経費\"}"}}]));
        let stops = acc.drain_stops();
        assert_eq!(stops.len(), 1);
        match &stops[0] {
            StreamDelta::ToolUseStop { id, input } => {
                assert_eq!(id, "t1");
                assert_eq!(input["query"], "経費");
            }
            _ => panic!("expected ToolUseStop"),
        }
    }
}
