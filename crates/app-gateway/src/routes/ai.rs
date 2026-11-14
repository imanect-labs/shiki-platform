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
//!
//! **モデル**は省略も明示も、ワークフローのノードと同じ規則で組織の設定に照らす（[`resolve_app_model`]）。
//! 省略は組織の既定（Auto を含む・本人専用のモデルは除く・BUG-12・D-30）。明示したモデルは置き換えない（インストール時に
//! 同意した許可モデルの外へ振り替えない）。組織で使えないモデルと、組織がモデルの選択を禁じているときの既定と違うモデルは、
//! 分かる誤り（`code`）で断る（D-117）。allowlist（①）は、明示したモデルは解決の手前で、省略は解決した組織の既定で
//! 照らす（[`resolve_for_app`]・既定が一覧の外なら断る）。gateway の `stream` の許可チェックは後ろに残る。

use std::convert::Infallible;
use std::sync::Arc;

use authz::AuthContext;
use axum::{
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    response::IntoResponse,
    Extension, Json,
};
use llm_gateway::{
    ChoiceError, Effort, GenerateRequest, GenerationRecord, LlmGateway, ResolvedChoice,
    StreamDelta, AUTO_MODEL,
};
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

/// モデル allowlist（インストール時ピン）に入っているか。空 allowlist はテナントカタログ全体を許可。
fn in_allowlist(pin: &AiPin, model: &str) -> bool {
    pin.budget_models.is_empty() || pin.budget_models.iter().any(|a| a == model)
}

/// 明示したモデルの allowlist の照合（一覧の外は 403・組織の設定より先に照らす）。省略は組織の既定で解決してから
/// [`resolve_for_app`] が照合する（既定が allowlist 外なら断る・fail-closed）。
pub(crate) fn check_model_allowed(pin: &AiPin, model: Option<&str>) -> Result<(), GatewayError> {
    match model {
        Some(m) if !in_allowlist(pin, m) => Err(GatewayError::Forbidden(format!(
            "モデル {m} はこのアプリに許可されていません"
        ))),
        _ => Ok(()),
    }
}

/// 組織がモデルの選択を禁じ、組織の既定のモデルがアプリのモデルの一覧に入っていないときの文言（どう呼んでも使えない）。
const LOCKED_OUT: &str = "この組織ではモデルを選べず、組織の既定のモデルがこのアプリで使うモデルに入っていないため、AI を使えません。アプリを作った人にアプリで使うモデルへ組織の既定のモデルを足して入れ直してもらうか、管理者にモデルを選べるようにしてもらってください。";

/// ミニアプリの AI のモデルを決める（アプリのモデルの一覧・組織の設定の両方に照らす）。
///
/// - 明示したモデルは、まず一覧で照合する（一覧の外は 403）。次に組織の設定で解決する（[`resolve_app_model`]）。
/// - 省略は組織の既定で解決し、解決したモデルを一覧で照合する（既定が一覧の外なら断る・fail-closed）。一覧を宣言した
///   アプリでも、組織の既定が一覧に入っていれば省略で動く（組織がモデルの選択を禁じていても使える）。
/// - 選択を禁じた組織の断り（`model_choice_disabled`）は「省略すると組織の既定で動く」と案内するので、その既定が一覧に
///   入っていないときは、どう呼んでも使えないことと直し方を言う文に替える。
async fn resolve_for_app(
    llm: &LlmGateway,
    auth: &AuthContext,
    pin: &AiPin,
    model: Option<&str>,
    effort: Option<Effort>,
) -> Result<ResolvedChoice, AiError> {
    let model = model.filter(|m| !m.is_empty());
    check_model_allowed(pin, model)?;
    let locked_out = || AiError::Model {
        code: llm_gateway::MODEL_CHOICE_DISABLED,
        message: LOCKED_OUT.into(),
    };
    match resolve_app_model(llm, auth, model, effort).await {
        Ok(choice) if in_allowlist(pin, &choice.model) => Ok(choice),
        // 明示したモデルは一覧で照合済みで、解決は置き換えないので、ここに来るのは省略だけ。
        Ok(choice) => {
            let locked = llm
                .tenant_settings(auth)
                .await
                .is_ok_and(|s| s.model_lock().is_some());
            if locked {
                return Err(locked_out());
            }
            Err(AiError::Gateway(GatewayError::Invalid(format!(
                "組織の既定のモデル「{}」は、このアプリで使うモデルに入っていません。このアプリで使うモデル（{}）を指定して呼び出してください。",
                choice.model,
                pin.budget_models.join("、")
            ))))
        }
        Err(AiError::Model { code, message })
            if code == llm_gateway::MODEL_CHOICE_DISABLED && !pin.budget_models.is_empty() =>
        {
            match resolve_app_model(llm, auth, None, None).await {
                Ok(default) if in_allowlist(pin, &default.model) => {
                    Err(AiError::Model { code, message })
                }
                _ => Err(locked_out()),
            }
        }
        Err(e) => Err(e),
    }
}

