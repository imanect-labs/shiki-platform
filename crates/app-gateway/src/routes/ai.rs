//! llm.invoke / agent.invoke 能力アダプタ（Task 9.9）。
//!
//! - **llm.invoke**: raw LLM 呼び出し（アプリがプロンプト供給）。`llm_gateway::LlmGateway`
//!   （チョークポイント）へ直委譲し、会計は app_id/user_sub 付きで `llm_usage` に刻む。
//! - **agent.invoke**: agent-core ループ起動。[`crate::AgentPort`]（api 配線実装）へ委譲する。
//!
//! **ガードレール**（インストール時ピン [`crate::AiPin`]・PR9 の同意フローが焼き込む）:
//! ①モデル allowlist ②日次 USD 予算 = min(宣言, 管理者キャップ)・超過 429
//! ③max_tokens キャップ。予算は `llm_usage` の当日集計で事前チェックし、agent は
//! 残額を agent-core Budget にも渡す（ループ中の超過も止まる）。

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    Extension, Json,
};
use llm_gateway::{GenerateRequest, GenerationRecord, LlmGateway, StreamDelta};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream_compat::ReceiverStream;
use uuid::Uuid;

use crate::{
    installation::AiPin,
    ports::{AgentInvokeSpec, AiEvent},
    router::{GatewayCtx, GatewayState},
    GatewayError,
};

/// 実効日次上限 = min(アプリ宣言, 管理者キャップ)。宣言なしは管理者キャップ。
pub(crate) fn effective_daily_limit(pin: &AiPin, admin_cap: i64) -> i64 {
    pin.budget_daily_usd_micros
        .map_or(admin_cap, |d| d.min(admin_cap))
}

/// モデル allowlist（インストール時ピン）。空 allowlist はテナントカタログ全体を許可。
///
/// allowlist が非空のとき、モデル未指定は**拒否**（既定モデルが allowlist 外へ落ちる
/// 事故を防ぐ・fail-closed）。
pub(crate) fn check_model_allowed(pin: &AiPin, model: Option<&str>) -> Result<(), GatewayError> {
    if pin.budget_models.is_empty() {
        return Ok(());
    }
    match model {
        Some(m) if pin.budget_models.iter().any(|a| a == m) => Ok(()),
        Some(m) => Err(GatewayError::Forbidden(format!(
            "モデル {m} はこのアプリに許可されていません"
        ))),
        None => Err(GatewayError::Invalid(
            "このアプリはモデル allowlist が設定されています。model を明示してください".into(),
        )),
    }
}

/// max_tokens のキャップ（要求とピンの min・どちらも無ければプロファイル既定に任せる）。
pub(crate) fn cap_max_tokens(requested: Option<u32>, pin: Option<i64>) -> Option<u32> {
    let pin = pin.and_then(|p| u32::try_from(p).ok());
    match (requested, pin) {
        (Some(r), Some(p)) => Some(r.min(p)),
        (None, Some(p)) => Some(p),
        (r, None) => r,
    }
}

/// 予算事前チェック（残額を返す・超過は 429）。
async fn check_budget(
    llm: &LlmGateway,
    ctx: &GatewayCtx,
    admin_cap: i64,
) -> Result<i64, GatewayError> {
    let limit = effective_daily_limit(&ctx.installation.ai, admin_cap);
    let spent = llm
        .app_spend_today(&ctx.auth, ctx.installation.app_id)
        .await
        .map_err(|e| GatewayError::Internal(format!("llm_usage 集計に失敗: {e}")))?;
    if spent >= limit {
        return Err(GatewayError::RateLimited(format!(
            "本日の AI 予算（{limit} µUSD）を使い切りました"
        )));
    }
    Ok(limit - spent)
}

fn require_llm(state: &GatewayState) -> Result<Arc<LlmGateway>, GatewayError> {
    state.caps.llm.clone().ok_or_else(|| {
        GatewayError::Upstream("LLM ゲートウェイがこの環境では構成されていません".into())
    })
}

/// [`AiEvent`] を SSE イベントへ写す。
fn to_sse(ev: AiEvent) -> Event {
    Event::default().event(ev.event).data(ev.data.to_string())
}

