//! `LlmGateway` — in-process の単一チョークポイント（ホップ0・別プロセス化しない）。
//!
//! shiki 固有責務をここに編み込む: **トークン会計（tenant_id+org・冪等キー）**・**Langfuse 計装**
//! （trace_id 起点）・フォールバック/リトライ/タイムアウト・プロバイダ差し替え。生成本体
//! （ストリーミング）はプロバイダアダプタへ委譲し、会計/計装は生成後に明示的に記録する
//! （agent ループが複数回呼ぶため attempt/呼び出し単位で刻む・design §4.5）。

use authz::AuthContext;
use sqlx::PgPool;

use std::collections::HashMap;

use crate::accounting::{UsageRecord, UsageRecorder};
use crate::config::{
    AutoConfig, GatewayConfig, LlmTimeouts, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
    DEFAULT_PROVIDER_ID,
};
use crate::langfuse::{GenerationTrace, LangfuseClient};
use crate::limits::{LimitError, UsageLimiter};
use crate::model::{Effort, GenerateRequest, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};
use crate::providers::{
    anthropic::AnthropicProvider, openai::OpenAiProvider,
    openai_responses::OpenAiResponsesProvider, stub::StubProvider,
};
use crate::tenant::{
    self, apply_patch, resolve_choice, ChoiceError, ModelChoice, ResolvedChoice, SettingsError,
    SettingsPatch, TenantLlmSettings, TenantLlmStore,
};
use crate::usage_report::{UsageFilter, UsageReport, UsageReporter};
use std::sync::Arc;

/// 会計の「利用元」（管理画面の利用元別集計・`llm_usage.source`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageSource {
    /// 通常チャット（1 往復の生成）。
    Chat,
    /// エージェント（ツールループ・自律・サブエージェント）。
    Agent,
    /// ワークフローの `llm.invoke`。
    Workflow,
    /// ミニアプリのゲートウェイ AI 能力。
    MiniApp,
    /// 上記以外（後方互換の既定）。
    #[default]
    Other,
}

impl UsageSource {
    /// DB に刻む文字列（`llm_usage.source` の CHECK と一致させる）。
    pub const fn as_str(self) -> &'static str {
        match self {
            UsageSource::Chat => "chat",
            UsageSource::Agent => "agent",
            UsageSource::Workflow => "workflow",
            UsageSource::MiniApp => "miniapp",
            UsageSource::Other => "other",
        }
    }
}

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
    /// 呼び出し元ミニアプリ（ゲートウェイ AI 能力・Task 9.9）。chat 等は `None`。
    pub app_id: Option<uuid::Uuid>,
    /// 利用元（管理画面の利用元別集計）。
    pub source: UsageSource,
    /// **この trace で最初の呼び出しか。** Langfuse の trace は upsert なので、
    /// input を毎回送るとユーザーの質問が最後の呼び出しの入力で上書きされる。
    /// 単発の呼び出しは true。
    pub first_in_trace: bool,
    /// 呼び出しの開始時刻。None だと Langfuse の所要時間が 0 と表示される。
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 管理画面に出すプロバイダ 1 件（秘密は含めない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub kind: ProviderKind,
    /// 接続先（ホスト部まで）。API キーやパスのクエリは載せない。
    pub endpoint: Option<String>,
    /// API キーが設定されているか（値そのものは出さない）。
    pub has_api_key: bool,
}

/// LLM ゲートウェイ（チョークポイント）。
#[derive(Clone)]
pub struct LlmGateway {
    inner: Arc<Inner>,
}

struct ProviderSlot {
    provider: Arc<dyn LlmProvider>,
    config: ProviderConfig,
}

struct Inner {
    /// プロバイダ ID → アダプタ。モデルの `provider` で振り分ける。
    providers: HashMap<String, ProviderSlot>,
    config: GatewayConfig,
    accounting: UsageRecorder,
    tenants: TenantLlmStore,
    usage: UsageReporter,
    limits: UsageLimiter,
    langfuse: Option<LangfuseClient>,
}

