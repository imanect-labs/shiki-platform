//! stub の構造化出力（GAP-13・`stub.rs` から分割）。実 LLM なしで llm.invoke の出力の形・修復・
//! 失敗の経路を結合テストと E2E で通すための決定的な応答。
//!
//! 形の指定（`response_format`、または prompt・json_object モードで system に足された指示）が
//! あるときだけ働く:
//! - 既定: スキーマに合う最小の値を返す（文字列の enum は本文に現れた値を優先し、無ければ先頭）。
//! - `json:<本文>` … 直近の user の本文を返す（不正な JSON・形の合わない出力の試験。
//!   修復の依頼〔直近の user が変わる〕には既定の最小値で答えるので、修復で直る経路になる）。
//! - `jsonalways:<本文>` … 最初の user の本文を、修復の依頼にも毎回返す（修復しても直らない経路）。
//! - `refuse:` … 拒否（[`StopReason::Refusal`]）で終える。
//! - `truncate:` … JSON の途中で max_tokens（[`StopReason::MaxTokens`]）に達して終える。
//!
//! 駆動の語は、本文の頭だけでなく、どの行の頭に置いてもよい（[`line_driver`]・最初に見つかった行の語を使う）。
//! 資料の本文（PDF の「〔p.1〕」など）や前置きを先に並べた、本物に近い prompt でも駆動できるようにする。
//! `json:`・`jsonalways:` は、語の後ろが JSON の値で始まれば、その最初の値だけを返して続く文を捨てる。
//! 始まらなければ、語の後ろから本文の終わりまでを返す（[`json_body`]）。
//!
//! 形の指定が無い呼び出しの失敗の駆動 `fail:`（[`failure_call`]）もここに置く。拾い方は同じく、どの行の頭でもよい。
//! チャットの `truncate:`（形の指定が無いとき・`stub.rs`）は本文の頭だけを見る（続きの催促〔#1247〕の判定が
//! 最初の発話の頭を見ているので、拾い方を揃えておく）。

use futures::stream::{self, StreamExt};
use serde_json::{Map, Value};

use super::stub_stream::text_stream;
use crate::model::{Block, GenerateRequest, ResponseFormat, Role, StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError};
use crate::structured::schema_in_system;

/// 形の指定があれば構造化出力の応答を返す（無ければ `None`・通常の stub の応答へ）。
pub(super) fn structured_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    let schema = requested_schema(req)?;
    if let Some(rest) = line_driver(&first_user_text(req), "jsonalways:") {
        return Some(text_stream(json_body(rest), prompt_tokens));
    }
    if let Some(rest) = line_driver(user_text, "json:") {
        return Some(text_stream(json_body(rest), prompt_tokens));
    }
    if line_driver(user_text, "refuse:").is_some() {
        return Some(ending(
            "この依頼にはお答えできません。",
            StopReason::Refusal,
            prompt_tokens,
        ));
    }
    if line_driver(user_text, "truncate:").is_some() {
        return Some(ending("{\"", StopReason::MaxTokens, prompt_tokens));
    }
    let hints = all_user_text(req);
    let value = minimal_value(&schema, &hints, 0);
    Some(text_stream(&value.to_string(), prompt_tokens))
}

/// `fail:`（形の指定が無い呼び出し）… 送り直されない誤り（[`LlmError::BadRequest`]）で失敗する。ワークフローの
/// llm.invoke では、再試行しない失敗（`invalid`）になる。一時障害（`Unavailable`）にすると、gateway の送り直し
/// （#743・D-151）や step の再試行に掛かり、失敗の出口を 1 回で通せない。形の指定があるときは [`structured_call`] が先に
/// 答える（形の指定がある呼び出しの失敗は `refuse:`・`truncate:`・`jsonalways:` で通す）。
pub(super) fn failure_call(user_text: &str) -> Option<DeltaStream> {
    line_driver(user_text, "fail:")?;
    Some(
        stream::iter([Err(LlmError::BadRequest(
            "stub: fail: の駆動で失敗させた".into(),
        ))])
        .boxed(),
    )
}

/// 本文のどこかの行の頭にある駆動の語 `word` の後ろ（最初に見つかった行の語から、本文の終わりまで）。
fn line_driver<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .find_map(|start| text[start..].strip_prefix(word))
}

/// `json:`・`jsonalways:` の後ろ `rest` から返す本文。JSON の値で始まれば最初の値だけ（書いたままの文字）、
/// 始まらなければ全体（前後の空白は外す）。
fn json_body(rest: &str) -> &str {
    let mut values = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    match values.next() {
        Some(Ok(_)) => rest[..values.byte_offset()].trim(),
        _ => rest.trim(),
    }
}