type SseResult = axum::response::Response;

fn sse_from_events(rx: mpsc::Receiver<AiEvent>) -> SseResult {
    use axum::response::IntoResponse;
    use futures::StreamExt;
    let stream = ReceiverStream::new(rx)
        .map(|ev| Ok::<Event, Infallible>(to_sse(ev)))
        .boxed();
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// raw LLM 呼び出し（SSE）。tools はアプリ側実行（deltas をそのまま返す）。
pub(crate) async fn llm_invoke(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Json(mut req): Json<GenerateRequest>,
) -> Result<SseResult, GatewayError> {
    let llm = require_llm(&state)?;
    let pin = &ctx.installation.ai;
    if req.messages.is_empty() {
        return Err(GatewayError::Invalid("messages が空です".into()));
    }
    check_model_allowed(pin, req.model.as_deref())?;
    check_budget(&llm, &ctx, state.caps.ai_daily_cap_usd_micros).await?;
    req.max_tokens = cap_max_tokens(req.max_tokens, pin.budget_max_tokens);

    let logical_model = req
        .model
        .clone()
        .unwrap_or_else(|| llm.default_model().to_string());
    let input_preview: String = req
        .messages
        .last()
        .map(|m| format!("{:?}", m.content).chars().take(200).collect())
        .unwrap_or_default();

    let deltas = llm.stream(req).await.map_err(map_llm_err)?;

    // driver task: クライアント切断でも Done まで消費して会計を落とさない。
    let (tx, rx) = mpsc::channel::<AiEvent>(64);
    let auth = ctx.auth.clone();
    let app_id = ctx.installation.app_id;
    tokio::spawn(async move {
        use futures::StreamExt;
        let mut deltas = deltas;
        let mut text_acc = String::new();
        let mut done_usage = None;
        while let Some(delta) = deltas.next().await {
            match delta {
                Ok(d) => {
                    if let StreamDelta::TextDelta { text } = &d {
                        text_acc.push_str(text);
                    }
                    if let StreamDelta::Done { usage, .. } = &d {
                        done_usage = Some(*usage);
                    }
                    let _ = tx.send(delta_event(&d)).await;
                }
                Err(e) => {
                    let _ = tx
                        .send(AiEvent {
                            event: "error".into(),
                            data: json!({ "error": e.to_string() }),
                        })
                        .await;
                    break;
                }
            }
        }
        if let Some(usage) = done_usage {
            llm.record_generation(
                &auth,
                &GenerationRecord {
                    idempotency_key: format!("gw-llm:{app_id}:{}", Uuid::new_v4()),
                    model: logical_model,
                    usage,
                    trace_id: None,
                    input_preview,
                    output_preview: text_acc.chars().take(2000).collect(),
                    app_id: Some(app_id),
                },
            )
            .await;
        }
    });
    Ok(sse_from_events(rx))
}

/// [`StreamDelta`] → SSE イベント（event=type タグ・data=中立形 JSON）。
fn delta_event(d: &StreamDelta) -> AiEvent {
    let name = match d {
        StreamDelta::TextDelta { .. } => "text",
        StreamDelta::ThinkingDelta { .. } => "thinking",
        StreamDelta::ToolUseStart { .. } => "tool_use_start",
        StreamDelta::ToolUseInputDelta { .. } => "tool_use_input_delta",
        StreamDelta::ToolUseStop { .. } => "tool_use_stop",
        StreamDelta::Done { .. } => "done",
    };
    AiEvent {
        event: name.into(),
        data: serde_json::to_value(d).unwrap_or_else(|_| json!({})),
    }
}

fn map_llm_err(e: llm_gateway::LlmError) -> GatewayError {
    match e {
        llm_gateway::LlmError::Config(m) => GatewayError::Invalid(m),
        other => GatewayError::Upstream(format!("llm: {other}")),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct AgentInvokeRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub max_steps: Option<usize>,
}

/// agent-core ループ起動（SSE）。ツール/RAG は呼出ユーザーの ReBAC（port 実装が保証）。
pub(crate) async fn agent_invoke(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Json(req): Json<AgentInvokeRequest>,
) -> Result<SseResult, GatewayError> {
    if req.prompt.trim().is_empty() {
        return Err(GatewayError::Invalid("prompt が空です".into()));
    }
    let pin = &ctx.installation.ai;
    check_model_allowed(pin, req.model.as_deref())?;
    // 予算は llm-gateway の会計が正（agent の LLM 呼び出しも app_id 付きで同じ財布に落ちる）。
    let remaining = match require_llm(&state) {
        Ok(llm) => check_budget(&llm, &ctx, state.caps.ai_daily_cap_usd_micros).await?,
        // LLM 未構成なら port 側も NoAgent（502）のはず。チェックは素通しにする。
        Err(_) => state.caps.ai_daily_cap_usd_micros,
    };
    let spec = AgentInvokeSpec {
        app_id: ctx.installation.app_id,
        prompt: req.prompt,
        model: req.model,
        declared_tools: pin.agent_tools.clone(),
        max_tokens: pin.budget_max_tokens,
        max_steps: req.max_steps,
        max_cost_usd_micros: remaining,
        trace_id: None,
    };
    let events = state.caps.agent.invoke(&ctx.auth, spec).await?;
    use axum::response::IntoResponse;
    use futures::StreamExt;
    let stream: futures::stream::BoxStream<'static, Result<Event, Infallible>> =
        events.map(|ev| Ok::<Event, Infallible>(to_sse(ev))).boxed();
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// tokio mpsc → Stream の薄いアダプタ（tokio-stream 依存を足さないための最小実装）。
mod tokio_stream_compat {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    pub(super) struct ReceiverStream<T>(tokio::sync::mpsc::Receiver<T>);

    impl<T> ReceiverStream<T> {
        pub(super) fn new(rx: tokio::sync::mpsc::Receiver<T>) -> Self {
            ReceiverStream(rx)
        }
    }

    impl<T> futures::Stream for ReceiverStream<T> {
        type Item = T;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
            self.0.poll_recv(cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(models: &[&str], daily: Option<i64>, max_tokens: Option<i64>) -> AiPin {
        AiPin {
            budget_models: models.iter().map(|s| (*s).to_string()).collect(),
            budget_daily_usd_micros: daily,
            budget_max_tokens: max_tokens,
            agent_tools: vec![],
        }
    }

    #[test]
    fn daily_limit_is_min_of_pin_and_admin_cap() {
        assert_eq!(
            effective_daily_limit(&pin(&[], None, None), 5_000_000),
            5_000_000
        );
        assert_eq!(
            effective_daily_limit(&pin(&[], Some(1_000), None), 5_000_000),
            1_000
        );
        // 宣言が管理者キャップより大きくてもキャップで抑える。
        assert_eq!(
            effective_daily_limit(&pin(&[], Some(9_000_000), None), 5_000_000),
            5_000_000
        );
    }

    #[test]
    fn model_allowlist_is_fail_closed() {
        let p = pin(&["gpt-a"], None, None);
        assert!(check_model_allowed(&p, Some("gpt-a")).is_ok());
        assert!(matches!(
            check_model_allowed(&p, Some("gpt-b")),
            Err(GatewayError::Forbidden(_))
        ));
        // allowlist 非空でモデル未指定は 400（既定モデルへのすり抜けを防ぐ）。
        assert!(matches!(
            check_model_allowed(&p, None),
            Err(GatewayError::Invalid(_))
        ));
        // allowlist 空は何でも通す（テナントカタログが最終判定）。
        assert!(check_model_allowed(&pin(&[], None, None), None).is_ok());
    }

    #[test]
    fn max_tokens_caps_to_pin() {
        assert_eq!(cap_max_tokens(Some(4096), Some(1024)), Some(1024));
        assert_eq!(cap_max_tokens(Some(512), Some(1024)), Some(512));
        assert_eq!(cap_max_tokens(None, Some(1024)), Some(1024));
        assert_eq!(cap_max_tokens(Some(512), None), Some(512));
        assert_eq!(cap_max_tokens(None, None), None);
    }
}