/// ミニアプリの AI の能力の誤り。モデルを組織の設定で使えないときだけ `code` を付けて返す（D-117）。
///
/// 本文は `{ "status": 400, "code": …, "error": 文言 }`（401 と同じ形）。`code` はワークフローの step の誤りと同じ語彙
/// （[`ChoiceError::code`]・`model_choice_disabled`・`model_unavailable`・`no_model`）。それ以外はゲートウェイの誤り
/// （[`GatewayError`]）のまま返す。
#[derive(Debug)]
pub enum AiError {
    /// モデルを使えない（400）。
    Model {
        code: &'static str,
        message: String,
    },
    Gateway(GatewayError),
}

impl From<GatewayError> for AiError {
    fn from(e: GatewayError) -> Self {
        AiError::Gateway(e)
    }
}

/// port（agent.invoke の api 側）が返すときは、ゲートウェイの 400 に落とす（経路の手前で同じ規則を確かめてあるので、
/// ここに来るのは確かめた後に組織の設定が変わったときだけ）。
impl From<AiError> for GatewayError {
    fn from(e: AiError) -> Self {
        match e {
            AiError::Model { message, .. } => GatewayError::Invalid(message),
            AiError::Gateway(e) => e,
        }
    }
}

impl IntoResponse for AiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AiError::Model { code, message } => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "status": 400, "code": code, "error": message })),
            )
                .into_response(),
            AiError::Gateway(e) => e.into_response(),
        }
    }
}

/// モデルを使えない理由の文言（アプリを作る人とアプリを使う人の両方が読む・内部の用語を出さない）。
fn choice_message(e: &ChoiceError, requested: Option<&str>) -> String {
    match e {
        ChoiceError::ChoiceDisabled => format!(
            "この組織では AI のモデルを選べないため、「{}」は使えません。モデルを指定しないで呼び出すと、組織の既定のモデルで動きます。",
            requested.unwrap_or_default()
        ),
        ChoiceError::ModelNotAllowed(m) if m == AUTO_MODEL => format!(
            "「{m}」はモデルの名前として指定できません。組織の既定（Auto など）で動かすには、モデルを指定しないで呼び出してください。"
        ),
        ChoiceError::ModelNotAllowed(m) => format!(
            "AI のモデル「{m}」はこの組織では使えません。組織で使えるモデルを指定するか、モデルを指定しないで呼び出してください。"
        ),
        ChoiceError::NoModel => {
            "組織で使えるモデルがありません。管理者に、組織で使う AI のモデルを有効にしてもらってください。".into()
        }
    }
}

