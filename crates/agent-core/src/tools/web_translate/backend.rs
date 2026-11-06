//! 生成の口（本番は llm-gateway・単体テストは台本）。
//!
//! ツールの内側だけの差し替え点で、プロバイダ差の吸収（`LlmProvider`）とは別物。
//! 本番の実装はチョークポイント（[`LlmGateway::stream`]）を通し、会計と計装を必ず記録する。

use authz::AuthContext;
use futures::StreamExt as _;
use llm_gateway::{
    GenerateRequest, GenerationRecord, LlmError, LlmGateway, StopReason, StreamDelta, Usage,
    UsageSource,
};

/// 生成 1 回の結果（失敗しても消費は返す）。
pub(super) struct Reply {
    pub text: String,
    pub stop: StopReason,
    /// 課金の累計・新規ぶん・コスト（マイクロ USD）。
    pub spent: (u64, u64, i64),
    /// 途中で失敗したならその理由（`text` はそこまで）。
    pub error: Option<LlmError>,
}

#[async_trait::async_trait]
pub(super) trait Backend: Send + Sync {
    /// 1 回生成する。接続前の失敗は `Err`、流れ始めてからの失敗は `Reply::error`。
    async fn generate(
        &self,
        ctx: &AuthContext,
        req: GenerateRequest,
        key: String,
        trace_id: Option<&str>,
    ) -> Result<Reply, LlmError>;

    /// モデルのコンテキスト長（トークン・分からなければ `None`）。
    fn context_window(&self, model: Option<&str>) -> Option<u32>;
}

/// llm-gateway 経由の生成（予算・使用量記録に乗る）。
pub(super) struct GatewayBackend(pub LlmGateway);

#[async_trait::async_trait]
impl Backend for GatewayBackend {
    async fn generate(
        &self,
        ctx: &AuthContext,
        req: GenerateRequest,
        key: String,
        trace_id: Option<&str>,
    ) -> Result<Reply, LlmError> {
        let gateway = &self.0;
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| gateway.default_model().to_string());
        let started = chrono::Utc::now();
        let mut stream = gateway.stream(ctx, req).await?;
        let (mut text, mut usage, mut stop) =
            (String::new(), Usage::default(), StopReason::EndTurn);
        let mut error = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamDelta::TextDelta { text: t }) => text.push_str(&t),
                Ok(StreamDelta::Done {
                    stop_reason,
                    usage: u,
                }) => {
                    stop = stop_reason;
                    usage = u;
                }
                Ok(_) => {}
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        // 失敗しても、そこまでの消費は記録して親の予算へ積む（無料で作り直せないように）。
        gateway
            .record_generation(
                ctx,
                &GenerationRecord {
                    idempotency_key: key,
                    model: model.clone(),
                    usage,
                    trace_id: trace_id.map(str::to_string),
                    // ページ本文は計装に載せない（外部ページの全文を Langfuse に複製しない）。
                    input_preview: "web_translate".into(),
                    output_preview: String::new(),
                    app_id: None,
                    source: UsageSource::Agent,
                    first_in_trace: false,
                    started_at: Some(started),
                },
            )
            .await;
        Ok(Reply {
            text,
            stop,
            spent: (
                usage.prompt_tokens + usage.completion_tokens,
                usage.uncached_prompt_tokens() + usage.completion_tokens,
                gateway.estimate_cost_usd_micros(&model, usage),
            ),
            error,
        })
    }

    fn context_window(&self, model: Option<&str>) -> Option<u32> {
        self.0.context_window_tokens(model)
    }
}
