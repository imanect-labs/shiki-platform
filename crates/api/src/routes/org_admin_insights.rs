//! 管理コンソール › AI › 活用分析の API（`/org-admin/insights/*`・#751）。
//!
//! 管理者チェックはルート表のポリシー（`AccessPolicy::OrgAdmin`）が一律に掛ける。範囲
//! （tenant_id ＋ org）は `InsightsService` が `AuthContext` から取る。少人数の区分は伏せ
//! （PIT-17）、個人別は組織の設定でオンのときだけ・開いたら監査に残す。

use axum::{
    extract::{Query, State},
    routing::get,
    Json,
};
use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use utoipa::{IntoParams, ToSchema};

use super::org_admin_ai::{invalid, nullable};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    insights::{
        report::{
            adoption::{Adoption, PersonRow},
            impact::Impact,
            overview::Overview,
            usecases::UseCases,
            ReportError,
        },
        settings::{InsightSettings, InsightSettingsPatch},
        timer::today_jst,
        Range, CATEGORIES, MIN_GROUP_SIZE,
    },
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn org_admin_insights_route_decls() -> Vec<RouteDecl> {
    let r = RouteDecl::new;
    vec![
        r(
            "/org-admin/insights/overview",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(get_overview),
        ),
        r(
            "/org-admin/insights/adoption",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(get_adoption),
        ),
        r(
            "/org-admin/insights/people",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(get_people),
        ),
        r(
            "/org-admin/insights/use-cases",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(get_use_cases),
        ),
        r(
            "/org-admin/insights/impact",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(get_impact),
        ),
        r(
            "/org-admin/insights/departments",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(list_departments),
        ),
        r(
            "/org-admin/insights/settings",
            &["GET", "PUT"],
            AccessPolicy::OrgAdmin,
            || get(get_settings).put(update_settings),
        ),
    ]
}

/// 期間の上限（日）。
const MAX_RANGE_DAYS: i64 = 366;

/// 期間と部署（日付は日本時間・両端含む）。省略は直近 30 日・全体。
#[derive(Debug, Deserialize, IntoParams)]
pub struct InsightsQuery {
    /// 開始日（YYYY-MM-DD）。
    pub from: Option<NaiveDate>,
    /// 終了日（YYYY-MM-DD）。
    pub to: Option<NaiveDate>,
    /// 部署（ロール ID）で絞る。5 人未満の部署は 400。
    pub dept: Option<String>,
}

fn range(q: &InsightsQuery) -> Result<Range, ApiError> {
    let today = today_jst();
    let to = q.to.unwrap_or(today).min(today);
    let from = q.from.unwrap_or(to - Duration::days(29));
    if from > to {
        return Err(invalid("開始日が終了日より後になっています"));
    }
    if (to - from).num_days() >= MAX_RANGE_DAYS {
        return Err(invalid("期間は 1 年以内で指定してください"));
    }
    Ok(Range {
        from,
        to,
        dept: q.dept.clone(),
    })
}

fn map_report(e: ReportError) -> ApiError {
    match e {
        ReportError::GroupTooSmall => invalid(&e.to_string()),
        ReportError::Db(e) => ApiError::Internal(format!("insights: {e}")),
    }
}

fn service(state: &AppState) -> &crate::insights::InsightsService {
    &state.insights
}

/// 利用量画面の円換算レート（LLM 未構成・未設定なら `None`）。
async fn jpy_rate(state: &AppState, ctx: &authz::AuthContext) -> Option<i32> {
    let gw = state.llm.as_ref()?;
    gw.tenant_settings(ctx).await.ok()?.jpy_per_usd_centi
}

