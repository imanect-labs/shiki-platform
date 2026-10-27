//! 判定のチョークポイント [`LlmGateway::decide`]（と決定キャッシュを通す [`LlmGateway::decide_cached`]）と、
//! 決定器の構築・構築時の検査（FEAT-1・gateway.rs の行数上限のため分割）。
//!
//! 設計の正: docs/design.md §4.5（決定モデル）。`stream` と同じ順で確かめる:
//! 1. カタログで引く（無い・判定用でない → `Config`）。
//! 2. 要求の上限（質問の数・選択肢・段階・判定材料の大きさ → `BadRequest`）。
//! 3. 組織の許可: 判定用のモデルと、元にする chat モデル（`decision_via`）の**両方**（fail-closed）。
//!    国外処理のモデルを止めた組織で、判定用のモデルが抜け道にならないようにする。
//! 4. 決定キャッシュ（`decide_cached` だけ・T10）: 当たればここで返す。許可（3）は当たっても確かめ済みで、
//!    会計と利用上限（5・8）は通さない（決定器を呼ばないので費用が出ない）。
//! 5. 請求先プロバイダの利用上限（判定用のモデルの `provider` は元のモデルと同じ）。
//! 6. 決定器（構築時に組んだもの）を呼ぶ。
//! 7. 応答を要求と突き合わせる（答えの過不足・値域・確率の範囲 → `Internal`）。
//! 8. 会計（ストリームではないので、呼び出し側の記録漏れが無いようにここで行う）。使えた判定は呼び出し側の
//!    キー、使えなかった判定（失敗・突き合わせで弾いたもの）で使用量のあるものは `{key}#f{uuid}`。
//!    失敗を呼び出し側のキーで記録すると、後の再試行・run の再開で成功した分が重複除去で消える。
//! 9. 決定キャッシュへ書く（`decide_cached` だけ・使えた判定で、答えた版が構築時の版と同じもの）。

use std::collections::HashMap;
use std::sync::Arc;

use authz::AuthContext;

use super::{GenerationRecord, LlmGateway, ProviderSlot};
use crate::config::{GatewayConfig, ModelEntry, ProviderKind};
use crate::decision::cache::{cache_key, Decided, DecisionCache};
use crate::decision::check::{check_request, check_response};
use crate::decision::{
    DecideCall, DecideRequest, DecideResponse, DecisionProvider, INVALID_REQUEST_HEAD,
};
use crate::model::Usage;
use crate::provider::LlmError;
use crate::providers::llm_decider::LlmDecider;
use crate::providers::stub_decider::{StubDecider, STUB_DECIDER_VERSION};
use crate::providers::typesafe;
use crate::structured;

/// 決定器と、その版（構築時にカタログから決まる・決定キャッシュのキーに入れる）。
pub(super) struct DeciderSlot {
    provider: Arc<dyn DecisionProvider>,
    /// 決定器が答えるときの `resolved_model`（llm_decider は元の実 ID・プロンプトの版・回数・思考の深さ）。
    version: String,
}

/// 決定器の表（判定用のモデルの ID → 決定器）。
pub(super) type Deciders = HashMap<String, DeciderSlot>;

/// Langfuse の generation 名。
const TRACE_NAME: &str = "decision";

impl LlmGateway {
    /// 判定する（チョークポイント）。失敗も会計に載せてから返す。
    ///
    /// 返す誤り: カタログ外・判定用でない・組織で無効（判定用・元のモデルのどちらか）は `Config`、要求の
    /// 上限は `BadRequest`（[`crate::decision::DecideErrorKind::InvalidRequest`]）、利用上限は
    /// `RateLimited`、決定器の失敗はその誤り（[`crate::decision::decide_error_kind`] で種類を見分ける）。
    pub async fn decide(
        &self,
        ctx: &AuthContext,
        req: &DecideRequest,
        call: &DecideCall,
    ) -> Result<DecideResponse, LlmError> {
        self.decide_inner(ctx, req, call, None)
            .await
            .map(|d| d.response)
    }