impl LlmGateway {
    /// 設定からゲートウェイを構築する（プロバイダを kind で選択）。
    ///
    /// カタログのモデルが**存在しないプロバイダ**を指していたら構築を失敗させる
    /// （実行時に初めて「振り先が無い」と分かる構成を起動させない）。
    // 呼び出し側（各配線・多数のテスト）は Client を値で渡す。プロバイダごとに clone して
    // 接続プールを共有するため、ここで消費はしない。
    #[allow(clippy::needless_pass_by_value)]
    pub fn build(
        db: PgPool,
        http: reqwest::Client,
        config: GatewayConfig,
    ) -> Result<Self, LlmError> {
        let langfuse = config
            .langfuse
            .as_ref()
            .map(|c| LangfuseClient::new(http.clone(), c));
        let mut providers: HashMap<String, ProviderSlot> = HashMap::new();
        for pc in std::iter::once(&config.provider).chain(config.extra_providers.iter()) {
            let id = pc.effective_id().to_string();
            if providers.contains_key(&id) {
                return Err(LlmError::Config(format!("duplicate llm provider id: {id}")));
            }
            let provider = build_provider(http.clone(), pc, &config.catalog, config.timeouts)?;
            providers.insert(
                id,
                ProviderSlot {
                    provider,
                    config: pc.clone(),
                },
            );
        }
        if let Some(m) = config
            .catalog
            .models
            .iter()
            .find(|m| !providers.contains_key(m.provider_id()))
        {
            return Err(LlmError::Config(format!(
                "model {} refers to unknown provider {}",
                m.id,
                m.provider_id()
            )));
        }
        Ok(LlmGateway {
            inner: Arc::new(Inner {
                providers,
                accounting: UsageRecorder::new(db.clone()),
                tenants: TenantLlmStore::new(db.clone()),
                usage: UsageReporter::new(db.clone()),
                limits: UsageLimiter::new(db),
                langfuse,
                config,
            }),
        })
    }

    /// 既定モデル（論理名）。
    pub fn default_model(&self) -> &str {
        &self.inner.config.catalog.default_model
    }

    /// モデルカタログ（プラットフォーム全体・テナントの絞り込み前）。
    pub fn catalog(&self) -> &ModelCatalog {
        &self.inner.config.catalog
    }

    /// Auto の振り先（設定値・カタログ外なら既定モデルへ落とす前の値）。
    pub fn auto_config(&self) -> &AutoConfig {
        &self.inner.config.auto
    }

    /// 接続済みプロバイダの一覧（管理画面用・秘密を含まない）。設定順で返す。
    pub fn providers(&self) -> Vec<ProviderInfo> {
        std::iter::once(&self.inner.config.provider)
            .chain(self.inner.config.extra_providers.iter())
            .map(|pc| ProviderInfo {
                id: pc.effective_id().to_string(),
                label: pc.label().to_string(),
                kind: pc.kind,
                endpoint: pc.base_url.as_deref().map(endpoint_label),
                has_api_key: pc.api_key.as_deref().is_some_and(|k| !k.is_empty()),
            })
            .collect()
    }

    /// モデルが思考強度を受け付けるか（カタログの明示 → プロバイダ種別の既定）。
    #[must_use]
    pub fn supports_effort(&self, entry: &ModelEntry) -> bool {
        entry.supports_effort.unwrap_or_else(|| {
            self.inner
                .providers
                .get(entry.provider_id())
                .is_some_and(|s| s.config.kind.effort_by_default())
        })
    }

    /// テナントの AI 設定を読む（行が無ければ既定＝全モデル有効・Auto）。
    pub async fn tenant_settings(
        &self,
        ctx: &AuthContext,
    ) -> Result<TenantLlmSettings, sqlx::Error> {
        self.inner.tenants.load(ctx).await
    }

    /// 組織の AI 設定を更新する（読み・検証・保存を 1 トランザクションで直列化し、保存後の設定を返す）。
    ///
    /// 呼び出し側（管理 API）が管理者であることを確認してから呼ぶこと（ここは認可しない）。
    pub async fn update_tenant_settings(
        &self,
        ctx: &AuthContext,
        patch: &SettingsPatch,
    ) -> Result<Result<TenantLlmSettings, SettingsError>, sqlx::Error> {
        let catalog = &self.inner.config.catalog;
        self.inner
            .tenants
            .update(ctx, |current| apply_patch(catalog, current, patch))
            .await
    }

