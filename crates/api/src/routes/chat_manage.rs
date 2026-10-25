//! スレッドの整理のルート（名前の変更・ピン留め・由来ノート・削除）。
//!
//! - 名前の変更・ピン留め・削除はサイドバー履歴の行メニューから使う（owner のみ）。
//! - 由来ノート（issue #282）: 下書き確定→ノート実体化時に、その会話を新ノートへ紐付けて
//!   「ノート由来」にする。ノートは**発話ユーザーの viewer 権限**で解決（見えないノートに
//!   紐づけない・fail-closed）。
//!
//! 認可・監査は下位の StorageService / ChatStore が担う（ここでは権限を判定しない）。

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
pub use super::chat_dto::UpdateThreadRequest;

/// スレッドを更新する（PATCH /threads/{id}・指定した項目だけ）。
#[utoipa::path(
    patch, path = "/threads/{id}", request_body = UpdateThreadRequest,
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 204, description = "更新した"),
        (status = 400, description = "項目が無い・タイトルが空または長すぎる"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn update_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateThreadRequest>,
) -> Result<StatusCode, ApiError> {
    if req.title.is_none() && req.pinned.is_none() && req.note_id.is_none() {
        return Err(ApiError::BadRequest(
            "title・pinned・note_id のいずれかを指定してください".into(),
        ));
    }
    let store = chat_store(&state)?;
    // 失敗し得る検証（ノートの解決）を先に済ませ、途中まで適用して失敗する経路を減らす。
    let note = match req.note_id {
        Some(note_id) => Some((
            note_id,
            state
                .storage
                .get_metadata(&ctx, note_id, trace.as_deref())
                .await?
                .name,
        )),
        None => None,
    };
    if let Some(title) = &req.title {
        store
            .rename_thread(&ctx, id, title, trace.as_deref())
            .await?;
    }
    if let Some(pinned) = req.pinned {
        store
            .set_thread_pinned(&ctx, id, pinned, trace.as_deref())
            .await?;
    }
    if let Some((note_id, name)) = note {
        store
            .set_thread_origin_note(&ctx, id, note_id, &name, trace.as_deref())
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// スレッドを削除する（DELETE /threads/{id}・owner のみ・論理削除）。
///
/// 一覧から消え、共有相手を含めて開けなくなる。生成中・順番待ちの応答は停止する。
#[utoipa::path(
    delete, path = "/threads/{id}",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 204, description = "削除した"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn delete_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .delete_thread(&ctx, id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