    /// 決定キャッシュを通して判定する（ワークフローの ai.decide の step 属性 `cache`・T10）。
    ///
    /// 範囲は `ctx` のテナント＋組織と `cache.workflow_id`。当たっても組織の許可（判定用のモデルと元の
    /// chat モデルの両方）は確かめ、無効なら [`Self::decide`] と同じ `Config` で失敗する。当たりは会計しない
    /// （利用上限も確かめない）。誤りは [`Self::decide`] と同じ。
    pub async fn decide_cached(
        &self,
        ctx: &AuthContext,
        req: &DecideRequest,
        call: &DecideCall,
        cache: &DecisionCache,
    ) -> Result<Decided, LlmError> {
        self.decide_inner(ctx, req, call, Some(cache)).await
    }

    async fn decide_inner(
        &self,
        ctx: &AuthContext,
        req: &DecideRequest,
        call: &DecideCall,
        cache: Option<&DecisionCache>,
    ) -> Result<Decided, LlmError> {
        let catalog = &self.inner.config.catalog;
        let entry = catalog
            .get(&req.model)
            .ok_or_else(|| LlmError::Config(format!("unknown model: {}", req.model)))?;
        if !entry.is_decision() {
            return Err(LlmError::Config(format!(
                "not a decision model: {}",
                req.model
            )));
        }
        check_request(req)
            .map_err(|m| LlmError::BadRequest(format!("{INVALID_REQUEST_HEAD}{m}")))?;
        let via = match entry.decision_via.as_deref() {
            Some(v) => Some(
                catalog
                    .get(v)
                    .ok_or_else(|| LlmError::Config(format!("unknown model: {v}")))?,
            ),
            None => None,
        };
        self.ensure_permitted(ctx, entry).await?;
        if let Some(via) = via {
            self.ensure_permitted(ctx, via).await?;
        }
        let decider = self
            .inner
            .deciders
            .get(&entry.id)
            .ok_or_else(|| LlmError::Config(format!("no decider for model: {}", entry.id)))?;
        // 決定キャッシュ（許可を確かめた後・利用上限と会計の前）。
        let cached = cache.map(|scope| (scope, cache_key(&decider.version, req)));
        if let Some((scope, key)) = &cached {
            if let Some(hit) = self.cached_answer(ctx, req, scope, key).await {
                return Ok(Decided {
                    response: hit,
                    cached: true,
                });
            }
        }
        let slot = self.slot_for(entry)?;
        self.check_limits(ctx, &slot.config).await?;

        let started_at = chrono::Utc::now();
        // 請求の単位は、LLM に真似させる決定器なら元のモデル（その単価・プロバイダの枠で数える）。
        let billed = via.unwrap_or(entry);
        let account = |usage: Usage, used: bool| async move {
            if used || has_usage(&usage) {
                self.record_named(
                    ctx,
                    &record(call, req, billed, usage, used, started_at),
                    TRACE_NAME,
                )
                .await;
            }
        };
        let provider = &decider.provider;
        let resp = match provider.decide(req).await {
            Ok(resp) => match check_response(req, &resp) {
                Ok(()) => {
                    account(resp.usage, true).await;
                    resp
                }
                Err(why) => {
                    tracing::error!(model = %entry.id, decider = provider.name(), %why, "決定器が要求と合わない答えを返した");
                    account(resp.usage, false).await;
                    return Err(LlmError::Internal(format!(
                        "decision provider returned an invalid response: {why}"
                    )));
                }
            },
            Err(failure) => {
                account(failure.usage, false).await;
                return Err(failure.error);
            }
        };
        if let Some((scope, key)) = &cached {
            // 構築時の版と違う版が答えたもの（決定モデルの API が別の版で答えた等）は、キーの版と合わないので
            // 使い回さない。
            if resp.resolved_model == decider.version {
                if let Err(e) = self.inner.decision_cache.put(ctx, scope, key, &resp).await {
                    tracing::warn!(model = %entry.id, error = %e, "決定キャッシュに書けない（使い回さないだけ）");
                }
            }
        }
        Ok(Decided {
            response: resp,
            cached: false,
        })
    }