    /// チャットのモデル指定を組織の設定で解決する（許可外は `Err`・API は 400 に写す）。
    pub async fn resolve_choice(
        &self,
        ctx: &AuthContext,
        choice: &ModelChoice,
    ) -> Result<Result<ResolvedChoice, ChoiceError>, sqlx::Error> {
        let settings = self.inner.tenants.load(ctx).await?;
        Ok(resolve_choice(
            &self.inner.config.catalog,
            &self.inner.config.auto,
            &settings,
            |e| self.supports_effort(e),
            choice,
        ))
    }

    /// 投入時に解決した run のモデルを、実行時点の設定と skill のピンで決め直す
    /// （[`tenant::resolve_at_run`]）。
    pub async fn resolve_at_run(
        &self,
        ctx: &AuthContext,
        snapshot: &ResolvedChoice,
        skill_model: Option<&str>,
    ) -> Result<Result<ResolvedChoice, ChoiceError>, sqlx::Error> {
        let settings = self.inner.tenants.load(ctx).await?;
        Ok(tenant::resolve_at_run(
            &self.inner.config.catalog,
            &self.inner.config.auto,
            &settings,
            |e| self.supports_effort(e),
            snapshot,
            skill_model,
        ))
    }

    /// テナント設定での Auto の振り先（管理画面・モデル一覧の表示用）。
    pub fn auto_target(&self, settings: &TenantLlmSettings) -> Option<(String, Effort)> {
        tenant::auto_target(
            &self.inner.config.catalog,
            &self.inner.config.auto,
            settings,
        )
        .ok()
    }

    /// 利用量ダッシュボードの集計（tenant_id ＋ org に閉じる）。
    pub async fn usage_report(
        &self,
        ctx: &AuthContext,
        filter: &UsageFilter,
    ) -> Result<UsageReport, sqlx::Error> {
        self.inner.usage.report(ctx, filter).await
    }

    /// トークン消費からコスト（マイクロ USD）を見積もる（予算ガード連動・agent-core が使う）。
    ///
    /// カタログ単価で算出する純粋計算（会計は [`Self::record_generation`] が別途永続化する）。
    /// モデル未知なら既定モデル単価にフォールバックし、それも無ければ 0。
    #[must_use]
    pub fn estimate_cost_usd_micros(&self, model: &str, usage: Usage) -> i64 {
        self.inner
            .config
            .catalog
            .get(model)
            .or_else(|| self.inner.config.catalog.default_entry())
            .map_or(0, |e| e.cost_usd_micros(&usage))
    }

    /// モデルのコンテキスト窓トークン数（agent-core の剪定閾値・#529）。
    ///
    /// `None` は既定モデル。**未知のモデルは既定へフォールバックしない**: 単価と違い、別モデルの
    /// 窓を当てると閾値を誤る（窓を知らない＝固定閾値のまま、の方が安全）。
    #[must_use]
    pub fn context_window_tokens(&self, model: Option<&str>) -> Option<u32> {
        let catalog = &self.inner.config.catalog;
        model
            .map_or_else(|| catalog.default_entry(), |m| catalog.get(m))
            .and_then(|e| e.context_window_tokens)
    }

    /// リクエストを**ストリーミング**生成する。論理モデル→実 ID をカタログで解決し、
    /// モデルの振り先プロバイダへ委譲する。接続確立前の一時障害は、待ち時間を置いて
    /// 最大 2 回再試行する（[`UNAVAILABLE_BACKOFF`]・フォールバックの床）。思考強度に
    /// 対応しないモデルへは effort を落としてから送る。
    ///
    /// 呼び出し元の組織が許可していないモデルはここで拒否する（#553）。チャット・ミニアプリ・
    /// ワークフロー・エージェントのどの経路も必ずここを通るので、管理画面での許可・無効化
    /// （国外処理モデルを止める等）が経路によらず効く。
    pub async fn stream(
        &self,
        ctx: &AuthContext,
        mut req: GenerateRequest,
    ) -> Result<DeltaStream, LlmError> {
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
        let slot = self
            .inner
            .providers
            .get(entry.provider_id())
            .ok_or_else(|| LlmError::Config(format!("unknown provider for model: {logical}")))?;
        let permitted = self
            .inner
            .tenants
            .permits(ctx, entry)
            .await
            .map_err(|e| LlmError::Unavailable(format!("tenant llm settings: {e}")))?;
        if !permitted {
            return Err(LlmError::Config(format!(
                "model is not enabled for this organization: {logical}"
            )));
        }
        self.check_limits(ctx, &slot.config).await?;
        if !self.supports_effort(entry) {
            req.effort = None;
        }
        req.model = Some(entry.resolved_id().to_string());

        stream_with_retry(slot.provider.as_ref(), &req, &UNAVAILABLE_BACKOFF).await
    }

