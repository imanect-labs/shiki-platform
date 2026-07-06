//! `LlmGateway` — in-process の単一チョークポイント（ホップ0・別プロセス化しない）。
//!
//! shiki 固有責務をここに編み込む: **トークン会計（tenant_id+org・冪等キー）**・**Langfuse 計装**
//! （trace_id 起点）・フォールバック/リトライ/タイムアウト・プロバイダ差し替え。生成本体
//! （ストリーミング）はプロバイダアダプタへ委譲し、会計/計装は生成後に明示的に記録する
//! （agent ループが複数回呼ぶため attempt/呼び出し単位で刻む・design §4.5）。

use authz::AuthContext;
use sqlx::PgPool;

use crate::accounting::{UsageRecord, UsageRecorder};
use crate::config::{GatewayConfig, ProviderKind};
use crate::langfuse::{GenerationTrace, LangfuseClient};
use crate::model::{GenerateRequest, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};
use crate::providers::{anthropic::AnthropicProvider, openai::OpenAiProvider, stub::StubProvider};
use std::sync::Arc;

/// 生成 1 回分の会計/計装レコード（ストリーム消費後に gateway へ渡す）。
pub struct GenerationRecord {
    /// 冪等キー（`<run_id>:<attempt>:<call_ordinal>`）。
    pub idempotency_key: String,
    /// 論理モデル名（カタログ引きの鍵）。
    pub model: String,
    pub usage: Usage,
    pub trace_id: Option<String>,
    /// Langfuse 表示用の入力/出力プレビュー（長文は呼び出し側で切る）。
    pub input_preview: String,
    pub output_preview: String,
}

/// LLM ゲートウェイ（チョークポイント）。
#[derive(Clone)]
pub struct LlmGateway {
    inner: Arc<Inner>,
}

struct Inner {
    provider: Arc<dyn LlmProvider>,
    config: GatewayConfig,
    accounting: UsageRecorder,
    langfuse: Option<LangfuseClient>,
}

impl LlmGateway {
    /// 設定からゲートウェイを構築する（プロバイダを kind で選択）。
    pub fn build(
        db: PgPool,
        http: reqwest::Client,
        config: GatewayConfig,
    ) -> Result<Self, LlmError> {
        let langfuse = config
            .langfuse
            .as_ref()
            .map(|c| LangfuseClient::new(http.clone(), c));
        let provider: Arc<dyn LlmProvider> = build_provider(http, &config)?;
        Ok(LlmGateway {
            inner: Arc::new(Inner {
                provider,
                accounting: UsageRecorder::new(db),
                langfuse,
                config,
            }),
        })
    }

    /// 既定モデル（論理名）。
    pub fn default_model(&self) -> &str {
        &self.inner.config.catalog.default_model
    }

    /// リクエストを**ストリーミング**生成する。論理モデル→実 ID をカタログで解決してから
    /// プロバイダへ委譲する。接続確立前の一時障害は 1 回だけ再試行する（フォールバックの床）。
    pub async fn stream(&self, mut req: GenerateRequest) -> Result<DeltaStream, LlmError> {
        let logical = req
            .model
            .clone()
            .unwrap_or_else(|| self.inner.config.catalog.default_model.clone());
        let entry = self
            .inner
            .config
            .catalog
            .get(&logical)
            .ok_or_else(|| LlmError::Config(format!("unknown model: {logical}")))?;
        req.model = Some(entry.resolved_id().to_string());

        // 接続確立前（最初のバイト前）の Unavailable のみ再試行する。
        match self.inner.provider.stream(&req).await {
            Ok(s) => Ok(s),
            Err(LlmError::Unavailable(first)) => {
                tracing::warn!(error = %first, "llm provider unavailable, retrying once");
                self.inner.provider.stream(&req).await
            }
            Err(e) => Err(e),
        }
    }

