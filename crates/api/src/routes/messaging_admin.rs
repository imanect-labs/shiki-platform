//! 職員間メッセージ API — 組織の設定・監査担当者の閲覧とエクスポート・閲覧の承認（Task 14.9 / 14.18）。
//!
//! 監査担当者の閲覧は `/org-audit/*`（`organization#audit_viewer` の middleware）に置き、通常の
//! `/messaging/*` の読取経路とは分ける（PIT-65）。サービス側でも監査担当者を引き直す（多層）。
//! エクスポートは期限付き（承認レコードの期間内）のダウンロードで返し、ドライブには保存しない。

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json,
};
use chrono::{DateTime, Utc};
use messaging::{AccessGrant, AccessRequest, AdminChannel, AdminMessage, MessagingSettings};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::messaging::{error, service};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/org-admin/messaging/settings",
            &["GET", "PUT"],
            AccessPolicy::OrgAdmin,
            || get(get_settings).put(put_settings),
        ),
        RouteDecl::new(
            "/org-audit/messaging/channels",
            &["GET"],
            AccessPolicy::OrgAuditor,
            || get(audit_channels),
        ),
        RouteDecl::new(
            "/org-audit/messaging/channels/{id}/messages",
            &["GET"],
            AccessPolicy::OrgAuditor,
            || get(audit_messages),
        ),
        RouteDecl::new(
            "/org-audit/messaging/channels/{id}/export",
            &["GET"],
            AccessPolicy::OrgAuditor,
            || get(audit_export),
        ),
        RouteDecl::new(
            "/org-audit/messaging/access-requests",
            &["POST"],
            AccessPolicy::OrgAuditor,
            || post(request_access),
        ),
        RouteDecl::new(
            "/messaging/access-requests",
            &["GET"],
            AccessPolicy::Session,
            || get(list_grants),
        ),
        RouteDecl::new(
            "/messaging/access-requests/{id}/decision",
            &["POST"],
            AccessPolicy::Session,
            || post(decide),
        ),
        RouteDecl::new(
            "/messaging/access-requests/{id}/revoke",
            &["POST"],
            AccessPolicy::Session,
            || post(revoke),
        ),
    ]
}

/// メッセージの設定（組織の管理者）。
#[utoipa::path(get, path = "/org-admin/messaging/settings",
    responses((status = 200, description = "成功", body = MessagingSettings), (status = 403, description = "組織の管理者ではない")),
    security(("session" = [])))]
pub async fn get_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<MessagingSettings>, ApiError> {
    Ok(Json(service(&state)?.settings(&ctx).await.map_err(error)?))
}

/// 設定を変える（保存期間は監査ログより短くできない・`rev` で楽観ロック）。
#[utoipa::path(put, path = "/org-admin/messaging/settings", request_body = MessagingSettings,
    responses((status = 200, description = "成功", body = MessagingSettings), (status = 400, description = "入力が不正"),
        (status = 409, description = "他の人が先に変更した")),
    security(("session" = [])))]
pub async fn put_settings(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(body): Json<MessagingSettings>,
) -> Result<Json<MessagingSettings>, ApiError> {
    Ok(Json(
        service(&state)?
            .update_settings(&ctx, body, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ChannelsQuery {
    #[serde(default)]
    pub q: String,
}

/// 監査担当者から見たチャンネルの一覧（名前と種別だけ）。
#[utoipa::path(get, path = "/org-audit/messaging/channels", params(ChannelsQuery),
    responses((status = 200, description = "成功", body = Vec<AdminChannel>), (status = 403, description = "監査担当者ではない")),
    security(("session" = [])))]
pub async fn audit_channels(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<ChannelsQuery>,
) -> Result<Json<Vec<AdminChannel>>, ApiError> {
    Ok(Json(
        service(&state)?
            .admin_channels(&ctx, &q.q, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct RangeQuery {
    pub from: DateTime<Utc>,
    pub until: DateTime<Utc>,
}

/// 監査担当者の閲覧（公開は承認不要・DM と非公開は承認レコードの期間内だけ）。
#[utoipa::path(get, path = "/org-audit/messaging/channels/{id}/messages", params(("id" = Uuid, Path), RangeQuery),
    responses((status = 200, description = "成功", body = Vec<AdminMessage>), (status = 403, description = "承認が無い・期間外")),
    security(("session" = [])))]
pub async fn audit_messages(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Vec<AdminMessage>>, ApiError> {
    Ok(Json(
        service(&state)?
            .admin_read(&ctx, id, q.from, q.until, false, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

/// エクスポート（JSON のダウンロード・ドライブには保存しない）。
#[utoipa::path(get, path = "/org-audit/messaging/channels/{id}/export", params(("id" = Uuid, Path), RangeQuery),
    responses((status = 200, description = "JSON のダウンロード", body = Vec<AdminMessage>), (status = 403, description = "承認が無い・期間外")),
    security(("session" = [])))]
pub async fn audit_export(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Response, ApiError> {
    let messages = service(&state)?
        .admin_read(&ctx, id, q.from, q.until, true, trace.0.as_deref())
        .await
        .map_err(error)?;
    let body =
        serde_json::to_vec_pretty(&messages).map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"messages-{id}.json\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

/// DM・非公開チャンネルの閲覧を申請する（監査担当者）。
#[utoipa::path(post, path = "/org-audit/messaging/access-requests", request_body = AccessRequest,
    responses((status = 200, description = "申請した", body = AccessGrant), (status = 400, description = "入力が不正")),
    security(("session" = [])))]
pub async fn request_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(body): Json<AccessRequest>,
) -> Result<Json<AccessGrant>, ApiError> {
    Ok(Json(
        service(&state)?
            .request_access(&ctx, body, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

/// 閲覧の申請の一覧（承認者は全件・監査担当者は自分の申請）。
#[utoipa::path(get, path = "/messaging/access-requests",
    responses((status = 200, description = "成功", body = Vec<AccessGrant>), (status = 403, description = "承認者でも監査担当者でもない")),
    security(("session" = [])))]
pub async fn list_grants(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<Vec<AccessGrant>>, ApiError> {
    Ok(Json(
        service(&state)?.list_grants(&ctx).await.map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DecisionBody {
    pub approve: bool,
}

/// 承認・却下する（テナントが指名した承認者・申請者本人は不可）。
#[utoipa::path(post, path = "/messaging/access-requests/{id}/decision", params(("id" = Uuid, Path)), request_body = DecisionBody,
    responses((status = 200, description = "判断した", body = AccessGrant), (status = 403, description = "承認者ではない・自分の申請")),
    security(("session" = [])))]
pub async fn decide(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(body): Json<DecisionBody>,
) -> Result<Json<AccessGrant>, ApiError> {
    Ok(Json(
        service(&state)?
            .decide_access(&ctx, id, body.approve, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

/// 承認を取り消す（申請者か承認者）。
#[utoipa::path(post, path = "/messaging/access-requests/{id}/revoke", params(("id" = Uuid, Path)),
    responses((status = 200, description = "取り消した", body = AccessGrant), (status = 403, description = "取り消せない")),
    security(("session" = [])))]
pub async fn revoke(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<AccessGrant>, ApiError> {
    Ok(Json(
        service(&state)?
            .revoke_access(&ctx, id, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}