/// 求められた形のスキーマ（`response_format` → system の指示の順・スキーマなしは空のオブジェクト）。
fn requested_schema(req: &GenerateRequest) -> Option<Value> {
    if let Some(ResponseFormat::JsonSchema { schema, .. }) = &req.response_format {
        return Some(schema.clone());
    }
    if let Some(schema) = req.system.as_deref().and_then(schema_in_system) {
        return Some(schema);
    }
    matches!(req.response_format, Some(ResponseFormat::JsonObject))
        .then(|| serde_json::json!({ "type": "object" }))
}

fn user_texts(req: &GenerateRequest) -> impl Iterator<Item = String> + '_ {
    req.messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
}

fn first_user_text(req: &GenerateRequest) -> String {
    user_texts(req).next().unwrap_or_default()
}

fn all_user_text(req: &GenerateRequest) -> String {
    user_texts(req).collect::<Vec<_>>().join("\n")
}

/// 本文 `text` を流してから `stop` で終える。
fn ending(text: &str, stop: StopReason, prompt_tokens: u64) -> DeltaStream {
    stream::iter([
        Ok(StreamDelta::TextDelta {
            text: text.to_string(),
        }),
        Ok(StreamDelta::Done {
            stop_reason: stop,
            usage: Usage {
                prompt_tokens,
                completion_tokens: 1,
                ..Default::default()
            },
        }),
    ])
    .boxed()
}

/// スキーマに合う最小の値（呼び出し側の事後検証を通る形）。`hints` は user の本文（enum の選択に使う）。
fn minimal_value(schema: &Value, hints: &str, depth: usize) -> Value {
    let Value::Object(s) = schema else {
        return Value::Null;
    };
    if depth > 32 {
        return Value::Null;
    }
    if let Some(c) = s.get("const") {
        return c.clone();
    }
    if let Some(Value::Array(options)) = s.get("enum") {
        return options
            .iter()
            .find(|o| {
                o.as_str()
                    .is_some_and(|t| !t.is_empty() && hints.contains(t))
            })
            .or_else(|| options.first())
            .cloned()
            .unwrap_or(Value::Null);
    }
    if let Some(Value::Array(branches)) = s.get("anyOf") {
        return branches
            .first()
            .map_or(Value::Null, |b| minimal_value(b, hints, depth + 1));
    }
    match primary_type(s) {
        Some("object") => object_value(s, hints, depth),
        Some("array") => {
            let n = s.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            let item = s
                .get("items")
                .map_or(Value::Null, |i| minimal_value(i, hints, depth + 1));
            Value::Array((0..n.min(16)).map(|_| item.clone()).collect())
        }
        Some("string") => Value::String(string_value(s)),
        Some(t @ ("integer" | "number")) => number_value(s, t == "integer"),
        Some("boolean") => Value::Bool(false),
        _ => Value::Null,
    }
}

/// `type`（`[T, "null"]` は T）。
fn primary_type(s: &Map<String, Value>) -> Option<&str> {
    match s.get("type")? {
        Value::String(t) => Some(t.as_str()),
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).find(|t| *t != "null"),
        _ => None,
    }
}

fn object_value(s: &Map<String, Value>, hints: &str, depth: usize) -> Value {
    let props = s.get("properties").and_then(Value::as_object);
    Value::Object(
        props
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), minimal_value(v, hints, depth + 1)))
            .collect(),
    )
}

fn string_value(s: &Map<String, Value>) -> String {
    match s.get("format").and_then(Value::as_str) {
        Some("date") => return "2026-01-01".into(),
        Some("date-time") => return "2026-01-01T00:00:00Z".into(),
        _ => {}
    }
    let n = s.get("minLength").and_then(Value::as_u64).unwrap_or(0);
    "x".repeat(usize::try_from(n.min(64)).unwrap_or(0))
}