    /// 生成 1 回分の会計＋Langfuse 計装を記録する（ストリーム消費後に呼ぶ）。
    ///
    /// 会計は同期（金額クリティカル・冪等）、Langfuse は best-effort（非同期・失敗は warn）。
    pub async fn record_generation(&self, ctx: &AuthContext, rec: &GenerationRecord) {
        let entry = self
            .inner
            .config
            .catalog
            .get(&rec.model)
            .or_else(|| self.inner.config.catalog.default_entry());
        let cost = entry.map_or(0, |e| {
            e.cost_usd_micros(rec.usage.prompt_tokens, rec.usage.completion_tokens)
        });
        let provider = self.inner.provider.name().to_string();

        let usage_rec = UsageRecord {
            idempotency_key: rec.idempotency_key.clone(),
            provider: provider.clone(),
            model: rec.model.clone(),
            usage: rec.usage,
            cost_usd_micros: cost,
            trace_id: rec.trace_id.clone(),
        };
        if let Err(e) = self.inner.accounting.record(ctx, &usage_rec).await {
            tracing::error!(error = %e, "llm usage accounting failed");
        }

        if let (Some(lf), Some(trace_id)) = (&self.inner.langfuse, rec.trace_id.as_deref()) {
            lf.record_generation(GenerationTrace {
                trace_id,
                name: "chat.generation",
                model: &rec.model,
                provider: &provider,
                input: &rec.input_preview,
                output: &rec.output_preview,
                usage: rec.usage,
                metadata: serde_json::json!({
                    "tenant_id": ctx.tenant_id,
                    "org": ctx.org,
                    "user": ctx.principal.id,
                    "cost_usd_micros": cost,
                }),
            })
            .await;
        }
    }
}

fn build_provider(
    http: reqwest::Client,
    config: &GatewayConfig,
) -> Result<Arc<dyn LlmProvider>, LlmError> {
    let pc = &config.provider;
    // タイムアウトはプロバイダ側の per-request `.timeout()` で制御するため、
    // 共有 HTTP クライアント（接続プール共有）をそのまま各アダプタへ渡す。
    let default_real = config.catalog.default_entry().map_or_else(
        || config.catalog.default_model.clone(),
        |e| e.resolved_id().to_string(),
    );
    match pc.kind {
        ProviderKind::Stub => Ok(Arc::new(StubProvider::new())),
        ProviderKind::Openai => {
            let base = pc
                .base_url
                .clone()
                .ok_or_else(|| LlmError::Config("openai base_url required".into()))?;
            Ok(Arc::new(OpenAiProvider::new(
                http,
                base,
                pc.api_key.clone(),
                default_real,
            )))
        }
        ProviderKind::Anthropic => {
            let base = pc
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.anthropic.com".to_string());
            let key = pc
                .api_key
                .clone()
                .ok_or_else(|| LlmError::Config("anthropic api_key required".into()))?;
            Ok(Arc::new(AnthropicProvider::new(
                http,
                base,
                key,
                default_real,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelCatalog, ModelEntry, ProviderConfig};

    /// 指定 kind ＋ base_url ＋ api_key から最小構成の設定を組む。
    fn config(kind: ProviderKind, base_url: Option<&str>, api_key: Option<&str>) -> GatewayConfig {
        GatewayConfig {
            provider: ProviderConfig {
                kind,
                base_url: base_url.map(str::to_string),
                api_key: api_key.map(str::to_string),
                timeout_secs: 120,
            },
            catalog: ModelCatalog {
                default_model: "m".into(),
                models: vec![ModelEntry {
                    id: "m".into(),
                    real_id: Some("real-m".into()),
                    prompt_price_micros_per_mtok: 0,
                    completion_price_micros_per_mtok: 0,
                }],
            },
            langfuse: None,
        }
    }

    #[test]
    fn stub_provider_needs_no_endpoint() {
        let cfg = config(ProviderKind::Stub, None, None);
        let p = build_provider(reqwest::Client::new(), &cfg).unwrap();
        assert_eq!(p.name(), "stub");
    }

    #[test]
    fn openai_requires_base_url() {
        let ok = config(ProviderKind::Openai, Some("http://vllm:8000/v1"), None);
        assert_eq!(
            build_provider(reqwest::Client::new(), &ok).unwrap().name(),
            "openai"
        );

        let missing = config(ProviderKind::Openai, None, None);
        let res = build_provider(reqwest::Client::new(), &missing);
        assert!(matches!(res, Err(LlmError::Config(_))));
    }

    #[test]
    fn anthropic_requires_api_key_and_defaults_base_url() {
        // base_url 省略時は既定エンドポイントへフォールバックし、api_key があれば構築できる。
        let ok = config(ProviderKind::Anthropic, None, Some("k"));
        assert_eq!(
            build_provider(reqwest::Client::new(), &ok).unwrap().name(),
            "anthropic"
        );

        let missing = config(ProviderKind::Anthropic, None, None);
        let res = build_provider(reqwest::Client::new(), &missing);
        assert!(matches!(res, Err(LlmError::Config(_))));
    }
}