    /// 決定キャッシュの答え（期限内で、要求と突き合わせて合うもの）。読めなければ外れとして扱う。
    async fn cached_answer(
        &self,
        ctx: &AuthContext,
        req: &DecideRequest,
        scope: &DecisionCache,
        key: &[u8; 32],
    ) -> Option<DecideResponse> {
        let hit = match self.inner.decision_cache.get(ctx, scope, key).await {
            Ok(hit) => hit?,
            Err(e) => {
                tracing::warn!(model = %req.model, error = %e, "決定キャッシュを読めない（決定器を呼ぶ）");
                return None;
            }
        };
        // キーに質問が入っているので合うはずだが、合わない答えは返さない（多層防御）。
        match check_response(req, &hit) {
            Ok(()) => Some(hit),
            Err(why) => {
                tracing::error!(model = %req.model, %why, "決定キャッシュの答えが要求と合わない（使わない）");
                None
            }
        }
    }

    /// 組織がこのモデルを使えるか（無効なら `Config`・設定を引けなければ `Unavailable`）。
    pub(super) async fn ensure_permitted(
        &self,
        ctx: &AuthContext,
        entry: &ModelEntry,
    ) -> Result<(), LlmError> {
        let permitted = self
            .inner
            .tenants
            .permits(ctx, entry)
            .await
            .map_err(|e| LlmError::Unavailable(format!("tenant llm settings: {e}")))?;
        if permitted {
            Ok(())
        } else {
            Err(LlmError::Config(format!(
                "model is not enabled for this organization: {}",
                entry.id
            )))
        }
    }

    /// モデルの振り先プロバイダ。
    pub(super) fn slot_for(&self, entry: &ModelEntry) -> Result<&ProviderSlot, LlmError> {
        self.inner
            .providers
            .get(entry.provider_id())
            .ok_or_else(|| LlmError::Config(format!("unknown provider for model: {}", entry.id)))
    }
}

const fn has_usage(u: &Usage) -> bool {
    u.prompt_tokens > 0 || u.completion_tokens > 0
}

/// 会計・計装の記録（材料と答えは Langfuse にも送らない。質問のキーだけを入力の要約にする）。
fn record(
    call: &DecideCall,
    req: &DecideRequest,
    billed: &ModelEntry,
    usage: Usage,
    used: bool,
    started_at: chrono::DateTime<chrono::Utc>,
) -> GenerationRecord {
    let idempotency_key = if used {
        call.idempotency_key.clone()
    } else {
        format!(
            "{}#f{}",
            call.idempotency_key,
            uuid::Uuid::new_v4().simple()
        )
    };
    let keys: Vec<&str> = req.questions.iter().map(|q| q.key.as_str()).collect();
    GenerationRecord {
        idempotency_key,
        model: billed.id.clone(),
        usage,
        trace_id: call.trace_id.clone(),
        input_preview: format!("{}: {}", req.model, keys.join(", ")),
        output_preview: String::new(),
        app_id: call.app_id,
        source: call.source,
        first_in_trace: false,
        started_at: Some(started_at),
    }
}

