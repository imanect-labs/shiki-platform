//! 管理コンソールの AI（`/org-admin/ai/*`・組織管理者のみ・#553）。
//!
//! - プロバイダ: デプロイ設定で接続したプロバイダの表示（秘密は出さない・追加/キー登録は範囲外）。
//! - モデル: プラットフォームのカタログと、テナントでの有効/無効・既定・Auto・円換算レート。
//! - 利用量: 会計（`llm_usage`）をそのまま集計して出す（課金と同じ数字）。
//!
//! 管理者チェックはルート表のポリシー（`AccessPolicy::OrgAdmin`）が一律に掛ける。
//! 設定の変更は監査ログに 1 操作 1 行で残す。

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    routing::get,
    Json,
};
use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use utoipa::{IntoParams, ToSchema};

use super::llm_models::{gateway, provider_label, EffortLevel};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn org_admin_ai_route_decls() -> Vec<RouteDecl> {
    let r = RouteDecl::new;
    vec![
        r(
            "/org-admin/ai/providers",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(list_providers),
        ),
        r(
            "/org-admin/ai/models",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(list_models),
        ),
        r(
            "/org-admin/ai/settings",
            &["GET", "PUT"],
            AccessPolicy::OrgAdmin,
            || get(get_settings).put(update_settings),
        ),
        r(
            "/org-admin/ai/usage",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(get_usage),
        ),
    ]
}

// ---------------------------------------------------------------------------
// プロバイダ
// ---------------------------------------------------------------------------

/// 接続済みプロバイダ 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminProvider {
    pub id: String,
    pub label: String,
    /// アダプタ種別（`anthropic` / `openai` / `openai_responses` / `stub`）。
    pub kind: String,
    /// 接続先（`scheme://host[:port]` まで）。既定エンドポイントを使う場合は `null`。
    pub endpoint: Option<String>,
    /// API キーが設定済みか（値は返さない）。
    pub has_api_key: bool,
    /// このプロバイダへ振られるモデル数（カタログ全体）。
    pub model_count: usize,
    /// このプロバイダのモデルに国外処理のものがあるか。
    pub processed_abroad: bool,
}