    /// 利用制限（月次コスト上限・プレビュー環境向け）を生成前に確かめる。
    ///
    /// 上限超過は [`LlmError::RateLimited`]（文言はそのままユーザーへ出す）。集計に失敗したら
    /// **生成しない**（fail-closed）。既定の無効設定では問い合わせ自体を行わない。
    async fn check_limits(
        &self,
        ctx: &AuthContext,
        provider: &ProviderConfig,
    ) -> Result<(), LlmError> {
        let res = self
            .inner
            .limits
            .check(
                ctx,
                &self.inner.config.limits,
                provider.effective_id(),
                provider.label(),
            )
            .await;
        match res {
            Ok(()) => Ok(()),
            Err(LimitError::Exceeded(msg)) => {
                tracing::info!(
                    provider = provider.effective_id(),
                    user = %ctx.principal.id,
                    "llm usage limit reached; generation rejected"
                );
                Err(LlmError::RateLimited(msg))
            }
            Err(LimitError::Query(e)) => {
                // DB 詳細はユーザーへ出さない（tracing にのみ残す）。
                tracing::error!(error = %e, "llm usage limit lookup failed");
                Err(LlmError::Unavailable(
                    "利用量の確認に失敗しました。しばらく待ってからもう一度お試しください。".into(),
                ))
            }
        }
    }

    /// モデル（論理名）の振り先プロバイダの会計ラベル（アダプタ種別名, プロバイダ ID）。
    ///
    /// 未知モデルは既定プロバイダへ落とす（会計の行を落とさない）。ID は利用制限の
    /// 「プロバイダ別の環境総枠」が数える軸なので、種別名とは別に刻む。
    fn provider_labels_for(&self, model: &str) -> (String, String) {
        let id = self
            .inner
            .config
            .catalog
            .get(model)
            .map_or(DEFAULT_PROVIDER_ID, ModelEntry::provider_id);
        let (id, slot) = match self.inner.providers.get(id) {
            Some(slot) => (id, Some(slot)),
            None => (
                DEFAULT_PROVIDER_ID,
                self.inner.providers.get(DEFAULT_PROVIDER_ID),
            ),
        };
        let kind = slot.map_or_else(|| "unknown".to_string(), |s| s.provider.name().to_string());
        (kind, id.to_string())
    }

