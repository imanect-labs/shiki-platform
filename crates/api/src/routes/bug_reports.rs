use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json,
};
use serde::Deserialize;
use shiki_bug_report::{
    BugReport, Command, CommandRequest, ReportContent, ReportDetail, ReportPage, ReporterPolicy,
};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
pub(crate) fn service(state: &AppState) -> Result<&shiki_bug_report::BugReportService, ApiError> {
    state
        .bug_report
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("不具合報告は利用できません".into()))
}
pub(crate) fn error(e: shiki_bug_report::Error) -> Response {
    use shiki_bug_report::Error;
    if matches!(e, Error::RateLimited) {
        let mut response = ApiError::RateLimited(
            "報告は1時間に5件、1日に20件までです。時間をおいてお試しください。".into(),
        )
        .into_response();
        response
            .headers_mut()
            .insert("retry-after", axum::http::HeaderValue::from_static("3600"));
        return response;
    }
    match e {
        Error::Forbidden => ApiError::Forbidden,
        Error::NotFound => ApiError::NotFound,
        Error::Conflict => ApiError::Conflict,
        Error::Invalid => {
            ApiError::BadRequest("入力内容を確認してください。URL・HTML・添付は送れません".into())
        }
        Error::Disabled => ApiError::ServiceUnavailable("報告の受付は停止しています".into()),
        other => ApiError::Internal(other.to_string()),
    }
    .into_response()
}
#[derive(Deserialize, IntoParams)]
pub struct PageQuery {
    pub cursor: Option<Uuid>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateReport {
    pub expected_rev: i64,
    pub content: ReportContent,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportCommand {
    pub expected_rev: i64,
    pub policy_rev: Option<i64>,
    pub consent: Option<bool>,
    pub notice_version: Option<String>,
    pub reason: Option<String>,
}
pub(crate) fn key(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_owned)
        .ok_or_else(|| ApiError::BadRequest("Idempotency-Key が必要です".into()))
}
pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/bug-reports",
            &["GET", "POST"],
            AccessPolicy::Session,
            || get(list).post(create),
        ),
        RouteDecl::new(
            "/bug-reports/policy",
            &["GET"],
            AccessPolicy::Session,
            || get(policy),
        ),
        RouteDecl::new(
            "/bug-reports/{id}",
            &["GET", "PATCH"],
            AccessPolicy::Session,
            || get(detail).patch(update),
        ),
        RouteDecl::new(
            "/bug-reports/{id}/submit",
            &["POST"],
            AccessPolicy::Session,
            || post(submit),
        ),
        RouteDecl::new(
            "/bug-reports/{id}/cancel",
            &["POST"],
            AccessPolicy::Session,
            || post(cancel),
        ),
        RouteDecl::new(
            "/bug-reports/{id}/reopen",
            &["POST"],
            AccessPolicy::Session,
            || post(reopen),
        ),
    ]
}
#[utoipa::path(get, path="/bug-reports", params(PageQuery), responses((status=200, description="成功", body=ReportPage), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn list(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<PageQuery>,
) -> Result<Json<ReportPage>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .list(&ctx, false, q.cursor, trace.0.as_deref().unwrap_or(""))
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(get, path="/bug-reports/policy", responses((status=200, description="成功", body=ReporterPolicy), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn policy(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
) -> Result<Json<ReporterPolicy>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .reporter_policy(&ctx, trace.0.as_deref().unwrap_or(""))
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post, path="/bug-reports", request_body=ReportContent, responses((status=200, description="成功", body=BugReport), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn create(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(content): Json<ReportContent>,
) -> Result<Json<BugReport>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .create(&ctx, content, trace.0.as_deref().unwrap_or(""))
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(get, path="/bug-reports/{id}", params(("id"=Uuid, Path)), responses((status=200, description="成功", body=ReportDetail), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn detail(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ReportDetail>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .detail(&ctx, id, false, trace.0.as_deref().unwrap_or(""))
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(patch, path="/bug-reports/{id}", request_body=UpdateReport, params(("id"=Uuid, Path)), responses((status=200, description="成功", body=BugReport), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn update(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateReport>,
) -> Result<Json<BugReport>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .update(
                &ctx,
                id,
                req.expected_rev,
                req.content,
                trace.0.as_deref().unwrap_or(""),
            )
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post, path="/bug-reports/{id}/submit", request_body=ReportCommand, params(("id"=Uuid, Path)), responses((status=200, description="成功", body=BugReport), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn submit(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<ReportCommand>,
) -> Result<Json<BugReport>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .command(
                &ctx,
                CommandRequest {
                    id,
                    expected_rev: req.expected_rev,
                    key: key(&headers).map_err(IntoResponse::into_response)?,
                    command: Command::Submit {
                        policy_rev: req
                            .policy_rev
                            .ok_or_else(|| error(shiki_bug_report::Error::Invalid))?,
                        consent: req
                            .consent
                            .ok_or_else(|| error(shiki_bug_report::Error::Invalid))?,
                        notice_version: req
                            .notice_version
                            .ok_or_else(|| error(shiki_bug_report::Error::Invalid))?,
                    },
                },
                trace.0.as_deref().unwrap_or(""),
            )
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post, path="/bug-reports/{id}/cancel", request_body=ReportCommand, params(("id"=Uuid, Path)), responses((status=200, description="成功", body=BugReport), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn cancel(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<ReportCommand>,
) -> Result<Json<BugReport>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .command(
                &ctx,
                CommandRequest {
                    id,
                    expected_rev: req.expected_rev,
                    key: key(&headers).map_err(IntoResponse::into_response)?,
                    command: Command::Cancel,
                },
                trace.0.as_deref().unwrap_or(""),
            )
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post, path="/bug-reports/{id}/reopen", request_body=ReportCommand, params(("id"=Uuid, Path)), responses((status=200, description="成功", body=BugReport), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn reopen(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<ReportCommand>,
) -> Result<Json<BugReport>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .command(
                &ctx,
                CommandRequest {
                    id,
                    expected_rev: req.expected_rev,
                    key: key(&headers).map_err(IntoResponse::into_response)?,
                    command: Command::Reopen,
                },
                trace.0.as_deref().unwrap_or(""),
            )
            .await
            .map_err(error)?,
    ))
}