/// カタログの判定用のモデルから決定器を組む（構築時の検査を兼ねる・誤りは起動を止める）。
///
/// - `decision_via` があれば llm_decider。元のモデルは同じカタログの chat モデルで、`provider` が同じ
///   （会計・利用上限の軸を揃える）、本人専用のプラン（chatgpt_plan）ではない、国外処理なら判定用の
///   モデルも国外処理と表示する。
/// - 無ければ、振り先が TypeSafe System One 互換（`typesafe_systemone`・実験的・opt-in）ならその決定器、stub なら
///   stub の決定器（テスト用）。
/// - 既定のモデルと Auto の振り先は chat モデルであること。chat モデルに判定用の項目は書けず、判定専用の
///   プロバイダ（`typesafe_systemone`）へも振れない。
pub(super) fn build_deciders(
    config: &GatewayConfig,
    providers: &HashMap<String, ProviderSlot>,
    http: &reqwest::Client,
) -> Result<Deciders, LlmError> {
    let catalog = &config.catalog;
    let bad = |m: &ModelEntry, why: &str| LlmError::Config(format!("model {}: {why}", m.id));
    for (what, id) in [
        ("default_model", catalog.default_model.as_str()),
        ("auto.model", config.auto.model.as_str()),
    ] {
        if catalog.get(id).is_some_and(ModelEntry::is_decision) {
            return Err(LlmError::Config(format!(
                "{what} {id} is a decision model (must be a chat model)"
            )));
        }
    }
    let mut deciders = Deciders::new();
    for m in &catalog.models {
        if m.is_chat() {
            if m.decision_via.is_some() || m.decision.is_some() {
                return Err(bad(
                    m,
                    "decision_via / decision are only for decision models",
                ));
            }
            if providers
                .get(m.provider_id())
                .is_some_and(|s| s.config.kind == ProviderKind::TypesafeSystemone)
            {
                return Err(bad(
                    m,
                    "typesafe_systemone serves decision models only (not chat models)",
                ));
            }
            continue;
        }
        let tuning = m.decision.unwrap_or_default();
        if ![1, 3, 5].contains(&tuning.samples) {
            return Err(bad(m, "decision.samples must be 1, 3 or 5"));
        }
        let slot = providers
            .get(m.provider_id())
            .ok_or_else(|| bad(m, "unknown provider"))?;
        let decider: DeciderSlot = match m.decision_via.as_deref() {
            Some(via_id) => {
                let via = catalog.get(via_id).ok_or_else(|| {
                    bad(m, &format!("decision_via {via_id} is not in the catalog"))
                })?;
                if !via.is_chat() {
                    return Err(bad(m, "decision_via must be a chat model"));
                }
                if via.provider_id() != m.provider_id() {
                    return Err(bad(
                        m,
                        "provider must be the same as the decision_via model",
                    ));
                }
                if slot.config.kind == ProviderKind::ChatgptPlan {
                    return Err(bad(
                        m,
                        "decision_via cannot be a personal (chatgpt_plan) model",
                    ));
                }
                if via.processed_abroad && !m.processed_abroad {
                    return Err(bad(
                        m,
                        "processed_abroad must be true when decision_via is processed abroad",
                    ));
                }
                let supports_effort = via
                    .supports_effort
                    .unwrap_or_else(|| slot.config.kind.effort_by_default());
                let decider = LlmDecider::new(
                    Arc::clone(&slot.provider),
                    via.resolved_id(),
                    supports_effort,
                    structured::mode_for(slot.config.kind, via),
                    tuning,
                );
                DeciderSlot {
                    version: decider.resolved_model().to_string(),
                    provider: Arc::new(decider),
                }
            }
            None if slot.config.kind == ProviderKind::TypesafeSystemone => {
                let decider =
                    typesafe::from_config(http, m, &slot.config).map_err(|why| bad(m, &why))?;
                DeciderSlot {
                    version: decider.version().to_string(),
                    provider: Arc::new(decider),
                }
            }
            None if slot.config.kind == ProviderKind::Stub => DeciderSlot {
                provider: Arc::new(StubDecider),
                version: STUB_DECIDER_VERSION.to_string(),
            },
            None => return Err(bad(m, "a decision model needs decision_via")),
        };
        deciders.insert(m.id.clone(), decider);
    }
    Ok(deciders)
}

#[cfg(test)]
mod tests;
