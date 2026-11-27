//! Anthropic Messages API 直結アダプタ（中立 content-block → Anthropic ブロック）。
//!
//! PIT-9 の中立正規形は Claude の tool_use / thinking を一級市民として持つため、Anthropic への
//! 写しは素直（最小公倍数で削らない）。既定モデルは Claude（`claude-opus-4-8`）前提。`effort` は
//! `output_config.effort` へ、思考は adaptive thinking へ翻訳する（budget_tokens は使わない）。
//!
//! 本アダプタは実装するが検証経路は openai-compat（human 指示）。ここではメッセージ変換の
//! 単体テストのみ行い、実サーバ結線はコードレビュー範囲とする。

use std::collections::HashSet;

use futures::channel::mpsc;
use futures::stream::StreamExt;
use serde_json::{json, Value};

use super::anthropic_events::EventReader;
use super::anthropic_headers::{self, RateLimitHeaders};
use super::identity::IdentityHeaders;
use super::timeouts::RequestClock;
use super::tool_names::ToolNameMap;
use crate::config::LlmTimeouts;
use crate::model::{Block, GenerateRequest, Message, Role, StreamDelta, ToolDef};
use crate::provider::{DeltaStream, LlmError, LlmProvider};
use crate::structured::{anthropic_format, schema_rejection, with_json_body, ANTHROPIC_SCHEMA_AT};
use crate::tool_loading::{context_tools, with_loaded_note};

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic 直結アダプタ。
pub struct AnthropicProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    default_model: String,
    identity: IdentityHeaders,
    timeouts: LlmTimeouts,
    /// prompt caching を使うモデル（実 ID・カタログの `prompt_cache`・T00）。
    prompt_cache_models: HashSet<String>,
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
            identity: IdentityHeaders::default(),
            timeouts: LlmTimeouts::default(),
            prompt_cache_models: HashSet::new(),
        }
    }

    /// prompt caching（要求のトップレベルの `cache_control`・5 分）を付けるモデル（実 ID）を指定する。
    /// ここに無いモデルの要求は変えない（自動キャッシュを受けない送り先へ送っても 400 にならないように）。
    #[must_use]
    pub fn with_prompt_cache_models(mut self, models: impl IntoIterator<Item = String>) -> Self {
        self.prompt_cache_models = models.into_iter().collect();
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
        // tool search（`defer_loading`＋`tool_reference`）はモデルが対応している時だけ使う。
        // 非対応モデルでは OpenAI 互換と同じく、読み込み済みの定義だけを `tools` に載せる。
        let native = supports_tool_search(&model);
        let refs = RefRendering {
            known: req.tools.iter().map(|t| t.name.as_str()).collect(),
            native,
            names,
        };
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| to_anthropic_message(m, &refs))
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
        // 自動キャッシュ: API が最後のキャッシュできるブロックに区切りを置く（system は文字列のまま）。
        if self.prompt_cache_models.contains(&model) {
            body["cache_control"] = json!({ "type": "ephemeral" });
        }
        let tools: Vec<&ToolDef> = if native {
            req.tools.iter().collect()
        } else {
            context_tools(&req.tools, &req.messages)
        };
        if !tools.is_empty() {
            // 全ツールが遅延だと API は 400 を返す（最低 1 つは非遅延が要る）。その場合は
            // 遅延を無視して全部載せる（提示を欠くより文脈が膨らむ方が安全）。
            let defer = native && tools.iter().any(|t| !t.defer_loading);
            body["tools"] = json!(tools
                .iter()
                .map(|t| {
                    let mut v = json!({
                        "name": names.wire(&t.name),
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
        // output_config は effort と構造化出力の format を**併合**する（片方で丸ごと上書きすると、
        // effort 付きの呼び出しで形の指定が黙って消える・GAP-13）。
        let mut output_config = serde_json::Map::new();
        if let Some(effort) = req.effort {
            // adaptive thinking + effort（Claude 4.6+）。budget_tokens は使わない。
            body["thinking"] = json!({ "type": "adaptive" });
            output_config.insert("effort".into(), json!(effort.as_str()));
        }
        // format だけのときは thinking を足さない（思考の既定はモデルに任せる）。
        if let Some(format) = req.response_format.as_ref().and_then(anthropic_format) {
            output_config.insert("format".into(), format);
        }
        if !output_config.is_empty() {
            body["output_config"] = Value::Object(output_config);
        }
        // temperature / top_p / top_k は送らない（新しいモデルは既定以外を 400 で拒む。決定性は
        // 構造化出力・版の固定で担保する）。
        body
    }
}

/// tool search に対応するモデルか（[互換性表] で非対応と明記されたものだけを外す）。
///
/// 対応は Claude 4.5 世代以降（Haiku 4.5・Sonnet 4.5・Opus 4.5〜）。Opus 4.1 以前と Claude 3 系は
/// `defer_loading` / `tool_reference` を受け付けない。未知の ID（新しいモデル）は対応扱いにする。
///
/// [互換性表]: https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool#model-compatibility
fn supports_tool_search(model: &str) -> bool {
    const UNSUPPORTED: [&str; 7] = [
        "claude-3",
        "claude-opus-4-0",
        "claude-opus-4-1",
        "claude-opus-4-2025",
        "claude-sonnet-4-0",
        "claude-sonnet-4-2025",
        "claude-haiku-4-0",
    ];
    !UNSUPPORTED.iter().any(|p| model.starts_with(p))
}

/// 履歴とツール参照の写し方。
struct RefRendering<'a> {
    /// このリクエストで提示するツール名（中立名。参照先が在るかの判定に使う）。
    known: HashSet<&'a str>,
    /// `tool_reference` ブロックで送るか（false なら本文に名前を書き足す）。
    native: bool,
    /// ツール名の wire 写像（ドット入りの名前は API が 400 で拒否する・#582）。
    names: &'a ToolNameMap,
}

/// 中立メッセージ 1 件を Anthropic message へ写す（tool 結果は user ロールの tool_result ブロック）。
///
/// `tool_reference` を返した tool_result の本文は、tool_result の中ではなく**同じメッセージの
/// 末尾**に text ブロックで置く（[`tool_result_content`] 参照）。
fn to_anthropic_message(m: &Message, refs: &RefRendering<'_>) -> Value {
    let mut blocks: Vec<Value> = Vec::new();
    let mut trailing: Vec<Value> = Vec::new();
    for b in &m.content {
        if let Some((block, after)) = to_anthropic_block(b, refs) {
            blocks.push(block);
            trailing.extend(after);
        }
    }
    blocks.extend(trailing);
    let role = match m.role {
        // system はトップレベル（build_body 側）へ回すため空にする。
        Role::System => return json!({ "role": "user", "content": [] }),
        Role::Assistant => "assistant",
        // user と tool 結果はどちらも user ロール（Anthropic は tool_result を user ブロックに置く）。
        Role::User | Role::Tool => "user",
    };
    json!({ "role": role, "content": blocks })
}

/// tool_result の content と、メッセージ末尾へ回す本文。
///
/// 読み込み参照があれば content を `tool_reference` ブロック**だけ**にする（API がそれを遅延
/// ツールの完全な定義へ展開する）。**API は `tool_reference` を含む tool_result に他の種類の
/// ブロックを混ぜると 400**（"Tool definitions/code execution functions cannot be mixed with
/// other content"）を返すため、本文（ループが観測に書き足す残り予算・催促）は tool_result の
/// 外へ出し、呼び出し側が同じメッセージの tool_result 群の後ろに text ブロックで置く（#857）。
///
/// `tools` に無い参照（ツールを外した着地ターン・配線が変わった再開）を送ると API は 400 を
/// 返すため、**提示中のものだけ**を参照として残す。1 つも残らなければ本文だけを送る。
fn tool_result_content(
    content: &str,
    references: &[String],
    refs: &RefRendering<'_>,
) -> (Value, Option<Value>) {
    // 参照の判定は中立名で行い、送り出す名前（`tool_reference` も本文の案内も）は wire 名にする
    // （`tools` に載せた名前と一致しないと API が参照を解決できない）。
    let loaded: Vec<String> = references
        .iter()
        .filter(|r| refs.known.contains(r.as_str()))
        .map(|r| refs.names.wire(r))
        .collect();
    if loaded.is_empty() {
        return (json!(content), None);
    }
    if !refs.native {
        return (json!(with_loaded_note(content, &loaded)), None);
    }
    let blocks: Vec<Value> = loaded
        .iter()
        .map(|r| json!({ "type": "tool_reference", "tool_name": r }))
        .collect();
    let after = (!content.is_empty()).then(|| json!({ "type": "text", "text": content }));
    (json!(blocks), after)
}

/// 中立ブロック 1 つを Anthropic のブロックへ写す。2 つ目はメッセージ末尾へ回す本文
/// （`tool_reference` を返した tool_result の本文だけ。[`tool_result_content`] 参照）。
fn to_anthropic_block(b: &Block, refs: &RefRendering<'_>) -> Option<(Value, Option<Value>)> {
    match b {
        Block::Text { text } => Some((json!({ "type": "text", "text": text }), None)),
        Block::Image {
            media_type, data, ..
        } => Some((
            json!({
                "type":"image", "source":{"type":"base64", "media_type":media_type, "data":data.0.as_ref()}
            }),
            None,
        )),
        // 再送する thinking はここでは扱わない（Phase 3 では省略）。
        // 他プロバイダの不透明な推論アイテムも送らない。
        Block::Thinking { .. } | Block::Reasoning { .. } => None,
        Block::ToolUse { id, name, input } => Some((
            json!({
                "type": "tool_use", "id": id, "name": refs.names.wire(name), "input": input,
            }),
            None,
        )),
        Block::ToolResult {
            tool_use_id,
            content,
            is_error,
            tool_references,
        } => {
            let (content, after) = tool_result_content(content, tool_references, refs);
            Some((
                json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                    "is_error": is_error,
                }),
                after,
            ))
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        let url = format!("{}/v1/messages", self.base_url);
        let names = ToolNameMap::for_request(req);
        let body = self.build_body(req, &names);
        let builder = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        let builder = with_json_body(builder, &body, req, &ANTHROPIC_SCHEMA_AT);
        let clock = RequestClock::start(self.timeouts, "anthropic");
        let resp = send_waiting_out_rate_limit(
            &clock,
            self.identity.apply(builder, req.session_id.as_deref()),
        )
        .await?;
        let status = resp.status();
        anthropic_headers::record(status, resp.headers());
        if !status.is_success() {
            let text = clock.error_text(resp).await?;
            if status.is_client_error() {
                // 429 は「直せば通る」ものではない（openai 側と同じ理由・provider.rs 参照）。
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    return Err(LlmError::RateLimited(crate::provider::rate_limit_message(
                        &text,
                    )));
                }
                // 構造化出力のスキーマを拒んだもの（複雑すぎる等）は、本文を出さない定型文にする。
                if let Some(e) = body
                    .pointer("/output_config/format")
                    .and_then(|_| schema_rejection("anthropic", status, &text))
                {
                    return Err(e);
                }
                return Err(crate::provider::rejection("anthropic", status, &text));
            }
            return Err(crate::provider::unavailable("anthropic", status, &text));
        }

        let (tx, rx) = mpsc::unbounded::<Result<StreamDelta, LlmError>>();
        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut reader = EventReader::new(names);
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
                    for out in reader.line(&String::from_utf8_lossy(&line)) {
                        let _ = tx.unbounded_send(out);
                    }
                    if reader.finished() {
                        return;
                    }
                }
            }
            // 受け手がストリームを捨てた（読むのをやめた）なら、何も送らずに終える。
            if tx.is_closed() {
                return;
            }
            // 末尾の改行が無いまま閉じた最後の行も読む。message_stop まで来ていなければ、途中までの応答を完了に
            // せず誤りにする（BUG-214）。
            for out in reader.line(&String::from_utf8_lossy(&buf)) {
                let _ = tx.unbounded_send(out);
            }
            if let Some(out) = reader.end() {
                let _ = tx.unbounded_send(out);
            }
        });

        Ok(rx.boxed())
    }
}