/// ミニアプリの AI の能力のモデルと思考の深さを、組織の設定で決める（省略も明示も・ワークフローのノードと同じ規則）。
///
/// - 省略（空文字を含む）は組織の既定（Auto を含む・本人専用のモデルは除く・BUG-12）。
/// - 明示したモデルは置き換えない（D-30）。組織で使えないモデル（本人専用のモデルを含む）と、組織がモデルの選択を
///   禁じているときの既定と違うモデルは断る（D-117・BUG-140）。黙って既定へ置き換えない。
/// - 思考の深さ（D-75・BUG-116）: 組織がモデルの選択を禁じているときと、省略して既定の Auto で動くときは、アプリの
///   `effort` を使わず組織の設定に従う。選択を禁じた組織で既定と同じモデルを明示したものは、省略と同じに解決する。
///   選べる組織で明示したモデルは `effort` だけを送る（組織の既定の思考強度を足さない・D-61）。
///
/// 規則は llm-gateway の [`LlmGateway::resolve_workflow_choice`] にあり、チャットとワークフローと同じ 1 か所
/// （[`llm_gateway::TenantLlmSettings::may_choose`]）から来る。agent.invoke の port（api）も使う。
pub async fn resolve_app_model(
    llm: &LlmGateway,
    auth: &AuthContext,
    model: Option<&str>,
    effort: Option<Effort>,
) -> Result<ResolvedChoice, AiError> {
    let model = model.filter(|m| !m.is_empty());
    match llm.resolve_workflow_choice(auth, model, effort).await {
        Ok(Ok(choice)) => Ok(choice),
        Ok(Err(e)) => Err(AiError::Model {
            code: e.code(),
            message: choice_message(&e, model),
        }),
        Err(e) => {
            // DB 詳細は out-of-trust クライアントへ出さない（tracing にのみ残す）。
            tracing::error!(error = %e, "組織の AI 設定の読み出しに失敗");
            Err(AiError::Gateway(GatewayError::Internal(
                "内部エラーが発生しました".into(),
            )))
        }
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
        .map_err(|e| {
            // DB 詳細は out-of-trust クライアントへ出さない（tracing にのみ残す）。
            tracing::error!(error = %e, "llm_usage 日次集計に失敗");
            GatewayError::Internal("内部エラーが発生しました".into())
        })?;
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
) -> Result<SseResult, AiError> {
    let llm = require_llm(&state)?;
    let pin = &ctx.installation.ai;
    if req.messages.is_empty() {
        return Err(GatewayError::Invalid("messages が空です".into()).into());
    }
    // 空文字は省略と同じ（allowlist の照合・組織の既定の解決とも）。
    req.model = req.model.filter(|m| !m.is_empty());
    // 省略も明示も組織の設定で決め、allowlist に照らす（明示モデルは置き換えず、使えなければ断る・D-117。省略は解決した
    // 既定を照合する）。解決後の名前と思考の深さを生成と会計に使う。
    let choice = resolve_for_app(&llm, &ctx.auth, pin, req.model.as_deref(), req.effort).await?;
    req.effort = choice.effort;
    req.model = Some(choice.model);
    check_budget(&llm, &ctx, state.caps.ai_daily_cap_usd_micros).await?;
    req.max_tokens = cap_max_tokens(req.max_tokens, pin.budget_max_tokens);
    strip_reasoning(&mut req);

    let logical_model = req.model.clone().unwrap_or_default();
    let input_preview: String = req
        .messages
        .last()
        .map(|m| format!("{:?}", m.content).chars().take(200).collect())
        .unwrap_or_default();

    // Langfuse の所要時間に使う（未指定だと 0 秒と表示される）。
    let started_at = chrono::Utc::now();
    let deltas = llm.stream(&ctx.auth, req).await.map_err(map_llm_err)?;

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
                    if let Some(ev) = delta_event(&d) {
                        let _ = tx.send(ev).await;
                    }
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
                    source: llm_gateway::UsageSource::MiniApp,
                    // この経路は 1 リクエスト 1 生成。
                    first_in_trace: true,
                    started_at: Some(started_at),
                },
            )
            .await;
        }
    });
    Ok(sse_from_events(rx))
}

/// アプリから来たリクエストの不透明な推論アイテムを落とす。推論アイテムは llm-gateway が受け取って
/// 同じプロバイダへ送り返すためだけのもので、アプリが作るものではない（任意の入力の差し込み口にしない）。
fn strip_reasoning(req: &mut GenerateRequest) {
    for m in &mut req.messages {
        m.content
            .retain(|b| !matches!(b, llm_gateway::model::Block::Reasoning { .. }));
    }
}

