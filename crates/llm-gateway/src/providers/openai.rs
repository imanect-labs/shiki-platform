//! OpenAI 互換プロバイダ（`/chat/completions`・SSE ストリーミング）。第一級・検証経路。
//!
//! APIキーで動く openai-compat（vLLM / OpenAI / LiteLLM Proxy 等）を賄う。中立 [`Block`] を
//! OpenAI messages へ写し、SSE `data:` チャンクを中立 [`StreamDelta`] へ戻す。ツール引数の
//! 逐次ストリーミングは Phase 3 では不要のため、各ツール呼び出しは累積して完了時に
//! [`StreamDelta::ToolUseStop`]（完全な入力 JSON）を 1 回出す。

use std::collections::{BTreeMap, HashMap, HashSet};

use futures::channel::mpsc;
use futures::stream::StreamExt;
use serde_json::{json, Value};

use super::identity::IdentityHeaders;
use super::think_tags::{split_content, Piece, ThinkSplitter};
use super::timeouts::RequestClock;
use super::tool_names::ToolNameMap;
use crate::config::LlmTimeouts;
use crate::model::{Block, GenerateRequest, Message, Role, StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};
use crate::tool_loading::{context_tools, with_loaded_note};

/// OpenAI 互換アダプタ。
pub struct OpenAiProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
    identity: IdentityHeaders,
    timeouts: LlmTimeouts,
    /// 本文の先頭に `<think>…</think>` で思考を返すモデル（実 ID・#584）。
    think_tag_models: HashSet<String>,
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
            identity: IdentityHeaders::default(),
            timeouts: LlmTimeouts::default(),
            think_tag_models: HashSet::new(),
        }
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

    /// 本文の先頭に `<think>…</think>` で思考を返すモデル（実 ID）を指定する。
    /// ここに無いモデルの本文はタグを含めてそのまま本文として流す。
    #[must_use]
    pub fn with_think_tag_models(mut self, models: impl IntoIterator<Item = String>) -> Self {
        self.think_tag_models = models.into_iter().collect();
        self
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
        let known: HashSet<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
        for m in &req.messages {
            messages.extend(to_openai_messages(m, names, &known));
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
        // 思考強度（OpenAI の reasoning 系）。対応しないモデル（vLLM の多く）には gateway が
        // 載せない（カタログの supports_effort で落とす）ので、ここに来たら素直に写す。
        if let Some(effort) = req.effort {
            body["reasoning_effort"] = json!(effort.as_str());
        }
        // 遅延ツールは読み込まれた分だけ載せる（OpenAI 互換に参照の展開は無い・tool_loading）。
        let tools = context_tools(&req.tools, &req.messages);
        if !tools.is_empty() {
            body["tools"] = json!(tools
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
/// `known` はこのリクエストで提示するツール名（読み込み参照の表示に使う）。
fn to_openai_messages(m: &Message, names: &ToolNameMap, known: &HashSet<&str>) -> Vec<Value> {
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
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": tool_message_content(content, tool_references, names, known),
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
            let extras = extra_contents(&m.content);
            let tool_calls: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, name, input } => {
                        let mut call = json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": names.wire(name), "arguments": input.to_string() },
                        });
                        if let Some(extra) = extras.get(id.as_str()) {
                            call["extra_content"] = (*extra).clone();
                        }
                        Some(call)
                    }
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

/// tool メッセージの本文。読み込み参照があれば、追加されたツールの **wire 名**を本文に
/// 書き足す（定義そのものは `tools` に足される・[`context_tools`]。本文の名前は中立名の
/// ままなので、呼び出しに使う名前をここで示す）。参照先が 1 つも提示に無ければ本文のまま。
pub(super) fn tool_message_content(
    content: &str,
    references: &[String],
    names: &ToolNameMap,
    known: &HashSet<&str>,
) -> String {
    let loaded: Vec<String> = references
        .iter()
        .filter(|r| known.contains(r.as_str()))
        .map(|r| names.wire(r))
        .collect();
    with_loaded_note(content, &loaded)
}

/// 履歴のツール呼び出し ID → 受け取ったときの `extra_content`。
fn extra_contents(blocks: &[Block]) -> HashMap<&str, &Value> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Reasoning { provider, item } if provider == EXTRA_CONTENT_PROVIDER => Some((
                item.get("tool_call_id")?.as_str()?,
                item.get("extra_content")?,
            )),
            _ => None,
        })
        .collect()
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
    /// 枠（index）→ プロバイダ固有の `extra_content`（届いたものだけ。ID は取り出すときに引く）。
    extras: BTreeMap<i64, Value>,
}