/// 429 の `retry-after` がこの長さ以下なら、その分だけ待って送り直す（これより長い待ちは、利用者へ
/// 「上限に達した」と返す）。
const MAX_RATE_LIMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// 429 を待って送り直す回数の上限（最初の送信を除く）。
const RATE_LIMIT_RETRIES: usize = 2;

/// 送信して応答ヘッダを待つ。429 が短い待ち（`retry-after` の秒数が [`MAX_RATE_LIMIT_WAIT`] 以下）を示すときは、
/// その分だけ待って [`RATE_LIMIT_RETRIES`] 回まで送り直す。
///
/// Anthropic の 429 は 1 分あたりのトークン・要求の枠で、数秒待てば通ることが多い。すぐ失敗にすると会話全体の
/// やり直しになる（やり直しは適用済みのツールを二重に実行しうる・#889）。送り直さないとき（待ちの指示が無い・
/// 長すぎる・生成全体の締め切りを越える・回数を使い切った）は 429 の応答をそのまま返し、呼び出し側が
/// いつもどおり [`LlmError::RateLimited`] にする。ゲートウェイの利用上限はプロバイダへ送る前に断るので、
/// ここは通らない（送り直されない）。
async fn send_waiting_out_rate_limit(
    clock: &RequestClock,
    builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response, LlmError> {
    let mut builder = builder;
    let mut retries = 0;
    loop {
        let again = builder.try_clone();
        let resp = clock.send(builder).await?;
        if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS || retries == RATE_LIMIT_RETRIES
        {
            return Ok(resp);
        }
        let wait = rate_limit_wait(resp.headers(), clock.remaining());
        let (Some(wait), Some(again)) = (wait, again) else {
            return Ok(resp);
        };
        let limits = RateLimitHeaders(resp.headers()).to_string();
        let body = clock.error_text(resp).await?;
        retries += 1;
        tracing::warn!(?wait, retries, rate_limit = %limits, body = %body, "anthropic が 429 で待ちを指示した（待って送り直す）");
        tokio::time::sleep(wait).await;
        builder = again;
    }
}

/// 429 の応答ヘッダから、送り直す前に待つ長さを決める（送り直さないなら `None`）。
///
/// `retry-after` の秒数（整数）だけを読む。HTTP-date の形・読めない値・指示の無いものは送り直さない（待ちを推測で
/// 決めない）。[`MAX_RATE_LIMIT_WAIT`] を超える待ちと、生成全体の締め切りまでの残り（`remaining`）以上の待ちもしない
/// （暴走止めの上限を待ちで延ばさない）。
fn rate_limit_wait(
    headers: &reqwest::header::HeaderMap,
    remaining: std::time::Duration,
) -> Option<std::time::Duration> {
    let secs = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let wait = std::time::Duration::from_secs(secs);
    (wait <= MAX_RATE_LIMIT_WAIT && wait < remaining).then_some(wait)
}

#[cfg(test)]
mod tests {
    #[test]
    fn user_image_is_sent_as_base64_source() {
        let image = Block::Image {
            media_type: "image/png".into(),
            source_id: "file".into(),
            data: crate::ImageData("YWJj".into()),
        };
        let names = ToolNameMap::new(std::iter::empty::<&str>());
        let refs = RefRendering {
            known: HashSet::new(),
            native: true,
            names: &names,
        };
        let (wire, after) = to_anthropic_block(&image, &refs).expect("image");
        assert_eq!(
            wire["source"],
            json!({"type":"base64", "media_type":"image/png", "data":"YWJj"})
        );
        assert!(after.is_none());
    }
    use super::*;

    /// 429 の `retry-after` は 20 秒まで待つ。長い待ち・HTTP-date・読めない値・指示の無いもの・締め切りを越える待ちは
    /// 送り直さない。
    #[test]
    fn rate_limit_wait_honors_only_short_retry_after_seconds() {
        use std::time::Duration;
        let at = |v: &str| {
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(reqwest::header::RETRY_AFTER, v.parse().unwrap());
            h
        };
        let plenty = Duration::from_secs(600);
        assert_eq!(rate_limit_wait(&at("0"), plenty), Some(Duration::ZERO));
        assert_eq!(
            rate_limit_wait(&at(" 3 "), plenty),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            rate_limit_wait(&at("20"), plenty),
            Some(Duration::from_secs(20))
        );
        for no in [
            "21",
            "86400",
            "-1",
            "1.5",
            "soon",
            "Wed, 21 Oct 2026 07:28:00 GMT",
        ] {
            assert_eq!(rate_limit_wait(&at(no), plenty), None, "{no}");
        }
        assert_eq!(
            rate_limit_wait(&reqwest::header::HeaderMap::new(), plenty),
            None
        );
        // 締め切りまでの残りより長い（等しい）待ちはしない。
        assert_eq!(rate_limit_wait(&at("5"), Duration::from_secs(5)), None);
        assert_eq!(
            rate_limit_wait(&at("4"), Duration::from_secs(5)),
            Some(Duration::from_secs(4))
        );
    }

    #[test]
    fn tool_result_maps_to_user_tool_result_block() {
        let m = Message {
            role: Role::Tool,
            content: vec![Block::tool_result("t1", "r", false)],
        };
        let names = ToolNameMap::new(std::iter::empty::<&str>());
        let refs = RefRendering {
            known: HashSet::new(),
            native: true,
            names: &names,
        };
        let out = to_anthropic_message(&m, &refs);
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
        let body = p.build_body(&req, &ToolNameMap::for_request(&req));
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["max_tokens"], 4096);
    }

    fn structured_request() -> GenerateRequest {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "分類して")]);
        req.response_format = Some(crate::model::ResponseFormat::JsonSchema {
            name: "triage".into(),
            schema: json!({
                "type": "object",
                "required": ["category"],
                "properties": {
                    "category": { "type": "string", "enum": ["billing", "bug"] },
                    "score": { "type": "integer", "minimum": 1 },
                },
            }),
        });
        req
    }

    #[test]
    fn effort_and_format_are_merged_into_output_config() {
        // 併合の回帰テスト: effort を書いても format が消えない（以前は output_config を上書きしていた）。
        let mut req = structured_request();
        req.effort = Some(crate::model::Effort::Low);
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "low");
        let format = &body["output_config"]["format"];
        assert_eq!(format["type"], "json_schema");
        assert_eq!(format["schema"]["additionalProperties"], false);
        assert_eq!(
            format["schema"]["properties"]["category"]["enum"],
            json!(["billing", "bug"])
        );
        // 受けない制約は落として説明に書く（正しさは呼び出し側の事後検証）。
        assert!(format["schema"]["properties"]["score"]
            .get("minimum")
            .is_none());
    }

    #[test]
    fn format_alone_adds_no_thinking_or_effort() {
        let req = structured_request();
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        assert!(body.get("thinking").is_none());
        assert!(body["output_config"].get("effort").is_none());
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        // スキーマを求めない形は Anthropic に無いので送らない（gateway が指示に落とす）。
        let mut plain = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        plain.response_format = Some(crate::model::ResponseFormat::JsonObject);
        let body = provider().build_body(&plain, &ToolNameMap::for_request(&plain));
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn sampling_parameters_are_never_sent() {
        // 新しいモデル（Haiku 5.5 等）は temperature・top_p・top_k の既定以外を 400 で拒む。
        // 中立形に temperature が来ても（skill のモデル既定など）送らないことを固定する。
        let mut req = structured_request();
        req.temperature = Some(0.0);
        req.effort = Some(crate::model::Effort::High);
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        for key in ["temperature", "top_p", "top_k"] {
            assert!(body.get(key).is_none(), "{key}: {body}");
        }
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
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        assert!(body["tools"][0].get("defer_loading").is_none());
        assert_eq!(body["tools"][1]["defer_loading"], true);
        // tool_result の content は tool_reference だけ（他の種類を混ぜると API が 400・#857）。
        // 本文は同じメッセージの tool_result の後ろへ text で置く。
        let msg = &body["messages"][1]["content"];
        assert_eq!(
            msg,
            &json!([
                {
                    "type": "tool_result",
                    "tool_use_id": "t1",
                    "content": [{ "type": "tool_reference", "tool_name": "csv_query" }],
                    "is_error": false,
                },
                { "type": "text", "text": "読み込みました" },
            ])
        );
    }

    #[test]
    fn trailing_texts_follow_every_tool_result_in_the_message() {
        // 並列で 2 つ読み込んだとき、tool_result が先に並び、本文はその後ろにまとまる
        // （API は user メッセージで tool_result を text より前に置くことを求める）。
        let mut req = GenerateRequest::new(vec![
            Message::text(Role::User, "hi"),
            Message {
                role: Role::Tool,
                content: vec![
                    Block::ToolResult {
                        tool_use_id: "t1".into(),
                        content: "一つ目".into(),
                        is_error: false,
                        tool_references: vec!["csv.query".into()],
                    },
                    Block::tool_result("t2", "普通の結果", false),
                    Block::ToolResult {
                        tool_use_id: "t3".into(),
                        content: String::new(),
                        is_error: false,
                        tool_references: vec!["office.edit".into()],
                    },
                ],
            },
        ]);
        req.tools = vec![
            ToolDef::new("tool_search", "s", json!({"type": "object"})),
            deferred("csv.query"),
            deferred("office.edit"),
        ];
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        let msg = body["messages"][1]["content"].as_array().unwrap();
        let kinds: Vec<&str> = msg.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["tool_result", "tool_result", "tool_result", "text"]);
        assert_eq!(
            msg[0]["content"],
            json!([{ "type": "tool_reference", "tool_name": "csv_query" }])
        );
        assert_eq!(msg[1]["content"], "普通の結果");
        // 本文が空なら text を足さない。
        assert_eq!(
            msg[2]["content"],
            json!([{ "type": "tool_reference", "tool_name": "office_edit" }])
        );
        assert_eq!(msg[3]["text"], "一つ目");
    }

    #[test]
    fn reference_to_tool_absent_from_request_falls_back_to_text() {
        // 着地ターン（tools 空）で参照を送ると API が 400 を返すため、本文へ落とす。
        let req = GenerateRequest::new(vec![
            Message::text(Role::User, "hi"),
            loaded(&["csv.query"]),
        ]);
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        assert_eq!(
            body["messages"][1]["content"][0]["content"],
            "読み込みました"
        );
    }

    #[test]
    fn all_deferred_request_drops_defer_flags() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.tools = vec![deferred("a"), deferred("b")];
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        assert!(body["tools"][0].get("defer_loading").is_none());
        assert!(body["tools"][1].get("defer_loading").is_none());
    }

    #[test]
    fn unknown_references_are_dropped_but_known_ones_stay_native() {
        let mut req = GenerateRequest::new(vec![
            Message::text(Role::User, "hi"),
            loaded(&["csv.query", "gone.tool"]),
        ]);
        req.tools = vec![
            ToolDef::new("tool_search", "s", json!({"type": "object"})),
            deferred("csv.query"),
        ];
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        let content = &body["messages"][1]["content"][0]["content"];
        assert_eq!(content[0]["tool_name"], "csv_query");
        assert_eq!(content.as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][1]["content"][1]["type"], "text");
    }

    #[test]
    fn models_without_tool_search_get_loaded_definitions_appended_instead() {
        let mut req = GenerateRequest::new(vec![
            Message::text(Role::User, "hi"),
            loaded(&["csv.query"]),
        ]);
        req.model = Some("claude-opus-4-1-20250805".into());
        req.tools = vec![
            ToolDef::new("tool_search", "s", json!({"type": "object"})),
            deferred("csv.query"),
            deferred("office.edit"),
        ];
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        // 読み込み済み（csv.query）だけが載り、defer_loading は送らない。
        assert_eq!(names, ["tool_search", "csv_query"]);
        assert!(body["tools"][1].get("defer_loading").is_none());
        let content = body["messages"][1]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert!(content.starts_with("読み込みました") && content.contains("csv_query"));
    }

    #[test]
    fn dotted_tool_names_are_sent_as_wire_names_in_tools_and_history() {
        // Anthropic は tools[].name に `^[a-zA-Z0-9_-]{1,128}$` を要求する（#582）。定義・履歴の
        // tool_use の両方で同じ wire 名に揃え、tool_result は id で結びつく。
        let mut req = GenerateRequest::new(vec![
            Message::text(Role::User, "hi"),
            Message {
                role: Role::Assistant,
                content: vec![Block::ToolUse {
                    id: "t1".into(),
                    name: "document.edit".into(),
                    input: json!({}),
                }],
            },
            Message {
                role: Role::Tool,
                content: vec![Block::tool_result("t1", "ok", false)],
            },
        ]);
        req.tools = vec![ToolDef::new(
            "document.edit",
            "d",
            json!({"type": "object"}),
        )];
        let body = provider().build_body(&req, &ToolNameMap::for_request(&req));
        assert_eq!(body["tools"][0]["name"], "document_edit");
        assert_eq!(body["messages"][1]["content"][0]["name"], "document_edit");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "t1");
    }

    #[test]
    fn tool_search_support_follows_the_compatibility_table() {
        for m in [
            "claude-opus-4-1-20250805",
            "claude-opus-4-20250514",
            "claude-sonnet-4-20250514",
            "claude-3-7-sonnet-latest",
        ] {
            assert!(!supports_tool_search(m), "{m}");
        }
        for m in [
            "claude-opus-4-8",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
            "claude-opus-5-5",
        ] {
            assert!(supports_tool_search(m), "{m}");
        }
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
            AnthropicProvider::new(reqwest::Client::new(), base, "k", "m"),
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

    /// 思考だけを流して切れた（message_stop の無い・error イベントの）応答は送り直し、本文・ツール呼び出しを
    /// 出した後に切れた応答は送り直さずに誤りにする（BUG-214・二重に渡さない）。
    #[tokio::test]
    async fn cut_streams_are_sent_again_only_while_nothing_is_settled() {
        use std::sync::atomic::Ordering;
        const THINK: &str = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"考え中\"}}\n\n";
        const OVERLOADED: &str = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        const ANSWER: &str = concat!(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"答え\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        const TEXT_THEN_CLOSE: &str = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"途中\"}}\n\n";
        const TOOL_THEN_CLOSE: &str = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"doc_search\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\"}}\n\n",
        );
        let think_then_error: &'static str =
            Box::leak(format!("{THINK}{OVERLOADED}").into_boxed_str());

        // 思考だけで閉じた・思考だけで error イベント → 送り直して答えを流す。
        for cut in [THINK, think_then_error] {
            let (base, calls) = scripted(vec![cut, ANSWER]).await;
            let out = through_resume(base).await;
            assert_eq!(calls.load(Ordering::SeqCst), 2, "{out:?}");
            assert_eq!(out.last().map(String::as_str), Some("done"), "{out:?}");
            assert!(out.contains(&"text:答え".to_string()), "{out:?}");
        }

        // 本文を出した後・書きかけのツール呼び出しで閉じた・本文の後の error イベント → 送り直さずに誤り。
        let text_then_error: &'static str =
            Box::leak(format!("{TEXT_THEN_CLOSE}{OVERLOADED}").into_boxed_str());
        for cut in [TEXT_THEN_CLOSE, TOOL_THEN_CLOSE, text_then_error] {
            let (base, calls) = scripted(vec![cut, ANSWER]).await;
            let out = through_resume(base).await;
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{cut}: {out:?}");
            let last = out.last().unwrap();
            assert!(
                last.starts_with("err:llm provider unavailable: cut by provider: anthropic: "),
                "{cut}: {out:?}"
            );
            assert!(!out.iter().any(|o| o == "done"), "{cut}: {out:?}");
        }
    }
}

// prompt caching（T00）の単体（このファイルを太らせないよう分ける）。
#[cfg(test)]
#[path = "anthropic_cache_tests.rs"]
mod cache_tests;
