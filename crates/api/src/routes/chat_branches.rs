//! 会話の分岐のルート（表示する枝の切り替え・新しいチャットに分岐・#1166）。
//!
//! 発話の編集そのものは `POST /threads/{id}/messages` の `edit_of`。認可・監査は ChatStore が担う
//! （ここでは権限を判定しない）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};
use chat::Thread;

use super::chat::chat_store;

/// 枝の指定（このメッセージを含む枝へ切り替える／この回答までを分岐する）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct BranchRequest {
    pub message_id: Uuid,
}

/// 表示する枝を切り替える（editor）。選んだメッセージから最も新しい続きを辿った枝を表示中にする。
#[utoipa::path(
    put, path = "/threads/{id}/branch", request_body = BranchRequest,
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 204, description = "切り替えた"),
        (status = 400, description = "回答の生成中（切り替えられない）"),
        (status = 403, description = "投稿権限なし"),
        (status = 404, description = "スレッド・メッセージが存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn select_branch(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<BranchRequest>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .select_branch(&ctx, id, req.message_id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// その回答までの会話を新しいスレッドへ複製する（元の editor・新しいスレッドの owner は呼び出し者）。
#[utoipa::path(
    post, path = "/threads/{id}/fork", request_body = BranchRequest,
    params(("id" = Uuid, Path, description = "分岐元のスレッド ID")),
    responses(
        (status = 201, description = "作成したスレッド", body = Thread),
        (status = 403, description = "元のスレッドの editor ではない"),
        (status = 404, description = "スレッドが存在しない・確定した回答ではない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn fork_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<BranchRequest>,
) -> Result<(StatusCode, Json<Thread>), ApiError> {
    let thread = chat_store(&state)?
        .fork_thread(&ctx, id, req.message_id, trace.as_deref())
        .await?;
    Ok((StatusCode::CREATED, Json(thread)))
}