impl ToolAcc {
    fn ingest(&mut self, tool_calls: &Value) {
        let Some(arr) = tool_calls.as_array() else {
            return;
        };
        for tc in arr {
            let id = tc
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty());
            let idx = self.index_of(tc, id);
            if let Some(extra) = tc.get("extra_content") {
                self.extras.insert(idx, extra.clone());
            }
            let entry = self.calls.entry(idx).or_default();
            if let Some(id) = id {
                entry.0 = id.to_string();
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

    /// 差分の行き先。`index` が無い（Gemini API は各呼び出しを 1 チャンクで丸ごと送り、
    /// `index` を付けない）ときは、新しい ID なら新しい枠、ID が無ければ直前の枠にする。
    /// 全部 0 に寄せると、並行の呼び出しが 1 つに混ざる。直前の枠の ID がまだ空なら
    /// （ID が名前より後に届いた）、新しい ID もその枠のもの。ID も無く名前が来たら、
    /// 名前の決まった枠の続きではなく次の呼び出しの始まり。
    fn index_of(&self, tc: &Value, id: Option<&str>) -> i64 {
        if let Some(idx) = tc.get("index").and_then(Value::as_i64) {
            return idx;
        }
        let last = self.calls.iter().next_back();
        let Some(id) = id else {
            let names_call = tc
                .pointer("/function/name")
                .and_then(Value::as_str)
                .is_some_and(|n| !n.is_empty());
            return match last {
                Some((idx, (_, name, _))) if names_call && !name.is_empty() => idx + 1,
                Some((idx, _)) => *idx,
                None => 0,
            };
        };
        if let Some((idx, _)) = self.calls.iter().find(|(_, (known, _, _))| known == id) {
            return *idx;
        }
        match last {
            Some((idx, (known, _, _))) if known.is_empty() => *idx,
            Some((idx, _)) => idx + 1,
            None => 0,
        }
    }

    /// 呼び出しに付いてきた `extra_content` を、送り返すための推論アイテムにする。
    fn drain_extras(&mut self) -> Vec<StreamDelta> {
        std::mem::take(&mut self.extras)
            .into_iter()
            .filter_map(|(idx, extra)| {
                let id = self.calls.get(&idx).map(|(id, _, _)| id.as_str())?;
                (!id.is_empty()).then(|| StreamDelta::ReasoningItem {
                    provider: EXTRA_CONTENT_PROVIDER.to_string(),
                    item: json!({ "tool_call_id": id, "extra_content": extra }),
                })
            })
            .collect()
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

/// 本文から切り出した断片を中立デルタへ写す。
fn piece_delta(piece: Piece) -> StreamDelta {
    match piece {
        Piece::Text(text) => StreamDelta::TextDelta { text },
        Piece::Thinking(text) => StreamDelta::ThinkingDelta { text },
    }
}

/// [`Block::Reasoning`] の `provider`。ツール呼び出しに付いてきたプロバイダ固有の
/// `extra_content`（Gemini API の `thought_signature` 等）を、次のターンで同じ呼び出しへ
/// 付けて送り返すためだけに持つ。
const EXTRA_CONTENT_PROVIDER: &str = "openai_chat.extra_content";

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
        let names = ToolNameMap::for_request(req);
        let body = self.build_body(req, &names);
        let think_tags = body["model"]
            .as_str()
            .is_some_and(|m| self.think_tag_models.contains(m));
        let mut builder = self.http.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        let builder = self.identity.apply(builder, req.session_id.as_deref());
        let clock = RequestClock::start(self.timeouts, "openai");
        let resp = send_waiting_out_rate_limit(&clock, builder).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = clock.error_text(resp).await?;
            // 429 は 4xx だが**こちらの組み立てが悪いわけではない**（プロバイダ側の枠）。
            // BadRequest に落とすと「直せば通る」と誤解され、リトライ方針も表示も間違える。
            // `LlmError::Unavailable` の doc どおり一時障害として扱う。
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(LlmError::RateLimited(crate::provider::rate_limit_message(
                    &text,
                )));
            }
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
            // 最後に受け取った finish_reason が "stop" か（終端が欠けたストリームは昇格させない）。
            let mut said_stop = false;
            // 終わりの合図（`[DONE]` か finish_reason）を受け取ったか・SSE でない行（[`cut_short`]）。
            let (mut completed, mut stray) = (false, String::new());
            // 本文先頭の `<think>…</think>` は思考として流す（対象モデルだけ・#584）。
            let mut think_tags = think_tags.then(ThinkSplitter::default);

            'outer: while let Some(chunk) = clock.next_chunk(&mut byte_stream, &tx).await {
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
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim();
                    let Some(data) = line.strip_prefix("data:") else {
                        keep_stray(&mut stray, line);
                        continue;
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        completed = true;
                        break 'outer;
                    }
                    let Ok(v): Result<Value, _> = serde_json::from_str(data) else {
                        continue;
                    };
                    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                        usage = super::usage::chat_completions(u);
                    }
                    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
                        continue;
                    };
                    if let Some(delta) = choice.get("delta") {
                        if let Some(text) = delta.get("content").and_then(Value::as_str) {
                            for piece in split_content(think_tags.as_mut(), text) {
                                let _ = tx.unbounded_send(Ok(piece_delta(piece)));
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
                        said_stop = fr == "stop";
                        completed = true;
                    }
                }
            }
            if let Some(e) = cut_short(completed, &stray) {
                let _ = tx.unbounded_send(Err(e));
                return;
            }
            for piece in think_tags
                .as_mut()
                .map(ThinkSplitter::finish)
                .unwrap_or_default()
            {
                let _ = tx.unbounded_send(Ok(piece_delta(piece)));
            }
            for s in acc.drain_extras() {
                let _ = tx.unbounded_send(Ok(s));
            }
            let stops = acc.drain_stops();
            // Gemini API はツールを呼んでも `finish_reason: "stop"` を返す。呼び出しがあるのに
            // EndTurn で返すと、エージェントがツールを実行せずに終わる。finish_reason が
            // 届かずに切れたストリーム（引数が途中かもしれない）は昇格させない。
            if !stops.is_empty() && said_stop {
                stop = StopReason::ToolUse;
            }
            for s in stops {
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

/// 429 の指示どおりに待つ上限（これより長い待ちは、利用者へ「上限に達した」と返す）。
const MAX_RATE_LIMIT_WAIT: std::time::Duration = std::time::Duration::from_mins(1);

/// 429 が短い待ち（`Retry-After`・Gemini API の `RetryInfo.retryDelay`）を示すときは、その分
/// だけ待って 1 度だけ送り直す（#897）。Gemini API の無料枠は 1 分あたりの入力トークンが小さく
/// （Gemma は 16,000）、数回の生成で「32 秒待て」と返る。すぐ失敗にすると会話全体のやり直しに
/// なる。待っても通らなければ、いつもどおり上限の誤りとして返す（呼び出し側の対応表で扱う）。
async fn send_waiting_out_rate_limit(
    clock: &RequestClock,
    builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response, LlmError> {
    let again = builder.try_clone();
    let resp = clock.send(builder).await?;
    let Some(again) = again.filter(|_| resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS)
    else {
        return Ok(resp);
    };
    let header = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_secs);
    let text = clock.error_text(resp).await?;
    match header
        .or_else(|| retry_delay(&text))
        // 生成全体の締め切りを越える待ちはしない（暴走止めの上限を待ちで延ばさない）。
        .filter(|d| *d <= MAX_RATE_LIMIT_WAIT && *d < clock.remaining())
    {
        Some(wait) => {
            tracing::warn!(?wait, "プロバイダが 429 で待ちを指示した（待って送り直す）");
            tokio::time::sleep(wait).await;
            clock.send(again).await
        }
        None => Err(LlmError::RateLimited(crate::provider::rate_limit_message(
            &text,
        ))),
    }
}

/// Gemini API の 429 の本文にある `"retryDelay": "32s"` を読む。
fn retry_delay(body: &str) -> Option<std::time::Duration> {
    let rest = &body[body.find("\"retryDelay\"")? + "\"retryDelay\"".len()..];
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    let secs: f64 = rest[start..end].strip_suffix('s')?.parse().ok()?;
    // 巨大・負・NaN は読めないものとして扱う（from_secs_f64 は panic する）。
    std::time::Duration::try_from_secs_f64(secs).ok()
}

/// SSE でない行を理由の表示用に残す（Gemini API は流し始めた後に素の `[{"error": …}]` で切る）。
fn keep_stray(stray: &mut String, line: &str) {
    if !line.is_empty() && !line.starts_with(':') && stray.len() < 2048 {
        stray.push_str(line);
    }
}

/// 終わりの合図（`[DONE]` も finish_reason も）が無いまま切れたストリームの誤り。
///
/// 正常な終わりにしない。思考の途中で切れた応答を「本文もツール呼び出しも無い回答」として畳むと、
/// エージェントは何もせずに終わる（Gemini API の Gemma で実測・#897）。一時障害として返し、まだ
/// 何も確定していなければ gateway が送り直す。`stray` は SSE でない行（素のエラー JSON 等）。
fn cut_short(completed: bool, stray: &str) -> Option<LlmError> {
    if completed {
        return None;
    }
    let why = if stray.is_empty() {
        "stream ended before completion".to_string()
    } else {
        format!("stream ended with: {stray}")
    };
    Some(LlmError::Unavailable(format!(
        "{}openai {why}",
        crate::provider::CUT_BY_PROVIDER
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_read_from_gemini_rate_limit_bodies() {
        let body = r#"[{"error":{"code":429,"details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay": "32s"}]}}]"#;
        assert_eq!(retry_delay(body), Some(std::time::Duration::from_secs(32)));
        assert_eq!(
            retry_delay(r#"{"retryDelay":"1.5s"}"#),
            Some(std::time::Duration::from_millis(1500))
        );
        assert_eq!(retry_delay(r#"{"error":"quota"}"#), None);
        assert_eq!(retry_delay(r#"{"retryDelay":"soon"}"#), None);
        assert_eq!(retry_delay(r#"{"retryDelay":"1e20s"}"#), None);
        assert_eq!(retry_delay(r#"{"retryDelay":"-1s"}"#), None);
    }

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
        let out = to_openai_messages(&m, &names, &HashSet::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["tool_calls"][0]["id"], "t1");
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "doc_search");
    }

    // 写像そのもの（サニタイズ・衝突回避・64 文字制約）の単体テストは
    // `tool_names.rs` 側にある。ここでは messages への適用のみ検証する。
    #[test]
    fn dotted_tool_names_map_in_history_tool_calls() {
        // 履歴メッセージのツール名も wire 名（ドット→`_`）で送られる。
        let names = ToolNameMap::new(["document.edit"].into_iter());
        let m = Message {
            role: Role::Assistant,
            content: vec![Block::ToolUse {
                id: "t9".into(),
                name: "document.edit".into(),
                input: json!({"path": "a.md"}),
            }],
        };
        let out = to_openai_messages(&m, &names, &HashSet::new());
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "document_edit");
    }

    #[test]
    fn extra_content_goes_back_on_the_same_tool_call() {
        // Gemini API は思考の署名（thought_signature）を同じ呼び出しへ付けて返すことを求める。
        let m = Message {
            role: Role::Assistant,
            content: vec![
                Block::Reasoning {
                    provider: EXTRA_CONTENT_PROVIDER.into(),
                    item: json!({"tool_call_id": "t2", "extra_content": {"google": {"thought_signature": "s"}}}),
                },
                Block::ToolUse {
                    id: "t1".into(),
                    name: "a".into(),
                    input: json!({}),
                },
                Block::ToolUse {
                    id: "t2".into(),
                    name: "a".into(),
                    input: json!({}),
                },
                // 他のアダプタの推論アイテムは送らない。
                Block::Reasoning {
                    provider: "openai_responses".into(),
                    item: json!({"tool_call_id": "t1", "extra_content": {"x": 1}}),
                },
            ],
        };
        let names = ToolNameMap::new(["a"].into_iter());
        let out = to_openai_messages(&m, &names, &HashSet::new());
        let calls = out[0]["tool_calls"].as_array().unwrap();
        assert!(calls[0].get("extra_content").is_none());
        assert_eq!(
            calls[1]["extra_content"]["google"]["thought_signature"],
            "s"
        );
    }

    #[test]
    fn tool_result_maps_to_tool_role() {
        let m = Message {
            role: Role::Tool,
            content: vec![Block::tool_result("t1", "result", false)],
        };
        let names = ToolNameMap::new(std::iter::empty::<&str>());
        let out = to_openai_messages(&m, &names, &HashSet::new());
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

    #[test]
    fn tool_acc_without_index_keeps_calls_apart_and_follows_late_ids() {
        // Gemini API の形: index 無し・1 チャンクに丸ごと。
        let mut acc = ToolAcc::default();
        acc.ingest(&json!([
            {"id":"c1","function":{"name":"a","arguments":"{\"x\":1}"},"extra_content":{"k":1}},
            {"id":"c2","function":{"name":"a","arguments":"{\"x\":2}"}}
        ]));
        // ID が最初のチャンクにだけ来て、引数が続きのチャンクに分かれる。
        acc.ingest(&json!([{"id":"c3","function":{"name":"b","arguments":"{\"y\""}}]));
        acc.ingest(&json!([{"function":{"arguments":":3}"},"extra_content":{"k":3}}]));
        // ID が名前より後に届く。
        acc.ingest(&json!([{"function":{"name":"c","arguments":""}}]));
        acc.ingest(&json!([{"id":"c4","function":{"arguments":"{}"}}]));
        let extras: Vec<(String, Value)> = acc
            .drain_extras()
            .into_iter()
            .map(|d| match d {
                StreamDelta::ReasoningItem { item, .. } => (
                    item["tool_call_id"].as_str().unwrap().to_string(),
                    item["extra_content"].clone(),
                ),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            extras,
            [("c1".into(), json!({"k":1})), ("c3".into(), json!({"k":3}))]
        );
        let stops: Vec<(String, Value)> = acc
            .drain_stops()
            .into_iter()
            .map(|d| match d {
                StreamDelta::ToolUseStop { id, input } => (id, input),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            stops,
            [
                ("c1".into(), json!({"x":1})),
                ("c2".into(), json!({"x":2})),
                ("c3".into(), json!({"y":3})),
                ("c4".into(), json!({})),
            ]
        );
    }

    #[test]
    fn effort_maps_to_reasoning_effort_only_when_set() {
        let p = OpenAiProvider::new(reqwest::Client::new(), "http://x/v1", None, "m");
        let names = ToolNameMap::new(std::iter::empty::<&str>());
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        assert!(p.build_body(&req, &names).get("reasoning_effort").is_none());
        req.effort = Some(crate::model::Effort::Low);
        assert_eq!(p.build_body(&req, &names)["reasoning_effort"], "low");
    }

    #[test]
    fn deferred_tools_are_sent_only_after_being_loaded_and_named_by_wire_name() {
        use crate::model::ToolDef;
        let p = OpenAiProvider::new(reqwest::Client::new(), "http://x/v1", None, "m");
        let mut csv = ToolDef::new("csv.query", "q", json!({"type": "object"}));
        csv.defer_loading = true;
        let mut office = ToolDef::new("office.edit", "e", json!({"type": "object"}));
        office.defer_loading = true;
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.tools = vec![
            ToolDef::new("tool_search", "s", json!({"type": "object"})),
            csv,
            office,
        ];
        let names = ToolNameMap::new(req.tools.iter().map(|t| t.name.as_str()));
        let wire = |b: &Value| -> Vec<String> {
            b["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(wire(&p.build_body(&req, &names)), ["tool_search"]);

        req.messages.push(Message {
            role: Role::Tool,
            content: vec![Block::ToolResult {
                tool_use_id: "t1".into(),
                content: "fallback".into(),
                is_error: false,
                tool_references: vec!["csv.query".into()],
            }],
        });
        let body = p.build_body(&req, &names);
        assert_eq!(wire(&body), ["tool_search", "csv_query"]);
        let shown = body["messages"][1]["content"].as_str().unwrap();
        assert!(shown.contains("csv_query"), "{shown}");
        // 本文（ループが書き足した観測を含む）は落とさない。
        assert!(shown.starts_with("fallback"), "{shown}");
    }
}