    /// 生成 1 回分の会計＋Langfuse 計装を記録する（ストリーム消費後に呼ぶ）。
    ///
    /// 会計は同期（金額クリティカル・冪等）、Langfuse は best-effort（非同期・失敗は warn）。
    pub async fn record_generation(&self, ctx: &AuthContext, rec: &GenerationRecord) {
        // **会計より先に取る。** 会計 DB が詰まると、その待ち時間が
        // そのまま生成の所要時間として記録されてしまう。
        let ended_at = chrono::Utc::now();
        let entry = self
            .inner
            .config
            .catalog
            .get(&rec.model)
            .or_else(|| self.inner.config.catalog.default_entry());
        // 内訳が総数を超える矛盾した報告は、会計の行と金額を過大にしないよう総数へ収める。
        let usage = rec.usage.normalized();
        if usage != rec.usage {
            tracing::warn!(
                model = %rec.model,
                prompt_tokens = rec.usage.prompt_tokens,
                cache_read_tokens = rec.usage.cache_read_tokens,
                cache_write_tokens = rec.usage.cache_write_tokens,
                "llm usage: cache breakdown exceeds prompt tokens; clamped"
            );
        }
        let cost = entry.map_or(0, |e| e.cost_usd_micros(&usage));
        let (provider, provider_id) = self.provider_labels_for(&rec.model);

        let usage_rec = UsageRecord {
            idempotency_key: rec.idempotency_key.clone(),
            provider: provider.clone(),
            provider_id,
            model: rec.model.clone(),
            usage,
            cost_usd_micros: cost,
            trace_id: rec.trace_id.clone(),
            app_id: rec.app_id,
            source: rec.source,
        };
        if let Err(e) = self.inner.accounting.record(ctx, &usage_rec).await {
            tracing::error!(error = %e, "llm usage accounting failed");
        }

        if let (Some(lf), Some(trace_id)) = (&self.inner.langfuse, rec.trace_id.as_deref()) {
            let user_id = ctx.principal.id.clone();
            lf.record_generation(GenerationTrace {
                trace_id,
                // **会計の冪等キーをそのまま使う。** 呼び出しごとに一意で、
                // 再試行では同じ値になる（langfuse.rs の doc 参照）。
                generation_id: &rec.idempotency_key,
                user_id: &user_id,
                first_in_trace: rec.first_in_trace,
                started_at: rec.started_at,
                ended_at,
                name: "chat.generation",
                model: &rec.model,
                provider: &provider,
                input: &rec.input_preview,
                output: &rec.output_preview,
                usage,
                metadata: serde_json::json!({
                    "tenant_id": ctx.tenant_id,
                    "org": ctx.org,
                    "user": ctx.principal.id,
                    "cost_usd_micros": cost,
                    "app_id": rec.app_id,
                    "source": rec.source.as_str(),
                }),
            })
            .await;
        }
    }

    /// アプリの当日合計コスト（マイクロ USD・日次予算チェック用・Task 9.9）。
    pub async fn app_spend_today(
        &self,
        ctx: &AuthContext,
        app_id: uuid::Uuid,
    ) -> Result<i64, sqlx::Error> {
        self.inner.accounting.app_spend_today(ctx, app_id).await
    }
}

/// 管理画面に出す接続先の表記（`scheme://host[:port]` まで。パス・クエリ・userinfo は落とす）。
fn endpoint_label(base_url: &str) -> String {
    match reqwest::Url::parse(base_url) {
        Ok(u) => {
            let host = u.host_str().unwrap_or_default();
            match u.port() {
                Some(p) => format!("{}://{host}:{p}", u.scheme()),
                None => format!("{}://{host}", u.scheme()),
            }
        }
        Err(_) => String::new(),
    }
}

fn build_provider(
    http: reqwest::Client,
    pc: &ProviderConfig,
    catalog: &ModelCatalog,
    timeouts: LlmTimeouts,
) -> Result<Arc<dyn LlmProvider>, LlmError> {
    // 共有 HTTP クライアント（接続プール共有・タイムアウトなし）をそのまま各アダプタへ渡し、
    // タイムアウト（最初の応答・無通信・全体）は各アダプタが共通のヘルパで測る（#554）。
    // 各アダプタの「既定モデル」は、このプロバイダへ振られるモデルのうち既定モデル→先頭の順
    // （gateway は常に実 ID を詰めて渡すので、既定は保険にすぎない）。
    let id = pc.effective_id();
    let default_real = catalog
        .default_entry()
        .filter(|e| e.provider_id() == id)
        .or_else(|| catalog.models.iter().find(|m| m.provider_id() == id))
        .map_or_else(
            || catalog.default_model.clone(),
            |e| e.resolved_id().to_string(),
        );
    match pc.kind {
        ProviderKind::Stub => Ok(Arc::new(StubProvider::new().with_timeouts(timeouts))),
        ProviderKind::Openai => {
            let base = pc
                .base_url
                .clone()
                .ok_or_else(|| LlmError::Config("openai base_url required".into()))?;
            // 本文に `<think>` を入れて返すモデル（実 ID）だけタグを解釈させる（#584）。
            let think_tag_models = catalog
                .models
                .iter()
                .filter(|m| m.provider_id() == id && m.reasoning_in_content)
                .map(|m| m.resolved_id().to_string());
            Ok(Arc::new(
                OpenAiProvider::new(http, base, pc.api_key.clone(), default_real)
                    .with_user_agent(pc.user_agent.as_deref())?
                    .with_session_header(pc.session_header.as_deref())?
                    .with_think_tag_models(think_tag_models)
                    .with_timeouts(timeouts),
            ))
        }
        ProviderKind::OpenaiResponses => {
            let base = pc
                .base_url
                .clone()
                .ok_or_else(|| LlmError::Config("openai_responses base_url required".into()))?;
            Ok(Arc::new(
                OpenAiResponsesProvider::new(http, base, pc.api_key.clone(), default_real)
                    .with_user_agent(pc.user_agent.as_deref())?
                    .with_session_header(pc.session_header.as_deref())?
                    .with_timeouts(timeouts),
            ))
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
            Ok(Arc::new(
                AnthropicProvider::new(http, base, key, default_real)
                    .with_user_agent(pc.user_agent.as_deref())?
                    .with_session_header(pc.session_header.as_deref())?
                    .with_timeouts(timeouts),
            ))
        }
    }
}

