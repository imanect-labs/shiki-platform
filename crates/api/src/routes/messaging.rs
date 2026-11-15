//! 職員間メッセージ API — チャンネル・参加者・DM（Phase 14・Task 14.3・design §4.14）。
//!
//! 認可・監査・配信の合図は [`messaging::MessagingService`] が持つ（ハンドラに権限判定を散らさない）。
//! 発言・検索・配信は [`super::messaging_messages`]、エージェントは [`super::messaging_agent`]、
//! 設定・監査担当者の閲覧は [`super::messaging_admin`]。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json,
};
use messaging::{
    ChannelView, CreateChannel, MemberView, MessagingService, NotifyLevel, UpdateChannel,
};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn service(state: &AppState) -> Result<&Arc<MessagingService>, ApiError> {
    state
        .messaging
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("メッセージは利用できません".into()))
}

/// messaging のエラーを HTTP へ写す（読めないものは 404 で存在を開示しない）。
pub(crate) fn error(e: messaging::Error) -> ApiError {
    use messaging::Error;
    match e {
        Error::NotFound => ApiError::NotFound,
        Error::Forbidden(m) => ApiError::ForbiddenJson(serde_json::json!({ "message": m })),
        Error::Conflict(m) => ApiError::ConflictJson(serde_json::json!({ "message": m })),
        Error::Invalid(m) => ApiError::BadRequest(m),
        Error::Unavailable(m) => ApiError::ServiceUnavailable(m.into()),
        // 認可基盤の失敗は fail-closed（拒否側）に倒す。
        Error::Authz(e) => ApiError::from(e),
        other => ApiError::Internal(other.to_string()),
    }
}

pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/messaging/channels",
            &["GET", "POST"],
            AccessPolicy::Session,
            || get(list_channels).post(create_channel),
        ),
        RouteDecl::new(
            "/messaging/channels/browse",
            &["GET"],
            AccessPolicy::Session,
            || get(browse_channels),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}",
            &["GET", "PATCH"],
            AccessPolicy::Session,
            || get(get_channel).patch(update_channel),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/archive",
            &["PUT"],
            AccessPolicy::Session,
            || put(set_archived),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/join",
            &["POST"],
            AccessPolicy::Session,
            || post(join_channel),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/members",
            &["GET", "POST"],
            AccessPolicy::Session,
            || get(list_members).post(invite_members),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/members/{user_id}",
            &["PATCH", "DELETE"],
            AccessPolicy::Session,
            || delete(remove_member).patch(update_member),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/notify",
            &["PUT"],
            AccessPolicy::Session,
            || put(set_notify),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/hide",
            &["POST"],
            AccessPolicy::Session,
            || post(hide_dm),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/read",
            &["POST"],
            AccessPolicy::Session,
            || post(mark_read),
        ),
        RouteDecl::new("/messaging/dms", &["POST"], AccessPolicy::Session, || {
            post(open_dm)
        }),
    ]
}

/// 自分が参加しているチャンネルと DM（未読・メンション数つき）。
#[utoipa::path(get, path = "/messaging/channels",
    responses((status = 200, description = "成功", body = Vec<ChannelView>), (status = 403, description = "組織のメンバーではない")),
    security(("session" = [])))]
pub async fn list_channels(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<Vec<ChannelView>>, ApiError> {
    Ok(Json(
        service(&state)?.list_channels(&ctx).await.map_err(error)?,
    ))
}

/// チャンネルを作る（公開・非公開）。作成者が管理者になる。
#[utoipa::path(post, path = "/messaging/channels", request_body = CreateChannel,
    responses((status = 200, description = "作成した", body = ChannelView), (status = 400, description = "入力が不正"),
        (status = 403, description = "作る権限が無い"), (status = 409, description = "同じ名前のチャンネルがある")),
    security(("session" = [])))]