/// 概要。
#[utoipa::path(
    get, path = "/org-admin/insights/overview", params(InsightsQuery),
    responses(
        (status = 200, description = "概要", body = Overview),
        (status = 400, description = "期間が不正・部署が 5 人未満"),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_overview(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<InsightsQuery>,
) -> Result<Json<Overview>, ApiError> {
    let r = range(&q)?;
    let rate = jpy_rate(&state, &ctx).await;
    service(&state)
        .overview(&ctx, &r, rate)
        .await
        .map(Json)
        .map_err(map_report)
}

/// 利用者と部署。
#[utoipa::path(
    get, path = "/org-admin/insights/adoption", params(InsightsQuery),
    responses(
        (status = 200, description = "利用者と部署", body = Adoption),
        (status = 400, description = "期間が不正・部署が 5 人未満"),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_adoption(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<InsightsQuery>,
) -> Result<Json<Adoption>, ApiError> {
    let r = range(&q)?;
    service(&state)
        .adoption(&ctx, &r)
        .await
        .map(Json)
        .map_err(map_report)
}

/// 個人別（組織の設定でオンのときだけ・閲覧は監査に残す）。
#[utoipa::path(
    get, path = "/org-admin/insights/people", params(InsightsQuery),
    responses(
        (status = 200, description = "個人別（会話本文は含まない）", body = Vec<PersonRow>),
        (status = 400, description = "期間が不正・部署が 5 人未満"),
        (status = 403, description = "組織管理者ではない・個人別の表示がオフ"),
    ),
    security(("session" = [])),
)]
pub async fn get_people(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<InsightsQuery>,
) -> Result<Json<Vec<PersonRow>>, ApiError> {
    let r = range(&q)?;
    let rows = service(&state)
        .people(&ctx, &r)
        .await
        .map_err(map_report)?
        .ok_or(ApiError::Forbidden)?;
    // 誰がいつ個人別を見たかを残す。残せなければ見せない（閲覧は必ず監査に残す約束）。
    let object_id = format!("{}|{}", ctx.tenant_id, ctx.org);
    state
        .audit
        .record(
            &ctx,
            AuditEntry {
                action: "org_admin.insights.people.view",
                object_type: "organization",
                object_id: &object_id,
                decision: Decision::Allow,
                trace_id: trace.as_deref(),
                metadata: json!({
                    "from": r.from, "to": r.to, "dept": r.dept, "rows": rows.len(),
                }),
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "個人別の閲覧の監査記録に失敗（一覧は返さない）");
            ApiError::Internal("監査ログに記録できなかったため表示を中止しました".into())
        })?;
    Ok(Json(rows))
}

/// 用途。
#[utoipa::path(
    get, path = "/org-admin/insights/use-cases", params(InsightsQuery),
    responses(
        (status = 200, description = "用途", body = UseCases),
        (status = 400, description = "期間が不正・部署が 5 人未満"),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_use_cases(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<InsightsQuery>,
) -> Result<Json<UseCases>, ApiError> {
    let r = range(&q)?;
    service(&state)
        .use_cases(&ctx, &r)
        .await
        .map(Json)
        .map_err(map_report)
}

/// 効果。
#[utoipa::path(
    get, path = "/org-admin/insights/impact", params(InsightsQuery),
    responses(
        (status = 200, description = "効果（削減時間は前提にもとづく推定値）", body = Impact),
        (status = 400, description = "期間が不正・部署が 5 人未満"),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_impact(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<InsightsQuery>,
) -> Result<Json<Impact>, ApiError> {
    let r = range(&q)?;
    let rate = jpy_rate(&state, &ctx).await;
    service(&state)
        .impact(&ctx, &r, rate)
        .await
        .map(Json)
        .map_err(map_report)
}

/// 部署 1 件（絞り込みの選択肢）。
#[derive(Debug, Serialize, ToSchema)]
pub struct InsightDepartment {
    pub role_id: String,
    pub label: String,
    pub members: i64,
    /// 5 人未満で絞り込みに使えない。
    pub too_small: bool,
}

/// 部署の一覧。
#[utoipa::path(
    get, path = "/org-admin/insights/departments",
    responses(
        (status = 200, description = "部署（groups 由来）", body = Vec<InsightDepartment>),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn list_departments(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<Vec<InsightDepartment>>, ApiError> {
    let rows = service(&state)
        .departments(&ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("insights: {e}")))?;
    Ok(Json(
        rows.into_iter()
            .map(|(role_id, label, members)| InsightDepartment {
                role_id,
                label,
                members,
                too_small: members < MIN_GROUP_SIZE,
            })
            .collect(),
    ))
}

/// 用途ごとの前提時間。
#[derive(Debug, Serialize, ToSchema)]
pub struct TaskMinutes {
    pub key: String,
    pub label: String,
    pub minutes: i32,
    pub default_minutes: i32,
}

/// 分析の設定。
#[derive(Debug, Serialize, ToSchema)]
pub struct InsightSettingsDto {
    /// 会話の用途分類（本文を AI に渡す処理）。
    pub classify_enabled: bool,
    /// 個人単位の表示。
    pub individual_view: bool,
    /// 金額換算の時給（円）。
    pub hourly_wage_jpy: Option<i32>,
    pub minutes: Vec<TaskMinutes>,
    pub workflow_minutes: i32,
}

fn settings_dto(s: &InsightSettings) -> InsightSettingsDto {
    InsightSettingsDto {
        classify_enabled: s.classify_enabled,
        individual_view: s.individual_view,
        hourly_wage_jpy: s.hourly_wage_jpy,
        minutes: CATEGORIES
            .iter()
            .map(|(key, label, default)| TaskMinutes {
                key: (*key).to_string(),
                label: (*label).to_string(),
                minutes: s.minutes_for(key),
                default_minutes: *default,
            })
            .collect(),
        workflow_minutes: s.workflow_minutes,
    }
}

/// 設定の取得。
#[utoipa::path(
    get, path = "/org-admin/insights/settings",
    responses(
        (status = 200, description = "分析の設定", body = InsightSettingsDto),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<InsightSettingsDto>, ApiError> {
    let s = service(&state)
        .settings(&ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("insights: {e}")))?;
    Ok(Json(settings_dto(&s)))
}

/// 前提時間 1 件の変更。
#[derive(Debug, Deserialize, ToSchema)]
pub struct TaskMinutesUpdate {
    pub key: String,
    pub minutes: i32,
}

/// 設定の更新（省略した項目は変えない）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateInsightSettingsRequest {
    pub classify_enabled: Option<bool>,
    pub individual_view: Option<bool>,
    /// 時給（円）。`null` で金額換算をやめる。
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i32>, nullable)]
    pub hourly_wage_jpy: Option<Option<i32>>,
    #[serde(default)]
    pub minutes: Vec<TaskMinutesUpdate>,
    pub workflow_minutes: Option<i32>,
}

