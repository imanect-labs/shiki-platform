//! ノート↔スレッド連携のルート（issue #282）。
//!
//! 下書き確定→ノート実体化時に、その会話を新ノートへ紐付けて「ノート由来」にする。
//! ノートは**発話ユーザーの viewer 権限**で解決（見えないノートに紐づけない・fail-closed）、
//! 会話は owner のみ（自分の会話の紐付け）。認可・監査は下位の StorageService / ChatStore が担う。

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
pub use super::chat_dto::SetOriginNoteRequest;

/// スレッドの由来ノートを設定する（PATCH /threads/{id}）。
#[utoipa::path(
    patch, path = "/threads/{id}", request_body = SetOriginNoteRequest,
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 204, description = "設定した"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn set_thread_origin_note(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<SetOriginNoteRequest>,
) -> Result<StatusCode, ApiError> {
    let node = state
        .storage
        .get_metadata(&ctx, req.note_id, trace.as_deref())
        .await?;
    chat_store(&state)?
        .set_thread_origin_note(&ctx, id, req.note_id, &node.name, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