pub async fn create_channel(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(input): Json<CreateChannel>,
) -> Result<Json<ChannelView>, ApiError> {
    Ok(Json(
        service(&state)?
            .create_channel(&ctx, input, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct BrowseQuery {
    /// 名前・トピックの部分一致。
    #[serde(default)]
    pub q: String,
    /// アーカイブ済みも含めるか。
    #[serde(default)]
    pub archived: bool,
}

/// 組織の公開チャンネルを探す（参加していなくても読める・参加できる）。
#[utoipa::path(get, path = "/messaging/channels/browse", params(BrowseQuery),
    responses((status = 200, description = "成功", body = Vec<ChannelView>)),
    security(("session" = [])))]
pub async fn browse_channels(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<BrowseQuery>,
) -> Result<Json<Vec<ChannelView>>, ApiError> {
    Ok(Json(
        service(&state)?
            .browse_public(&ctx, &q.q, q.archived)
            .await
            .map_err(error)?,
    ))
}

/// 1 つのチャンネル（読めなければ 404）。
#[utoipa::path(get, path = "/messaging/channels/{id}", params(("id" = Uuid, Path)),
    responses((status = 200, description = "成功", body = ChannelView), (status = 404, description = "無い・読めない")),
    security(("session" = [])))]
pub async fn get_channel(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ChannelView>, ApiError> {
    Ok(Json(
        service(&state)?
            .channel_view(&ctx, id)
            .await
            .map_err(error)?,
    ))
}

/// 名前・トピックを変える（チャンネルの管理者）。
#[utoipa::path(patch, path = "/messaging/channels/{id}", params(("id" = Uuid, Path)), request_body = UpdateChannel,
    responses((status = 200, description = "成功", body = ChannelView), (status = 403, description = "管理者ではない"),
        (status = 404, description = "無い・読めない"), (status = 409, description = "同じ名前のチャンネルがある")),
    security(("session" = [])))]
