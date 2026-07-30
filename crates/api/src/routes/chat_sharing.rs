//! スレッドの ReBAC 共有 API（#37・500 行規約で `chat.rs` から分離）。
//!
//! 認可は `ChatStore`（owner 要求＋監査）の既存チョークポイントに委ねる。共有役割は
//! viewer/commenter/editor の閉集合（owner の横展開を防ぐ）。editor 共有は自律 run の
//! ワークスペースフォルダにも editor を伝播する（store 側の責務）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::chat::chat_store;
use super::chat_dto::{ShareThreadRequest, ThreadShareEntry, ThreadSharesResponse};

/// スレッドを共有する（owner 権限）。
#[utoipa::path(
    post, path = "/threads/{id}/shares",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    request_body = ShareThreadRequest,
    responses(
        (status = 204, description = "共有を付与"),
        (status = 403, description = "owner でない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn share_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ShareThreadRequest>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .share_thread(&ctx, id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 共有を解除する（owner 権限・冪等）。
#[utoipa::path(
    delete, path = "/threads/{id}/shares",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    request_body = ShareThreadRequest,
    responses(
        (status = 204, description = "共有を解除"),
        (status = 403, description = "owner でない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn unshare_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ShareThreadRequest>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .unshare_thread(&ctx, id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 共有相手一覧（owner 権限）。
#[utoipa::path(
    get, path = "/threads/{id}/shares",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 200, description = "共有相手一覧", body = ThreadSharesResponse),
        (status = 403, description = "owner でない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn list_thread_shares(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ThreadSharesResponse>, ApiError> {
    let entries = chat_store(&state)?
        .list_thread_shares(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(ThreadSharesResponse {
        shares: entries
            .into_iter()
            .map(|(target, role)| ThreadShareEntry { target, role })
            .collect(),
    }))
}
