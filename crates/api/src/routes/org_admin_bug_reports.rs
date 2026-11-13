use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json,
};

use super::bug_reports::{error, key, service, PageQuery, ReportCommand};
use crate::{
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};
use shiki_bug_report::{
    BugReport, BugReportPolicy, Command, CommandRequest, ReportDetail, ReportPage,
};
use uuid::Uuid;
pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/org-admin/bug-reports",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(list),
        ),
        RouteDecl::new(
            "/org-admin/bug-reports/settings",
            &["GET", "PUT"],
            AccessPolicy::OrgAdmin,
            || get(settings).put(set_settings),
        ),
        RouteDecl::new(
            "/org-admin/bug-reports/{id}",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(detail),
        ),
        RouteDecl::new(
            "/org-admin/bug-reports/{id}/approve",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(approve),
        ),
        RouteDecl::new(
            "/org-admin/bug-reports/{id}/reject",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(reject),
        ),
        RouteDecl::new(
            "/org-admin/bug-reports/{id}/retry",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(retry),
        ),
    ]
}
#[utoipa::path(get, path="/org-admin/bug-reports", params(PageQuery), responses((status=200, description="成功", body=ReportPage), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn list(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<PageQuery>,
) -> Result<Json<ReportPage>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .list(&ctx, true, q.cursor, trace.0.as_deref().unwrap_or(""))
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(get, path="/org-admin/bug-reports/{id}", params(("id"=Uuid, Path)), responses((status=200, description="成功", body=ReportDetail), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn detail(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ReportDetail>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .detail(&ctx, id, true, trace.0.as_deref().unwrap_or(""))
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(get, path="/org-admin/bug-reports/settings", responses((status=200, description="成功", body=BugReportPolicy), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
) -> Result<Json<BugReportPolicy>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .policy(&ctx, trace.0.as_deref().unwrap_or(""))
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(put, path="/org-admin/bug-reports/settings", request_body=BugReportPolicy, responses((status=200, description="成功", body=BugReportPolicy), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn set_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<BugReportPolicy>,
) -> Result<Json<BugReportPolicy>, Response> {
    Ok(Json(
        service(&state)
            .map_err(IntoResponse::into_response)?
            .set_policy(&ctx, req, trace.0.as_deref().unwrap_or(""))
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post, path="/org-admin/bug-reports/{id}/approve", request_body=ReportCommand, params(("id"=Uuid, Path)), responses((status=200, description="成功", body=BugReport), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn approve(
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
                    command: Command::Approve,
                },
                trace.0.as_deref().unwrap_or(""),
            )
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post, path="/org-admin/bug-reports/{id}/reject", request_body=ReportCommand, params(("id"=Uuid, Path)), responses((status=200, description="成功", body=BugReport), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn reject(
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
                    command: Command::Reject {
                        reason: req
                            .reason
                            .ok_or_else(|| error(shiki_bug_report::Error::Invalid))?,
                    },
                },
                trace.0.as_deref().unwrap_or(""),
            )
            .await
            .map_err(error)?,
    ))
}
#[utoipa::path(post, path="/org-admin/bug-reports/{id}/retry", request_body=ReportCommand, params(("id"=Uuid, Path)), responses((status=200, description="成功", body=BugReport), (status=409, description="変更が競合"), (status=403, description="権限なし")), security(("session"=[])))]
pub async fn retry(
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
                    command: Command::Retry,
                },
                trace.0.as_deref().unwrap_or(""),
            )
            .await
            .map_err(error)?,
    ))
}