fn number_value(s: &Map<String, Value>, integer: bool) -> Value {
    let lower = s
        .get("minimum")
        .and_then(Value::as_f64)
        .or_else(|| {
            s.get("exclusiveMinimum").and_then(Value::as_f64).map(|m| {
                if integer {
                    m.floor() + 1.0
                } else {
                    m + 1.0
                }
            })
        })
        .unwrap_or(0.0);
    let upper = s.get("maximum").and_then(Value::as_f64);
    let v = upper.map_or(lower, |u| lower.min(u));
    if integer {
        // 値域は呼び出し側の検査（16 KiB・数値の範囲）で小さく抑えてある。
        #[allow(clippy::cast_possible_truncation)]
        let i = v.ceil() as i64;
        Value::from(i)
    } else {
        serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde_json::json;

    use super::*;
    use crate::model::Message;
    use crate::structured::{apply_mode, StructuredOutputMode};

    fn req(user: &str, schema: Value) -> GenerateRequest {
        let mut r = GenerateRequest::new(vec![Message::text(Role::User, user)]);
        r.response_format = Some(ResponseFormat::JsonSchema {
            name: "out".into(),
            schema,
        });
        r
    }

    async fn run(req: &GenerateRequest) -> (String, StopReason) {
        let user = user_texts(req).last().unwrap_or_default();
        let mut s = structured_call(req, &user, 1).expect("structured");
        let (mut text, mut stop) = (String::new(), StopReason::Other);
        while let Some(d) = s.next().await {
            match d.unwrap() {
                StreamDelta::TextDelta { text: t } => text.push_str(&t),
                StreamDelta::Done { stop_reason, .. } => stop = stop_reason,
                _ => {}
            }
        }
        (text, stop)
    }

    fn triage() -> Value {
        json!({
            "type": "object",
            "required": ["category", "score"],
            "properties": {
                "category": { "type": "string", "enum": ["billing", "bug", "howto"] },
                "score": { "type": "integer", "minimum": 1, "maximum": 5 },
                "summary": { "type": "string", "minLength": 3 },
                "due": { "type": ["string", "null"], "format": "date" },
                "ok": { "type": "boolean" },
                "tags": { "type": "array", "minItems": 1, "items": { "type": "string" } },
                "nested": { "type": "object", "properties": { "n": { "type": "number", "exclusiveMinimum": 0.5 } } },
            },
        })
    }

    #[tokio::test]
    async fn returns_the_smallest_value_that_fits_the_schema() {
        let (text, stop) = run(&req("どうぞ", triage())).await;
        assert_eq!(stop, StopReason::EndTurn);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v,
            json!({
                "category": "billing", "score": 1, "summary": "xxx", "due": "2026-01-01",
                "ok": false, "tags": [""], "nested": { "n": 1.5 },
            })
        );
    }

    #[tokio::test]
    async fn enum_prefers_a_value_mentioned_in_the_prompt() {
        let (text, _) = run(&req("不具合（bug）の報告です", triage())).await;
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["category"], "bug");
    }

    #[tokio::test]
    async fn json_prefix_returns_the_body_once_then_repairs() {
        let mut r = req("json:{\"category\": 1}", triage());
        assert_eq!(run(&r).await.0, "{\"category\": 1}");
        // 修復の依頼（直近の user が変わる）には最小値で答える。
        r.messages
            .push(Message::text(Role::Assistant, "{\"category\": 1}"));
        r.messages
            .push(Message::text(Role::User, "形式に合いません"));
        let v: Value = serde_json::from_str(&run(&r).await.0).unwrap();
        assert_eq!(v["category"], "billing");
    }

    #[tokio::test]
    async fn jsonalways_prefix_never_repairs() {
        let mut r = req("jsonalways:not json", triage());
        r.messages.push(Message::text(Role::Assistant, "not json"));
        r.messages
            .push(Message::text(Role::User, "形式に合いません"));
        assert_eq!(run(&r).await.0, "not json");
    }

    #[tokio::test]
    async fn refusal_and_truncation_end_with_their_stop_reasons() {
        assert_eq!(run(&req("refuse:", triage())).await.1, StopReason::Refusal);
        let (text, stop) = run(&req("truncate:", triage())).await;
        assert_eq!(stop, StopReason::MaxTokens);
        assert!(serde_json::from_str::<Value>(&text).is_err());
    }

    #[tokio::test]
    async fn prompt_mode_reads_the_schema_back_from_the_instruction() {
        let mut r = req("どうぞ", triage());
        apply_mode(&mut r, StructuredOutputMode::Prompt);
        assert!(r.response_format.is_none());
        let v: Value = serde_json::from_str(&run(&r).await.0).unwrap();
        assert_eq!(v["score"], 1);
        // スキーマなしの JSON オブジェクトは空のオブジェクト。
        let mut plain = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
        plain.response_format = Some(ResponseFormat::JsonObject);
        assert_eq!(run(&plain).await.0, "{}");
    }

    #[test]
    fn without_a_format_the_stub_answers_as_usual() {
        let r = GenerateRequest::new(vec![Message::text(Role::User, "json:{}")]);
        assert!(structured_call(&r, "json:{}", 1).is_none());
    }

    /// 駆動の語は、資料の本文や前置きの後ろの行の頭に置いてもよい。`json:` は最初の JSON の値だけを返す。
    #[tokio::test]
    async fn drivers_work_at_the_head_of_any_line() {
        let page = "〔p.1〕問い合わせの本文です。\n";
        let mut r = req(
            &format!("{page}json:{{\"category\": 1}} 以下は資料の続き\n次の行"),
            triage(),
        );
        assert_eq!(run(&r).await.0, "{\"category\": 1}");
        // 修復の依頼には最小値で答える（前の行の語は、直近の user が変われば見ない）。
        r.messages
            .push(Message::text(Role::Assistant, "{\"category\": 1}"));
        r.messages
            .push(Message::text(Role::User, "形式に合いません"));
        let v: Value = serde_json::from_str(&run(&r).await.0).unwrap();
        assert_eq!(v["category"], "billing");

        let mut r = req(
            &format!("{page}jsonalways:{{\"category\":\"refund\"}}\n資料の続き"),
            triage(),
        );
        r.messages
            .push(Message::text(Role::Assistant, "{\"category\":\"refund\"}"));
        r.messages
            .push(Message::text(Role::User, "形式に合いません"));
        assert_eq!(run(&r).await.0, "{\"category\":\"refund\"}");

        let (_, stop) = run(&req(&format!("{page}refuse:"), triage())).await;
        assert_eq!(stop, StopReason::Refusal);
        let (text, stop) = run(&req(&format!("{page}truncate:長い出力"), triage())).await;
        assert_eq!(stop, StopReason::MaxTokens);
        assert!(serde_json::from_str::<Value>(&text).is_err());
    }

    /// `json:` の後ろが JSON の値で始まらなければ、語の後ろから本文の終わりまでを返す（今までと同じ）。
    /// 行の途中の語は駆動ではない。
    #[tokio::test]
    async fn json_body_falls_back_to_the_rest_and_mid_line_words_are_text() {
        let (text, _) = run(&req("前置き\njson:申し訳ありません\n続き", triage())).await;
        assert_eq!(text, "申し訳ありません\n続き");
        assert_eq!(json_body(" {\"a\":[1,2]}{\"b\":2}"), "{\"a\":[1,2]}");
        assert_eq!(json_body("not json {}"), "not json {}");
        let (text, stop) = run(&req(
            "本文の途中の json:{} と refuse: は語ではない",
            triage(),
        ))
        .await;
        assert_eq!(stop, StopReason::EndTurn);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["category"], "billing");
    }

    async fn plain(text: &str) -> Vec<Result<StreamDelta, LlmError>> {
        use crate::provider::LlmProvider as _;
        let r = GenerateRequest::new(vec![Message::text(Role::User, text)]);
        let s = super::super::stub::StubProvider::new()
            .stream(&r)
            .await
            .unwrap();
        s.collect().await
    }

    /// 形の指定が無い `fail:`（どの行の頭でもよい）は、送り直されない誤り（BadRequest）だけを返す。形の指定が
    /// あれば構造化出力の応答が先に答える。
    #[tokio::test]
    async fn fail_driver_ends_a_plain_call_with_a_permanent_error() {
        for text in ["fail:", "〔p.1〕資料の本文\nfail: 失敗の出口を通す"] {
            let items = plain(text).await;
            assert_eq!(items.len(), 1, "{text}");
            assert!(
                matches!(&items[0], Err(LlmError::BadRequest(_))),
                "{text}: {:?}",
                items[0]
            );
        }
        let (text, stop) = run(&req("fail:", triage())).await;
        assert_eq!(stop, StopReason::EndTurn);
        assert!(serde_json::from_str::<Value>(&text).is_ok(), "{text}");
        // 行の途中の語は駆動ではない。
        assert!(plain("本文の fail: は語ではない")
            .await
            .iter()
            .all(Result::is_ok));
    }

    /// 形の指定が無い `truncate:` は本文の頭だけを見る（チャットの続きの催促〔#1247〕と揃える）。
    #[tokio::test]
    async fn plain_truncate_still_needs_the_head_of_the_text() {
        let items = plain("前置き\ntruncate:長い説明").await;
        let stop = items.iter().find_map(|d| match d {
            Ok(StreamDelta::Done { stop_reason, .. }) => Some(*stop_reason),
            _ => None,
        });
        assert_eq!(stop, Some(StopReason::EndTurn));
    }
}
