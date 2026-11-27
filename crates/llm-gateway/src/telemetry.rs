//! LLM 呼び出しの計装（`tracing` span → OTel・GenAI semantic conventions）。
//!
//! span は**チョークポイント（[`LlmGateway::stream`](crate::gateway::LlmGateway::stream)）**で
//! 付ける。各プロバイダアダプタには付けない（差し替えで計装が抜けない・形が揃う）。
//!
//! - 属性は GenAI semconv（`gen_ai.operation.name`・`gen_ai.request.model`・
//!   `gen_ai.provider.name`・`gen_ai.usage.input_tokens`/`output_tokens`・
//!   `gen_ai.response.finish_reasons`）に合わせる。`finish_reasons` は規約どおり文字列配列で
//!   OTel の span に直接載せる（tracing のフィールドは配列を表せないため）。
//! - **モデル名は振り先が決まってから載せる。** 呼び出し元が渡した未検証の文字列を span 名・
//!   属性に入れない（カタログに無い名前はそのまま外へ送らない）。
//! - 最初のトークンまでの時間は属性 `gen_ai.server.time_to_first_token`（秒）と
//!   イベント `gen_ai.first_token` で残す。
//! - **ストリームを span で包み、ストリームが終端（`None`）に達した時点で span を閉じる。**
//!   途中で drop されたらその時点で閉じる（消費側の後処理の時間を生成時間に混ぜない）。
//! - **本文は載せない。** プロンプト・応答は Langfuse の担当（trace_id で突合）。span は
//!   呼び出し元の span の子として作るので、trace_id は呼び出し元（＝Langfuse の trace id を
//!   採番した文脈）のものを引き継ぐ。

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use futures::Stream;
use opentelemetry::{Array, StringValue, Value};
use tracing::{field::Empty, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::model::{StopReason, StreamDelta};
use crate::provider::{DeltaStream, LlmError};

/// LLM 呼び出し 1 回の span。名前は振り先が決まるまで `chat`（[`record_target`] で
/// `chat <論理モデル名>` にする）。
pub(crate) fn llm_span() -> Span {
    tracing::info_span!(
        "gen_ai.chat",
        "otel.name" = "chat",
        "otel.kind" = "client",
        "gen_ai.operation.name" = "chat",
        "shiki.llm.model" = Empty,
        "gen_ai.request.model" = Empty,
        "gen_ai.provider.name" = Empty,
        "gen_ai.usage.input_tokens" = Empty,
        "gen_ai.usage.output_tokens" = Empty,
        "gen_ai.server.time_to_first_token" = Empty,
        "otel.status_code" = Empty,
        "error.type" = Empty,
    )
}

/// 振り先が決まった時点で（＝カタログにあるモデルと確かめた後で）、論理モデル名・実モデル ID・
/// プロバイダ種別を刻む。
pub(crate) fn record_target(
    span: &Span,
    logical_model: &str,
    resolved_model: &str,
    provider: &str,
) {
    span.record("otel.name", format!("chat {logical_model}"));
    span.record("shiki.llm.model", logical_model);
    span.record("gen_ai.request.model", resolved_model);
    span.record("gen_ai.provider.name", provider);
}

/// 失敗を刻む（文言は載せない。プロバイダの本文が混ざり得る）。
pub(crate) fn record_error(span: &Span, e: &LlmError) {
    span.record("otel.status_code", "ERROR");
    span.record("error.type", error_kind(e));
}

fn error_kind(e: &LlmError) -> &'static str {
    match e {
        LlmError::Config(_) => "config",
        LlmError::Unavailable(_) => "unavailable",
        LlmError::RateLimited(_) => "rate_limited",
        LlmError::BadRequest(_) => "bad_request",
        LlmError::Internal(_) => "internal",
    }
}

const fn stop_reason_str(r: StopReason) -> &'static str {
    match r {
        StopReason::EndTurn => "end_turn",
        StopReason::ToolUse => "tool_use",
        StopReason::MaxTokens => "max_tokens",
        StopReason::Other => "other",
    }
}

/// ストリームを span で包む。span はストリームの終端（`None`）か drop の早い方で閉じる。
pub(crate) fn trace_stream(inner: DeltaStream, span: Span, started: Instant) -> DeltaStream {
    Box::pin(TracedStream {
        inner,
        span,
        started,
        first_token: false,
    })
}

struct TracedStream {
    inner: DeltaStream,
    span: Span,
    started: Instant,
    first_token: bool,
}

impl TracedStream {
    fn observe(&mut self, delta: &StreamDelta) {
        match delta {
            StreamDelta::TextDelta { .. }
            | StreamDelta::ThinkingDelta { .. }
            | StreamDelta::ToolUseStart { .. }
            | StreamDelta::ToolUseInputDelta { .. }
                if !self.first_token =>
            {
                self.first_token = true;
                let ttft = self.started.elapsed().as_secs_f64();
                self.span.record("gen_ai.server.time_to_first_token", ttft);
                tracing::info!(time_to_first_token_s = ttft, "gen_ai.first_token");
            }
            StreamDelta::Done { stop_reason, usage } => {
                self.span
                    .record("gen_ai.usage.input_tokens", usage.prompt_tokens);
                self.span
                    .record("gen_ai.usage.output_tokens", usage.completion_tokens);
                // 規約どおり文字列配列（OTel 層が無いときは何もしない）。
                self.span.set_attribute(
                    "gen_ai.response.finish_reasons",
                    Value::Array(Array::String(vec![StringValue::from(stop_reason_str(
                        *stop_reason,
                    ))])),
                );
            }
            _ => {}
        }
    }
}

impl Stream for TracedStream {
    type Item = Result<StreamDelta, LlmError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let span = this.span.clone();
        let _entered = span.enter();
        let polled = this.inner.as_mut().poll_next(cx);
        match &polled {
            Poll::Ready(Some(Ok(delta))) => this.observe(delta),
            Poll::Ready(Some(Err(e))) => record_error(&this.span, e),
            // 終端で手放して閉じる（この型が持つのが最後の参照）。
            Poll::Ready(None) => this.span = Span::none(),
            Poll::Pending => {}
        }
        polled
    }
}

#[cfg(test)]
#[path = "telemetry/capture.rs"]
mod capture;

#[cfg(test)]
#[path = "telemetry/tests.rs"]
mod tests;
