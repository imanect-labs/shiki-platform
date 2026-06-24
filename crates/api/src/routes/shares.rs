//! 共有 API（Task 1.6 / ReBAC）。
//!
//! ファイル/フォルダを user / role へ viewer/editor で共有/解除する。OpenFGA tuple の
//! 付与/削除として StorageService 経由で実装し（単一チョークポイント・owner 認可・監査）、
//! 共有相手一覧・自分が共有された一覧も提供する。共有解除は PIT-11（HIGHER_CONSISTENCY）で即時。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    routes::files::FileResponse,
    state::AppState,
};

/// 共有先（API 表現）。storage の `ShareTarget` のミラー（utoipa スキーマ生成のため）。
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShareTargetDto {
    /// 個人ユーザー。
    User { id: String },
    /// ロールメンバー全体（配下ロールも含む。org 継承は含まない）。
    Role { id: String },
}

impl From<ShareTargetDto> for storage::ShareTarget {
    fn from(d: ShareTargetDto) -> Self {
        match d {
            ShareTargetDto::User { id } => storage::ShareTarget::User { id },
            ShareTargetDto::Role { id } => storage::ShareTarget::Role { id },
        }
    }
}

impl From<storage::ShareTarget> for ShareTargetDto {
    fn from(t: storage::ShareTarget) -> Self {
        match t {
            storage::ShareTarget::User { id } => ShareTargetDto::User { id },
            storage::ShareTarget::Role { id } => ShareTargetDto::Role { id },
        }
    }
}

/// 共有役割（API 表現）。viewer/editor のみ（閉じた共有語彙）。
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShareRoleDto {
    Viewer,
    Editor,
}

impl From<ShareRoleDto> for storage::ShareRole {
    fn from(d: ShareRoleDto) -> Self {
        match d {
            ShareRoleDto::Viewer => storage::ShareRole::Viewer,
            ShareRoleDto::Editor => storage::ShareRole::Editor,
        }
    }
}

impl From<storage::ShareRole> for ShareRoleDto {
    fn from(r: storage::ShareRole) -> Self {
        match r {
            storage::ShareRole::Viewer => ShareRoleDto::Viewer,
            storage::ShareRole::Editor => ShareRoleDto::Editor,
        }
    }
}

/// 共有/解除リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ShareRequest {
    pub target: ShareTargetDto,
    pub role: ShareRoleDto,
}

/// 共有相手 1 件（誰に・どの役割で）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ShareEntryResponse {
    pub target: ShareTargetDto,
    pub role: ShareRoleDto,
}

impl From<storage::ShareEntry> for ShareEntryResponse {
    fn from(e: storage::ShareEntry) -> Self {
        ShareEntryResponse {
            target: e.target.into(),
            role: e.role.into(),
        }
    }
}

/// ノードを user/role へ共有する（owner 権限・冪等）。
#[utoipa::path(
    put,
    path = "/nodes/{id}/shares",
    params(("id" = Uuid, Path, description = "ノード ID")),
    request_body = ShareRequest,
    responses(
        (status = 204, description = "共有を付与した"),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない）"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn share_node(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ShareRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .share_node(
            &ctx,
            id,
            &req.target.into(),
            req.role.into(),
            trace.as_deref(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 共有を解除する（owner 権限・冪等・即時反映）。
#[utoipa::path(
    delete,
    path = "/nodes/{id}/shares",
    params(("id" = Uuid, Path, description = "ノード ID")),
    request_body = ShareRequest,
    responses(
        (status = 204, description = "共有を解除した"),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない）"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn unshare_node(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ShareRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .unshare_node(
            &ctx,
            id,
            &req.target.into(),
            req.role.into(),
            trace.as_deref(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// このノードの共有相手一覧（owner 権限）。
#[utoipa::path(
    get,
    path = "/nodes/{id}/shares",
    params(("id" = Uuid, Path, description = "ノード ID")),
    responses(
        (status = 200, description = "共有相手一覧", body = [ShareEntryResponse]),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない）"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn list_shares(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ShareEntryResponse>>, ApiError> {
    let entries = state
        .storage
        .list_shares(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(entries.into_iter().map(Into::into).collect()))
}

/// 自分に共有されたノード一覧（自分が作成したものを除く）。
#[utoipa::path(
    get,
    path = "/shares/shared-with-me",
    responses(
        (status = 200, description = "共有されたノード一覧", body = [FileResponse]),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn shared_with_me(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
) -> Result<Json<Vec<FileResponse>>, ApiError> {
    let nodes = state
        .storage
        .list_shared_with_me(&ctx, trace.as_deref())
        .await?;
    Ok(Json(nodes.into_iter().map(Into::into).collect()))
}
