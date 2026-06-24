//! 共有 API（Task 1.6 / ReBAC）。
//!
//! ファイル/フォルダを user へ viewer/editor で共有/解除する（role 共有は #76 で defer）。OpenFGA tuple の
//! 付与/削除として StorageService 経由で実装し（単一チョークポイント・owner 認可・監査）、
//! 共有相手一覧・自分が共有された一覧も提供する。共有解除は PIT-11（HIGHER_CONSISTENCY）で即時。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use storage::{ShareEntry, ShareRole, ShareTarget};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    routes::files::NodeResponse,
    state::AppState,
};

/// 共有/解除リクエスト。共有語彙（`ShareTarget`/`ShareRole`）は storage 側の単一定義を
/// そのまま使う（手書きミラーを作らない・codegen が正）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ShareRequest {
    pub target: ShareTarget,
    pub role: ShareRole,
}

/// ノードを user へ共有する（owner 権限・冪等）。
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
        .share_node(&ctx, id, &req.target, req.role, trace.as_deref())
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
        .unshare_node(&ctx, id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// このノードの共有相手一覧（owner 権限）。
#[utoipa::path(
    get,
    path = "/nodes/{id}/shares",
    params(("id" = Uuid, Path, description = "ノード ID")),
    responses(
        (status = 200, description = "共有相手一覧", body = [ShareEntry]),
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
) -> Result<Json<Vec<ShareEntry>>, ApiError> {
    let entries = state
        .storage
        .list_shares(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(entries))
}

/// 自分に共有されたノード一覧（自分が作成したものを除く）。
#[utoipa::path(
    get,
    path = "/shares/shared-with-me",
    responses(
        (status = 200, description = "共有されたノード一覧", body = [NodeResponse]),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn shared_with_me(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
) -> Result<Json<Vec<NodeResponse>>, ApiError> {
    let nodes = state
        .storage
        .list_shared_with_me(&ctx, trace.as_deref())
        .await?;
    Ok(Json(nodes.into_iter().map(Into::into).collect()))
}