/// 設定の更新（監査あり）。
#[utoipa::path(
    put, path = "/org-admin/insights/settings", request_body = UpdateInsightSettingsRequest,
    responses(
        (status = 200, description = "更新後の設定", body = InsightSettingsDto),
        (status = 400, description = "範囲外・不明な用途"),
        (status = 403, description = "組織管理者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn update_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<UpdateInsightSettingsRequest>,
) -> Result<Json<InsightSettingsDto>, ApiError> {
    let patch = InsightSettingsPatch {
        classify_enabled: req.classify_enabled,
        individual_view: req.individual_view,
        hourly_wage_jpy: req.hourly_wage_jpy,
        minutes_per_task: req
            .minutes
            .iter()
            .map(|m| (m.key.clone(), m.minutes))
            .collect(),
        workflow_minutes: req.workflow_minutes,
    };
    let next = match service(&state)
        .update_settings(&ctx, &patch)
        .await
        .map_err(|e| ApiError::Internal(format!("insights: {e}")))?
    {
        Ok(n) => n,
        Err(e) => return Err(invalid(&e.to_string())),
    };
    let object_id = format!("{}|{}", ctx.tenant_id, ctx.org);
    if let Err(e) = state
        .audit
        .record(
            &ctx,
            AuditEntry {
                action: "org_admin.insights_settings.update",
                object_type: "ai_insight_settings",
                object_id: &object_id,
                decision: Decision::Allow,
                trace_id: trace.as_deref(),
                metadata: json!({
                    "classify_enabled": req.classify_enabled,
                    "individual_view": req.individual_view,
                    "hourly_wage_jpy": req.hourly_wage_jpy,
                    "minutes": req.minutes.iter().map(|m| json!({"key": m.key, "minutes": m.minutes})).collect::<Vec<_>>(),
                    "workflow_minutes": req.workflow_minutes,
                }),
            },
        )
        .await
    {
        tracing::error!(error = %e, "分析の設定変更の監査記録に失敗");
    }
    Ok(Json(settings_dto(&next)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(from: Option<NaiveDate>, to: Option<NaiveDate>) -> InsightsQuery {
        InsightsQuery {
            from,
            to,
            dept: None,
        }
    }

    #[test]
    fn range_defaults_to_last_30_days_and_validates() {
        let today = today_jst();
        let r = range(&q(None, None)).unwrap();
        assert_eq!((r.from, r.to), (today - Duration::days(29), today));
        // 未来の終了日は今日に丸める。
        let r = range(&q(None, Some(today + Duration::days(5)))).unwrap();
        assert_eq!(r.to, today);
        assert!(range(&q(Some(today), Some(today - Duration::days(1)))).is_err());
        assert!(range(&q(Some(today - Duration::days(366)), Some(today))).is_err());
        assert!(range(&q(Some(today - Duration::days(365)), Some(today))).is_ok());
    }

    #[test]
    fn settings_dto_lists_every_category() {
        let dto = settings_dto(&InsightSettings::default());
        assert_eq!(dto.minutes.len(), CATEGORIES.len());
        assert!(dto.minutes.iter().all(|m| m.minutes == m.default_minutes));
        assert!(!dto.classify_enabled && !dto.individual_view);
    }
}