/// 接続確立前（最初のバイト前）の Unavailable を再試行するまでの待ち時間（試行の間ごと）。
///
/// 最初のバイト前なので、送り直しても二重に生成されることはない。Gemini API の
/// gemma-4-31b-it は数回に 1 回 500 を返し、すぐの 1 回だけでは足りなかった（#743）。
const UNAVAILABLE_BACKOFF: [std::time::Duration; 2] = [
    std::time::Duration::from_millis(500),
    std::time::Duration::from_millis(1500),
];

/// `backoff` の数だけ、接続確立前の Unavailable を待ってから送り直す。
async fn stream_with_retry(
    provider: &dyn LlmProvider,
    req: &GenerateRequest,
    backoff: &[std::time::Duration],
) -> Result<DeltaStream, LlmError> {
    let mut waits = backoff.iter();
    loop {
        match provider.stream(req).await {
            Err(LlmError::Unavailable(e)) => {
                let Some(wait) = waits.next() else {
                    return Err(LlmError::Unavailable(e));
                };
                tracing::warn!(error = %e, ?wait, "llm provider unavailable, retrying");
                tokio::time::sleep(*wait).await;
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelCatalog, ModelEntry, ProviderConfig};

    /// 最初の `fail` 回は `err` を返し、その後は空のストリームを返す。
    struct Flaky {
        fail: usize,
        err: fn() -> LlmError,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for Flaky {
        fn name(&self) -> &'static str {
            "flaky"
        }
        async fn stream(&self, _req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < self.fail {
                return Err((self.err)());
            }
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn unavailable_before_first_byte_is_retried_as_many_times_as_backoff_allows() {
        let backoff = [std::time::Duration::from_millis(1); 2];
        let req = GenerateRequest::new(Vec::new());
        let unavailable = || LlmError::Unavailable("500".into());
        let calls = |p: &Flaky| p.calls.load(std::sync::atomic::Ordering::SeqCst);

        let p = Flaky {
            fail: 2,
            err: unavailable,
            calls: 0.into(),
        };
        assert!(stream_with_retry(&p, &req, &backoff).await.is_ok());
        assert_eq!(calls(&p), 3);

        let p = Flaky {
            fail: 3,
            err: unavailable,
            calls: 0.into(),
        };
        assert!(matches!(
            stream_with_retry(&p, &req, &backoff).await,
            Err(LlmError::Unavailable(_))
        ));
        assert_eq!(calls(&p), 3);

        // 直せば通るたぐいの誤りは送り直さない。
        let p = Flaky {
            fail: 1,
            err: || LlmError::BadRequest("400".into()),
            calls: 0.into(),
        };
        assert!(matches!(
            stream_with_retry(&p, &req, &backoff).await,
            Err(LlmError::BadRequest(_))
        ));
        assert_eq!(calls(&p), 1);
    }

    /// 指定 kind ＋ base_url ＋ api_key から最小構成の設定を組む。
    fn config(kind: ProviderKind, base_url: Option<&str>, api_key: Option<&str>) -> GatewayConfig {
        GatewayConfig {
            provider: ProviderConfig {
                kind,
                base_url: base_url.map(str::to_string),
                api_key: api_key.map(str::to_string),
                user_agent: None,
                session_header: None,
                ..Default::default()
            },
            catalog: ModelCatalog {
                default_model: "m".into(),
                models: vec![ModelEntry {
                    id: "m".into(),
                    real_id: Some("real-m".into()),
                    prompt_price_micros_per_mtok: 0,
                    completion_price_micros_per_mtok: 0,
                    context_window_tokens: None,
                    ..Default::default()
                }],
            },
            langfuse: None,
            ..Default::default()
        }
    }

    #[test]
    fn stub_provider_needs_no_endpoint() {
        let cfg = config(ProviderKind::Stub, None, None);
        let p = build_provider(
            reqwest::Client::new(),
            &cfg.provider,
            &cfg.catalog,
            LlmTimeouts::default(),
        )
        .unwrap();
        assert_eq!(p.name(), "stub");
    }

    #[test]
    fn openai_responses_requires_base_url() {
        let ok = config(
            ProviderKind::OpenaiResponses,
            Some("https://opencode.ai/inference/go/v1"),
            None,
        );
        assert_eq!(
            build_provider(
                reqwest::Client::new(),
                &ok.provider,
                &ok.catalog,
                LlmTimeouts::default()
            )
            .unwrap()
            .name(),
            "openai_responses"
        );
        let missing = config(ProviderKind::OpenaiResponses, None, None);
        let res = build_provider(
            reqwest::Client::new(),
            &missing.provider,
            &missing.catalog,
            LlmTimeouts::default(),
        );
        assert!(matches!(res, Err(LlmError::Config(_))));
    }

    #[test]
    fn openai_requires_base_url() {
        let ok = config(ProviderKind::Openai, Some("http://vllm:8000/v1"), None);
        assert_eq!(
            build_provider(
                reqwest::Client::new(),
                &ok.provider,
                &ok.catalog,
                LlmTimeouts::default()
            )
            .unwrap()
            .name(),
            "openai"
        );

        let missing = config(ProviderKind::Openai, None, None);
        let res = build_provider(
            reqwest::Client::new(),
            &missing.provider,
            &missing.catalog,
            LlmTimeouts::default(),
        );
        assert!(matches!(res, Err(LlmError::Config(_))));
    }

    #[test]
    fn anthropic_requires_api_key_and_defaults_base_url() {
        // base_url 省略時は既定エンドポイントへフォールバックし、api_key があれば構築できる。
        let ok = config(ProviderKind::Anthropic, None, Some("k"));
        assert_eq!(
            build_provider(
                reqwest::Client::new(),
                &ok.provider,
                &ok.catalog,
                LlmTimeouts::default()
            )
            .unwrap()
            .name(),
            "anthropic"
        );

        let missing = config(ProviderKind::Anthropic, None, None);
        let res = build_provider(
            reqwest::Client::new(),
            &missing.provider,
            &missing.catalog,
            LlmTimeouts::default(),
        );
        assert!(matches!(res, Err(LlmError::Config(_))));
    }

    fn lazy_db() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
            .unwrap()
    }

    /// 既定（stub）＋追加（openai・vLLM）の 2 プロバイダ構成。
    fn two_providers() -> GatewayConfig {
        let mut cfg = config(ProviderKind::Stub, None, None);
        cfg.extra_providers.push(ProviderConfig {
            id: "vllm".into(),
            display_name: Some("社内 GPU".into()),
            kind: ProviderKind::Openai,
            base_url: Some("http://user:pw@vllm.internal:8000/v1?x=1".into()),
            ..Default::default()
        });
        cfg.catalog.models.push(ModelEntry {
            id: "local".into(),
            provider: Some("vllm".into()),
            ..Default::default()
        });
        cfg
    }

    // connect_lazy は tokio ランタイムを要する。
    #[tokio::test]
    async fn multiple_providers_are_listed_without_secrets() {
        let gw = LlmGateway::build(lazy_db(), reqwest::Client::new(), two_providers()).unwrap();
        let ps = gw.providers();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].id, DEFAULT_PROVIDER_ID);
        assert_eq!(ps[1].id, "vllm");
        assert_eq!(ps[1].label, "社内 GPU");
        // userinfo・パス・クエリは表示に出さない。
        assert_eq!(ps[1].endpoint.as_deref(), Some("http://vllm.internal:8000"));
        assert!(!ps[1].has_api_key);
        // 振り先プロバイダで会計のラベル（種別名・プロバイダ ID）が決まる。
        assert_eq!(
            gw.provider_labels_for("local"),
            ("openai".to_string(), "vllm".to_string())
        );
        assert_eq!(
            gw.provider_labels_for("m"),
            ("stub".to_string(), DEFAULT_PROVIDER_ID.to_string())
        );
        // 未知モデルは既定プロバイダへ落ちる（会計の行を落とさない）。
        assert_eq!(
            gw.provider_labels_for("unknown-model"),
            ("stub".to_string(), DEFAULT_PROVIDER_ID.to_string())
        );
    }

    // connect_lazy は tokio ランタイムを要する。
    #[tokio::test]
    async fn model_pointing_to_unknown_provider_fails_to_build() {
        let mut cfg = config(ProviderKind::Stub, None, None);
        cfg.catalog.models[0].provider = Some("nope".into());
        let res = LlmGateway::build(lazy_db(), reqwest::Client::new(), cfg);
        assert!(matches!(res, Err(LlmError::Config(_))));
    }

    // connect_lazy は tokio ランタイムを要する。
    #[tokio::test]
    async fn duplicate_provider_id_fails_to_build() {
        let mut cfg = config(ProviderKind::Stub, None, None);
        cfg.extra_providers.push(ProviderConfig::default());
        let res = LlmGateway::build(lazy_db(), reqwest::Client::new(), cfg);
        assert!(matches!(res, Err(LlmError::Config(_))));
    }

    // connect_lazy は tokio ランタイムを要する。
    #[tokio::test]
    async fn effort_support_follows_catalog_then_provider_kind() {
        let gw = LlmGateway::build(lazy_db(), reqwest::Client::new(), two_providers()).unwrap();
        let local = gw.catalog().get("local").unwrap().clone();
        // OpenAI 互換（Chat Completions）は既定で送らない。
        assert!(!gw.supports_effort(&local));
        let explicit = ModelEntry {
            supports_effort: Some(true),
            ..local
        };
        assert!(gw.supports_effort(&explicit));
    }

    #[tokio::test]
    async fn stream_rejects_unknown_model_before_touching_tenant_settings() {
        let gw = LlmGateway::build(lazy_db(), reqwest::Client::new(), two_providers()).unwrap();
        let ctx = AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t".into()),
            },
            "o".into(),
            "t".into(),
        );
        let mut req = GenerateRequest::new(vec![crate::model::Message::text(
            crate::model::Role::User,
            "hi",
        )]);
        // 未知モデルは既定へ落とさず拒否する（DB に届く前・接続先は無いので届けば Unavailable）。
        req.model = Some("ghost".into());
        assert!(matches!(
            gw.stream(&ctx, req.clone()).await,
            Err(LlmError::Config(_))
        ));
        // 既知モデルは組織の設定を引く（無効化の判定・ここでは DB が無いので Unavailable）。
        req.model = Some("m".into());
        assert!(matches!(
            gw.stream(&ctx, req).await,
            Err(LlmError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn context_window_does_not_borrow_another_models_window() {
        let mut cfg = config(ProviderKind::Stub, None, None);
        cfg.catalog.models[0].context_window_tokens = Some(32_768);
        let db = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
            .unwrap();
        let gw = LlmGateway::build(db, reqwest::Client::new(), cfg).unwrap();
        assert_eq!(gw.context_window_tokens(None), Some(32_768)); // 既定モデル
        assert_eq!(gw.context_window_tokens(Some("m")), Some(32_768));
        // 未知モデルに既定の窓を当てると閾値を誤るので「知らない」を返す。
        assert_eq!(gw.context_window_tokens(Some("other")), None);
    }
}