pub async fn update_channel(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateChannel>,
) -> Result<Json<ChannelView>, ApiError> {
    Ok(Json(
        service(&state)?
            .update_channel(&ctx, id, input, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ArchiveBody {
    pub archived: bool,
}

/// アーカイブする／戻す（チャンネルの管理者）。
#[utoipa::path(put, path = "/messaging/channels/{id}/archive", params(("id" = Uuid, Path)), request_body = ArchiveBody,
    responses((status = 200, description = "成功", body = ChannelView), (status = 403, description = "管理者ではない")),
    security(("session" = [])))]
pub async fn set_archived(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(body): Json<ArchiveBody>,
) -> Result<Json<ChannelView>, ApiError> {
    Ok(Json(
        service(&state)?
            .set_archived(&ctx, id, body.archived, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

/// 公開チャンネルに参加する。
#[utoipa::path(post, path = "/messaging/channels/{id}/join", params(("id" = Uuid, Path)),
    responses((status = 200, description = "参加した", body = ChannelView), (status = 403, description = "招待が必要")),
    security(("session" = [])))]
pub async fn join_channel(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ChannelView>, ApiError> {
    Ok(Json(
        service(&state)?
            .join(&ctx, id, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

/// 参加者の一覧。
#[utoipa::path(get, path = "/messaging/channels/{id}/members", params(("id" = Uuid, Path)),
    responses((status = 200, description = "成功", body = Vec<MemberView>), (status = 404, description = "無い・読めない")),
    security(("session" = [])))]
pub async fn list_members(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<MemberView>>, ApiError> {
    Ok(Json(
        service(&state)?.members(&ctx, id).await.map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct InviteBody {
    pub user_ids: Vec<String>,
}

/// 招待する（公開は参加者なら誰でも・非公開は管理者だけ・DM には招待できない）。
#[utoipa::path(post, path = "/messaging/channels/{id}/members", params(("id" = Uuid, Path)), request_body = InviteBody,
    responses((status = 200, description = "成功", body = Vec<MemberView>), (status = 403, description = "招待できない")),
    security(("session" = [])))]
pub async fn invite_members(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(body): Json<InviteBody>,
) -> Result<Json<Vec<MemberView>>, ApiError> {
    Ok(Json(
        service(&state)?
            .invite(&ctx, id, body.user_ids, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

/// 退出する（自分）・外す（管理者）。DM は退出できず、最後の管理者も退出できない。
#[utoipa::path(delete, path = "/messaging/channels/{id}/members/{user_id}",
    params(("id" = Uuid, Path), ("user_id" = String, Path)),
    responses((status = 204, description = "外した"), (status = 403, description = "外せない")),
    security(("session" = [])))]
pub async fn remove_member(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, user)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    service(&state)?
        .remove_member(&ctx, id, &user, trace.0.as_deref())
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MemberRoleBody {
    /// 管理者にする／外す。
    #[serde(default)]
    pub admin: Option<bool>,
    /// 投稿者にする／外す（アナウンス専用チャンネルだけ）。
    #[serde(default)]
    pub poster: Option<bool>,
}

/// 管理者・投稿者を付け外しする（チャンネルの管理者）。
#[utoipa::path(patch, path = "/messaging/channels/{id}/members/{user_id}",
    params(("id" = Uuid, Path), ("user_id" = String, Path)), request_body = MemberRoleBody,
    responses((status = 200, description = "成功", body = Vec<MemberView>), (status = 403, description = "管理者ではない")),
    security(("session" = [])))]
pub async fn update_member(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, user)): Path<(Uuid, String)>,
    Json(body): Json<MemberRoleBody>,
) -> Result<Json<Vec<MemberView>>, ApiError> {
    Ok(Json(
        service(&state)?
            .set_member_role(&ctx, id, &user, body.admin, body.poster, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NotifyBody {
    pub level: NotifyLevel,
}

/// 通知の受け方（すべて／メンションだけ／なし）。
#[utoipa::path(put, path = "/messaging/channels/{id}/notify", params(("id" = Uuid, Path)), request_body = NotifyBody,
    responses((status = 204, description = "変更した"), (status = 403, description = "参加していない")),
    security(("session" = [])))]
pub async fn set_notify(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
    Json(body): Json<NotifyBody>,
) -> Result<StatusCode, ApiError> {
    service(&state)?
        .set_notify(&ctx, id, body.level)
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// DM を一覧から隠す（新しい発言が来たら戻る）。
#[utoipa::path(post, path = "/messaging/channels/{id}/hide", params(("id" = Uuid, Path)),
    responses((status = 204, description = "隠した"), (status = 400, description = "DM ではない")),
    security(("session" = [])))]
pub async fn hide_dm(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    service(&state)?.hide_dm(&ctx, id).await.map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReadBody {
    /// ここまで読んだ（省略時は今）。
    #[serde(default)]
    pub at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 既読位置を進める。
#[utoipa::path(post, path = "/messaging/channels/{id}/read", params(("id" = Uuid, Path)), request_body = ReadBody,
    responses((status = 204, description = "記録した")),
    security(("session" = [])))]
pub async fn mark_read(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
    Json(body): Json<ReadBody>,
) -> Result<StatusCode, ApiError> {
    service(&state)?
        .mark_read(&ctx, id, body.at.unwrap_or_else(chrono::Utc::now))
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct OpenDmBody {
    pub user_id: String,
}

/// 1 対 1 の DM を開く（同じ 2 人の DM は 1 つ）。
#[utoipa::path(post, path = "/messaging/dms", request_body = OpenDmBody,
    responses((status = 200, description = "成功", body = ChannelView), (status = 400, description = "相手が組織のメンバーではない")),
    security(("session" = [])))]
pub async fn open_dm(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(body): Json<OpenDmBody>,
) -> Result<Json<ChannelView>, ApiError> {
    Ok(Json(
        service(&state)?
            .open_dm(&ctx, &body.user_id, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}