/// 接続済みプロバイダの一覧。
#[utoipa::path(
    get,
    path = "/org-admin/ai/providers",
    responses(
        (status = 200, description = "プロバイダ一覧", body = [AdminProvider]),
        (status = 403, description = "組織管理者ではない"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<AdminProvider>>, ApiError> {
    let gw = gateway(&state)?;
    let catalog = gw.catalog();
    Ok(Json(
        gw.providers()
            .into_iter()
            .map(|p| {
                let models: Vec<_> = catalog
                    .models
                    .iter()
                    .filter(|m| m.provider_id() == p.id)
                    .collect();
                AdminProvider {
                    kind: p.kind.as_str().to_string(),
                    endpoint: p.endpoint.clone(),
                    has_api_key: p.has_api_key,
                    model_count: models.len(),
                    processed_abroad: models.iter().any(|m| m.processed_abroad),
                    id: p.id,
                    label: p.label,
                }
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// モデルと設定
// ---------------------------------------------------------------------------

/// カタログのモデル 1 件（管理者向け・単価を含む）。
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminModel {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub provider_id: String,
    pub provider_label: String,
    pub processed_abroad: bool,
    pub supports_effort: bool,
    /// テナントで有効か。
    pub enabled: bool,
    /// 単価（マイクロ USD / 100 万トークン）。0 は未設定（自社 GPU 等）。
    pub prompt_price_micros_per_mtok: u64,
    pub completion_price_micros_per_mtok: u64,
    pub context_window_tokens: Option<u32>,
}

/// テナントの AI 設定。
#[derive(Debug, Serialize, ToSchema)]
pub struct AiSettings {
    /// 新しいチャットの既定（`auto` かモデル ID）。
    pub default_model: String,
    pub allow_user_choice: bool,
    pub default_effort: EffortLevel,
    /// Auto の振り先の上書き（`null` はデプロイ設定）。
    pub auto_model: Option<String>,
    pub auto_effort: Option<EffortLevel>,
    /// 実際の Auto の振り先（上書き・設定・フォールバックを解決した結果）。
    pub auto_target_model: Option<String>,
    pub auto_target_effort: Option<EffortLevel>,
    /// 円換算レート（1 USD あたりの円）。`null` は円を出さない。参考表示のみ。
    pub jpy_per_usd: Option<f64>,
}

/// `GET /org-admin/ai/models` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminModelsResponse {
    pub models: Vec<AdminModel>,
    pub settings: AiSettings,
}

fn settings_dto(gw: &llm_gateway::LlmGateway, s: &llm_gateway::TenantLlmSettings) -> AiSettings {
    let target = gw.auto_target(s);
    AiSettings {
        default_model: s.default_model.clone(),
        allow_user_choice: s.allow_user_choice,
        default_effort: s.default_effort.into(),
        auto_model: s.auto_model.clone(),
        auto_effort: s.auto_effort.map(Into::into),
        auto_target_model: target.as_ref().map(|(m, _)| m.clone()),
        auto_target_effort: target.map(|(_, e)| e.into()),
        jpy_per_usd: s.jpy_per_usd_centi.map(|c| f64::from(c) / 100.0),
    }
}

async fn load_settings(
    gw: &llm_gateway::LlmGateway,
    ctx: &authz::AuthContext,
) -> Result<llm_gateway::TenantLlmSettings, ApiError> {
    gw.tenant_settings(ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("tenant llm settings: {e}")))
}

/// カタログのモデルとテナントの設定。
#[utoipa::path(
    get,
    path = "/org-admin/ai/models",
    responses(
        (status = 200, description = "モデルと設定", body = AdminModelsResponse),
        (status = 403, description = "組織管理者ではない"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn list_models(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<AdminModelsResponse>, ApiError> {
    let gw = gateway(&state)?;
    let settings = load_settings(gw, &ctx).await?;
    let catalog = gw.catalog();
    let models = catalog
        .models
        .iter()
        .map(|m| AdminModel {
            id: m.id.clone(),
            label: m.label().to_string(),
            description: m.description.clone(),
            provider_id: m.provider_id().to_string(),
            provider_label: provider_label(gw, m.provider_id()),
            processed_abroad: m.processed_abroad,
            supports_effort: gw.supports_effort(m),
            enabled: settings.is_enabled(catalog, &m.id),
            prompt_price_micros_per_mtok: m.prompt_price_micros_per_mtok,
            completion_price_micros_per_mtok: m.completion_price_micros_per_mtok,
            context_window_tokens: m.context_window_tokens,
        })
        .collect();
    Ok(Json(AdminModelsResponse {
        models,
        settings: settings_dto(gw, &settings),
    }))
}

/// テナントの AI 設定。
#[utoipa::path(
    get,
    path = "/org-admin/ai/settings",
    responses(
        (status = 200, description = "AI 設定", body = AiSettings),
        (status = 403, description = "組織管理者ではない"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn get_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<AiSettings>, ApiError> {
    let gw = gateway(&state)?;
    let settings = load_settings(gw, &ctx).await?;
    Ok(Json(settings_dto(gw, &settings)))
}

/// 欠落（変えない）と `null`（上書きを外す）を区別して読む。
#[allow(clippy::option_option)] // 「変えない／外す／値」の 3 状態をそのまま表す。
fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// モデル 1 件の有効/無効。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ModelToggle {
    pub id: String,
    pub enabled: bool,
}

/// 設定の更新（省略した項目は変えない）。
#[derive(Debug, Default, Deserialize, ToSchema)]
#[allow(clippy::option_option)]
pub struct UpdateAiSettingsRequest {
    /// `auto` かモデル ID。
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub allow_user_choice: Option<bool>,
    #[serde(default)]
    pub default_effort: Option<EffortLevel>,
    /// Auto の振り先の上書き。`null` でデプロイ設定へ戻す。
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>, nullable)]
    pub auto_model: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<EffortLevel>, nullable)]
    pub auto_effort: Option<Option<EffortLevel>>,
    /// 円換算レート（1 USD あたりの円・1〜1000）。`null` で円を出さない。
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<f64>, nullable)]
    pub jpy_per_usd: Option<Option<f64>>,
    /// モデルの有効/無効の変更。
    #[serde(default)]
    pub models: Vec<ModelToggle>,
}

/// テナントの AI 設定を更新する（検証エラーは 400・監査に残す）。
#[utoipa::path(
    put,
    path = "/org-admin/ai/settings",
    request_body = UpdateAiSettingsRequest,
    responses(
        (status = 200, description = "更新後の設定", body = AiSettings),
        (status = 400, description = "検証エラー（カタログ外・既定が無効・全モデル無効・レート範囲外）"),
        (status = 403, description = "組織管理者ではない"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn update_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<UpdateAiSettingsRequest>,
) -> Result<Json<AiSettings>, ApiError> {
    let gw = gateway(&state)?;
    let jpy_per_usd_centi = match req.jpy_per_usd {
        None => None,
        Some(None) => Some(None),
        Some(Some(rate)) => {
            if !rate.is_finite() || !(1.0..=1000.0).contains(&rate) {
                return Err(invalid("円換算レートは 1〜1000 円の範囲で指定してください"));
            }
            // 範囲検査済み（1〜1000 円 → 100〜100000）なので i32 に収まる。
            #[allow(clippy::cast_possible_truncation)]
            Some(Some((rate * 100.0).round() as i32))
        }
    };
    let patch = llm_gateway::SettingsPatch {
        default_model: req.default_model.clone(),
        allow_user_choice: req.allow_user_choice,
        default_effort: req.default_effort.map(Into::into),
        auto_model: req.auto_model.clone(),
        auto_effort: req.auto_effort.map(|e| e.map(Into::into)),
        jpy_per_usd_centi,
        models: req
            .models
            .iter()
            .map(|m| (m.id.clone(), m.enabled))
            .collect(),
    };
    let next = match gw
        .update_tenant_settings(&ctx, &patch)
        .await
        .map_err(|e| ApiError::Internal(format!("tenant llm settings: {e}")))?
    {
        Ok(next) => next,
        Err(e) => return Err(invalid(&e.to_string())),
    };
    // 監査はコミット済み操作の記録（失敗しても結果は変えず、欠落をログで顕在化する）。
    if let Err(e) = state
        .audit
        .record(
            &ctx,
            AuditEntry {
                action: "org_admin.ai_settings.update",
                object_type: "tenant_llm_settings",
                object_id: &ctx.tenant_id,
                decision: Decision::Allow,
                trace_id: trace.as_deref(),
                metadata: json!({
                    "default_model": req.default_model,
                    "allow_user_choice": req.allow_user_choice,
                    "default_effort": req.default_effort,
                    "auto_model": req.auto_model,
                    "auto_effort": req.auto_effort,
                    "jpy_per_usd": req.jpy_per_usd,
                    "models": req.models.iter().map(|m| json!({"id": m.id, "enabled": m.enabled})).collect::<Vec<_>>(),
                }),
            },
        )
        .await
    {
        tracing::error!(error = %e, "AI 設定変更の監査記録に失敗");
    }
    Ok(Json(settings_dto(gw, &next)))
}

/// 利用者へ理由を返す 400（`BadRequest` は本文が潰れるので構造化ボディで返す）。
fn invalid(detail: &str) -> ApiError {
    ApiError::UnprocessableJson(json!({
        "status": 400,
        "title": "Bad Request",
        "detail": detail,
    }))
}

// ---------------------------------------------------------------------------
// 利用量
// ---------------------------------------------------------------------------

/// 利用量の絞り込み（日付は日本時間・両端含む）。省略は今月 1 日〜今日。
#[derive(Debug, Deserialize, IntoParams)]
pub struct UsageQuery {
    /// 開始日（YYYY-MM-DD）。
    pub from: Option<NaiveDate>,
    /// 終了日（YYYY-MM-DD）。
    pub to: Option<NaiveDate>,
    /// モデル ID で絞る。
    pub model: Option<String>,
    /// 利用元で絞る（`chat` / `agent` / `workflow` / `miniapp` / `other`）。
    pub source: Option<String>,
}

/// 合計。
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageTotalsDto {
    pub cost_usd_micros: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub calls: i64,
    /// 利用したユーザー数（ミニアプリ・ワークフロー経由も実行ユーザーで数える）。
    pub users: i64,
}

/// 日別 1 行（範囲内の全日付を返す・利用の無い日は 0）。
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageDayDto {
    pub day: NaiveDate,
    pub cost_usd_micros: i64,
    pub tokens: i64,
    pub calls: i64,
}

/// 内訳 1 行。
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageRowDto {
    /// モデル ID・ユーザー ID・利用元のいずれか。
    pub key: String,
    /// 表示名（モデル名・ユーザーの表示名・利用元の日本語名）。
    pub label: String,
    pub cost_usd_micros: i64,
    pub tokens: i64,
    pub calls: i64,
}

/// `GET /org-admin/ai/usage` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageResponse {
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// 円換算レート（1 USD あたりの円）。`null` は円を出さない。参考表示のみ。
    pub jpy_per_usd: Option<f64>,
    pub totals: UsageTotalsDto,
    pub daily: Vec<UsageDayDto>,
    pub by_model: Vec<UsageRowDto>,
    pub by_user: Vec<UsageRowDto>,
    pub by_source: Vec<UsageRowDto>,
}

/// 集計できる期間の上限（日）。
const MAX_RANGE_DAYS: i64 = 366;

const SOURCES: &[(&str, &str)] = &[
    ("chat", "チャット"),
    ("agent", "エージェント"),
    ("workflow", "ワークフロー"),
    ("miniapp", "ミニアプリ"),
    ("other", "その他"),
];

/// 日本時間の今日。
fn today_jst() -> NaiveDate {
    (Utc::now() + chrono::Duration::hours(9)).date_naive()
}

/// 期間を決めて検証する（省略は今月 1 日〜今日）。
fn resolve_range(q: &UsageQuery, today: NaiveDate) -> Result<(NaiveDate, NaiveDate), ApiError> {
    let to = q.to.unwrap_or(today);
    let from = q.from.unwrap_or_else(|| to.with_day(1).unwrap_or(to));
    if from > to {
        return Err(invalid("開始日が終了日より後になっています"));
    }
    if (to - from).num_days() >= MAX_RANGE_DAYS {
        return Err(invalid("期間は 1 年以内で指定してください"));
    }
    Ok((from, to))
}

/// 利用量ダッシュボード。
#[utoipa::path(
    get,
    path = "/org-admin/ai/usage",
    params(UsageQuery),
    responses(
        (status = 200, description = "利用量", body = UsageResponse),
        (status = 400, description = "期間・利用元が不正"),
        (status = 403, description = "組織管理者ではない"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn get_usage(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<UsageQuery>,
) -> Result<Json<UsageResponse>, ApiError> {
    let gw = gateway(&state)?;
    let (from, to) = resolve_range(&q, today_jst())?;
    if let Some(s) = q.source.as_deref() {
        if !SOURCES.iter().any(|(k, _)| *k == s) {
            return Err(invalid("利用元が不正です"));
        }
    }
    let report = gw
        .usage_report(
            &ctx,
            &llm_gateway::UsageFilter {
                from,
                to,
                model: q.model.clone().filter(|m| !m.is_empty()),
                source: q.source.clone(),
            },
        )
        .await
        .map_err(|e| ApiError::Internal(format!("usage report: {e}")))?;
    let settings = load_settings(gw, &ctx).await?;

    // 日別: 範囲内の全日付を 0 埋めで返す（グラフの欠け＝利用なし、を表現する）。
    let by_day: BTreeMap<NaiveDate, &llm_gateway::UsageDay> =
        report.daily.iter().map(|d| (d.day, d)).collect();
    let daily = from
        .iter_days()
        .take_while(|d| *d <= to)
        .map(|day| match by_day.get(&day) {
            Some(d) => UsageDayDto {
                day,
                cost_usd_micros: d.cost_usd_micros,
                tokens: d.tokens,
                calls: d.calls,
            },
            None => UsageDayDto {
                day,
                cost_usd_micros: 0,
                tokens: 0,
                calls: 0,
            },
        })
        .collect();

    let catalog = gw.catalog();
    let by_model = report
        .by_model
        .iter()
        .map(|r| {
            row(
                r,
                catalog
                    .get(&r.key)
                    .map_or_else(|| r.key.clone(), |m| m.label().to_string()),
            )
        })
        .collect();
    let user_ids: Vec<String> = report.by_user.iter().map(|r| r.key.clone()).collect();
    let names = state
        .directory
        .resolve_display_names(&ctx, &user_ids)
        .await?;
    let by_user = report
        .by_user
        .iter()
        .map(|r| {
            let label = names.get(&r.key).cloned().unwrap_or_else(|| {
                if r.key.is_empty() {
                    "（不明）".to_string()
                } else {
                    r.key.clone()
                }
            });
            row(r, label)
        })
        .collect();
    let by_source = report
        .by_source
        .iter()
        .map(|r| {
            let label = SOURCES
                .iter()
                .find(|(k, _)| *k == r.key)
                .map_or_else(|| r.key.clone(), |(_, l)| (*l).to_string());
            row(r, label)
        })
        .collect();

    Ok(Json(UsageResponse {
        from,
        to,
        jpy_per_usd: settings.jpy_per_usd_centi.map(|c| f64::from(c) / 100.0),
        totals: UsageTotalsDto {
            cost_usd_micros: report.totals.cost_usd_micros,
            prompt_tokens: report.totals.prompt_tokens,
            completion_tokens: report.totals.completion_tokens,
            calls: report.totals.calls,
            users: report.totals.users,
        },
        daily,
        by_model,
        by_user,
        by_source,
    }))
}

fn row(r: &llm_gateway::UsageBreakdown, label: String) -> UsageRowDto {
    UsageRowDto {
        key: r.key.clone(),
        label,
        cost_usd_micros: r.cost_usd_micros,
        tokens: r.tokens,
        calls: r.calls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn q(from: Option<&str>, to: Option<&str>) -> UsageQuery {
        UsageQuery {
            from: from.map(d),
            to: to.map(d),
            model: None,
            source: None,
        }
    }

    #[test]
    fn range_defaults_to_this_month() {
        let (from, to) = resolve_range(&q(None, None), d("2026-10-13")).unwrap();
        assert_eq!((from, to), (d("2026-10-01"), d("2026-10-13")));
    }

    #[test]
    fn range_rejects_reversed_and_too_long() {
        assert!(
            resolve_range(&q(Some("2026-10-05"), Some("2026-10-01")), d("2026-10-13")).is_err()
        );
        assert!(
            resolve_range(&q(Some("2025-01-01"), Some("2026-10-01")), d("2026-10-13")).is_err()
        );
        assert!(resolve_range(&q(Some("2025-10-02"), Some("2026-10-01")), d("2026-10-13")).is_ok());
    }

    #[test]
    fn patch_body_distinguishes_missing_and_null() {
        let r: UpdateAiSettingsRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.auto_model, None);
        let r: UpdateAiSettingsRequest =
            serde_json::from_str(r#"{"auto_model": null, "jpy_per_usd": 150.5}"#).unwrap();
        assert_eq!(r.auto_model, Some(None));
        assert_eq!(r.jpy_per_usd, Some(Some(150.5)));
    }
}