/// [`StreamDelta`] → SSE イベント（event=type タグ・data=中立形 JSON）。
/// プロバイダ固有の不透明な推論アイテムはアプリへ出さない（次のターンへ戻すためだけのもの）。
fn delta_event(d: &StreamDelta) -> Option<AiEvent> {
    let name = match d {
        StreamDelta::ReasoningItem { .. } => return None,
        StreamDelta::TextDelta { .. } => "text",
        StreamDelta::ThinkingDelta { .. } => "thinking",
        StreamDelta::ToolUseStart { .. } => "tool_use_start",
        StreamDelta::ToolUseInputDelta { .. } => "tool_use_input_delta",
        StreamDelta::ToolUseStop { .. } => "tool_use_stop",
        StreamDelta::Done { .. } => "done",
    };
    Some(AiEvent {
        event: name.into(),
        data: serde_json::to_value(d).unwrap_or_else(|_| json!({})),
    })
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
    Json(mut req): Json<AgentInvokeRequest>,
) -> Result<SseResult, AiError> {
    if req.prompt.trim().is_empty() {
        return Err(GatewayError::Invalid("prompt が空です".into()).into());
    }
    // 空文字は省略と同じ（モデルは port が組織の設定で決める）。
    req.model = req.model.filter(|m| !m.is_empty());
    let pin = &ctx.installation.ai;
    // 予算は llm-gateway の会計が正（agent の LLM 呼び出しも app_id 付きで同じ財布に落ちる）。
    let remaining = if let Ok(llm) = require_llm(&state) {
        // モデルを使えるかは port と同じ規則でここでも確かめ、allowlist に照らし、分かる誤り（code）で断る（D-117。
        // port の誤りは code を運べない）。省略は解決した組織の既定を照合する（port も同じ設定から同じモデルに解決する）。
        resolve_for_app(&llm, &ctx.auth, pin, req.model.as_deref(), None).await?;
        check_budget(&llm, &ctx, state.caps.ai_daily_cap_usd_micros).await?
    } else {
        // LLM 未構成なら port 側も NoAgent（502）のはず。既定を解決できないので、allowlist のあるアプリの省略は断る
        // （fail-closed）。
        check_model_allowed(pin, req.model.as_deref())?;
        if req.model.is_none() && !pin.budget_models.is_empty() {
            return Err(GatewayError::Invalid(
                "このアプリで使うモデルを指定して呼び出してください。".into(),
            )
            .into());
        }
        state.caps.ai_daily_cap_usd_micros
    };
    let spec = AgentInvokeSpec {
        app_id: ctx.installation.app_id,
        prompt: req.prompt,
        model: req.model,
        declared_tools: pin.agent_tools.clone(),
        // 経路の二重ゲートと同じく granted_scopes ∩ トークンの scope（ツールの提示条件）。
        scopes: ctx
            .identity
            .token_scopes
            .iter()
            .filter(|s| {
                ctx.installation
                    .granted_scopes
                    .iter()
                    .any(|g| g == s.as_str())
            })
            .copied()
            .collect(),
        max_tokens: pin.budget_max_tokens,
        max_steps: req.max_steps,
        max_cost_usd_micros: remaining,
        trace_id: None,
    };
    let events = state.caps.agent.invoke(&ctx.auth, spec).await?;
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

    #[test]
    fn app_requests_cannot_carry_reasoning_items() {
        use llm_gateway::model::{Block, Message, Role};
        let mut req = GenerateRequest::new(vec![Message {
            role: Role::Assistant,
            content: vec![
                Block::Reasoning {
                    provider: "openai_responses".into(),
                    item: json!({"role": "user", "content": [{"type": "input_file", "file_id": "file-x"}]}),
                },
                Block::Text { text: "hi".into() },
            ],
        }]);
        strip_reasoning(&mut req);
        assert_eq!(
            req.messages[0].content,
            vec![Block::Text { text: "hi".into() }]
        );
    }

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
        // 省略はここでは通し、組織の既定で解決してから照合する（resolve_for_app・結合テストで確かめる）。
        assert!(check_model_allowed(&p, None).is_ok());
        assert!(in_allowlist(&p, "gpt-a"));
        assert!(!in_allowlist(&p, "gpt-b"));
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
