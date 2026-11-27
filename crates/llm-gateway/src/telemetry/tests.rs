//! `telemetry.rs` の単体テスト（ストリームを包んだ span の寿命と属性）。

use std::time::Instant;

use futures::StreamExt;

use super::capture::Captured;
use super::*;
use crate::model::Usage;

fn deltas(items: Vec<Result<StreamDelta, LlmError>>) -> DeltaStream {
    futures::stream::iter(items).boxed()
}

fn text(t: &str) -> Result<StreamDelta, LlmError> {
    Ok(StreamDelta::TextDelta { text: t.into() })
}

#[tokio::test]
async fn span_closes_at_stream_end_and_records_usage_and_ttft() {
    let cap = Captured::default();
    let _g = cap.guard();
    let parent = tracing::info_span!("chat.turn");
    let mut stream = {
        let _p = parent.enter();
        let span = llm_span();
        record_target(&span, "auto", "gpt-x-2026", "openai");
        trace_stream(
            deltas(vec![
                Ok(StreamDelta::ReasoningItem {
                    provider: "openai".into(),
                    item: serde_json::json!({}),
                }),
                text("秘密の応答"),
                text("続き"),
                Ok(StreamDelta::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        prompt_tokens: 120,
                        completion_tokens: 30,
                        ..Usage::default()
                    },
                }),
            ]),
            span,
            Instant::now(),
        )
    };
    drop(parent);

    // 1 件目（推論アイテム）では最初のトークンにならない。
    stream.next().await.unwrap().unwrap();
    assert!(!cap
        .span("gen_ai.chat")
        .fields
        .contains_key("gen_ai.server.time_to_first_token"));
    stream.next().await.unwrap().unwrap();
    let s = cap.span("gen_ai.chat");
    assert!(!s.closed, "ストリームの途中では閉じない");
    assert!(s.fields.contains_key("gen_ai.server.time_to_first_token"));
    assert_eq!(s.events, vec!["gen_ai.first_token".to_string()]);

    stream.next().await.unwrap().unwrap();
    stream.next().await.unwrap().unwrap(); // Done
    assert!(
        !cap.span("gen_ai.chat").closed,
        "終端（None）の前は閉じない"
    );
    assert!(stream.next().await.is_none());
    // 終端に達した時点で閉じる（drop を待たない＝消費側の後処理を生成時間に混ぜない）。
    let s = cap.span("gen_ai.chat");
    assert!(s.closed, "終端で閉じる");
    drop(stream);
    assert_eq!(
        s.parent.as_deref(),
        Some("chat.turn"),
        "呼び出し元の子（同じ trace）"
    );
    assert_eq!(s.field("otel.name"), "chat auto");
    assert_eq!(s.field("gen_ai.operation.name"), "chat");
    assert_eq!(s.field("shiki.llm.model"), "auto");
    assert_eq!(s.field("gen_ai.request.model"), "gpt-x-2026");
    assert_eq!(s.field("gen_ai.provider.name"), "openai");
    assert_eq!(s.field("gen_ai.usage.input_tokens"), "120");
    assert_eq!(s.field("gen_ai.usage.output_tokens"), "30");
    // finish_reasons は OTel の配列属性として直接載せる（gateway_spans_it で確認）。
    assert_eq!(s.events.len(), 1, "最初のトークンのイベントは 1 回だけ");
    // 最初のトークンのイベントは時間だけを持つ（本文を持たない）。
    let ev = &s.event_fields[0];
    assert!(ev.contains_key("time_to_first_token_s"));
    assert_eq!(ev.len(), 2, "message と時間だけ: {ev:?}");
    assert!(
        !cap.all_values().iter().any(|v| v.contains("秘密")),
        "応答本文は span・イベントのどの属性にも載せない"
    );
}

#[test]
fn record_target_sets_name_only_after_resolution() {
    let cap = Captured::default();
    cap.run(|| {
        let span = llm_span();
        drop(span);
    });
    let s = cap.span("gen_ai.chat");
    assert_eq!(s.field("otel.name"), "chat");
    assert!(!s.fields.contains_key("shiki.llm.model"));
}

/// 捕捉層自体が、イベントの message 以外の属性に入った文字列も検出できること。
#[test]
fn capture_sees_event_fields_other_than_message() {
    let cap = Captured::default();
    cap.run(|| {
        let span = llm_span();
        let _e = span.enter();
        tracing::info!(leaked = "秘密の値", "gen_ai.first_token");
    });
    assert!(cap.all_values().iter().any(|v| v.contains("秘密の値")));
}

#[tokio::test]
async fn tool_use_counts_as_first_token() {
    let cap = Captured::default();
    let _g = cap.guard();
    let mut stream = trace_stream(
        deltas(vec![Ok(StreamDelta::ToolUseStart {
            id: "t1".into(),
            name: "search".into(),
        })]),
        llm_span(),
        Instant::now(),
    );
    stream.next().await.unwrap().unwrap();
    assert!(cap
        .span("gen_ai.chat")
        .fields
        .contains_key("gen_ai.server.time_to_first_token"));
}

#[tokio::test]
async fn midstream_error_marks_span_without_message() {
    let cap = Captured::default();
    let _g = cap.guard();
    let mut stream = trace_stream(
        deltas(vec![
            text("a"),
            Err(LlmError::Unavailable("provider body: 秘密".into())),
        ]),
        llm_span(),
        Instant::now(),
    );
    while stream.next().await.is_some() {}
    let s = cap.span("gen_ai.chat");
    assert_eq!(s.field("otel.status_code"), "ERROR");
    assert_eq!(s.field("error.type"), "unavailable");
    assert!(!cap.all_values().iter().any(|v| v.contains("秘密")));
}

#[test]
fn error_and_stop_reason_names() {
    assert_eq!(error_kind(&LlmError::Config(String::new())), "config");
    assert_eq!(
        error_kind(&LlmError::RateLimited(String::new())),
        "rate_limited"
    );
    assert_eq!(
        error_kind(&LlmError::BadRequest(String::new())),
        "bad_request"
    );
    assert_eq!(error_kind(&LlmError::Internal(String::new())), "internal");
    assert_eq!(stop_reason_str(StopReason::ToolUse), "tool_use");
    assert_eq!(stop_reason_str(StopReason::MaxTokens), "max_tokens");
    assert_eq!(stop_reason_str(StopReason::Other), "other");
}
