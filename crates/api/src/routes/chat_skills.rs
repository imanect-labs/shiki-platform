//! スレッドの skill ピン系ルート（#344 Task 10.11・`routes/chat.rs` から分割・500 行規約）。
//!
//! ピンは**設定者の権限**で version 込みに解決してから保存する（存在・kind・viewer 検証は
//! SkillStore が担う・fail-closed）。ピンの意味は「最初からロード済みのスキル」であり、
//! 途中適用は skill ツール（カタログ引き）が担う。

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
use super::chat_dto::ArtifactPinRequest;
pub use super::chat_dto::SetThreadSkillsRequest;

/// 1 スレッドにピンできる skill の上限（カタログ/プロンプト肥大の防御）。
const MAX_THREAD_SKILL_PINS: usize = 8;

/// skill ピン要求を**設定者の権限**で version 込みに解決する（重複排除・上限・fail-closed）。
pub(super) async fn resolve_skill_pins(
    state: &AppState,
    ctx: &authz::AuthContext,
    pins: &[ArtifactPinRequest],
    trace_id: Option<&str>,
) -> Result<Vec<chat::SkillPin>, ApiError> {
    if pins.len() > MAX_THREAD_SKILL_PINS {
        return Err(ApiError::BadRequest(format!(
            "skill ピンは最大 {MAX_THREAD_SKILL_PINS} 件です"
        )));
    }
    let mut out: Vec<chat::SkillPin> = Vec::with_capacity(pins.len());
    for pin in pins {
        if out.iter().any(|p| p.skill_id == pin.artifact_id) {
            continue; // 同一 skill の重複指定は先勝ちで無視（エラーにしない）
        }
        let (version, _body, _raw) = match pin.version {
            Some(v) => {
                state
                    .skills
                    .get_version(ctx, pin.artifact_id, v, trace_id)
                    .await
            }
            None => {
                state
                    .skills
                    .get_latest(ctx, pin.artifact_id, trace_id)
                    .await
            }
        }
        .map_err(super::ui_specs::map_gui_err)?;
        out.push(chat::SkillPin {
            skill_id: pin.artifact_id,
            skill_version: version,
        });
    }
    Ok(out)
}

/// スレッドの skill ピン集合を置き換える（owner のみ・途中変更・#344）。
#[utoipa::path(
    put, path = "/threads/{id}/skills", request_body = SetThreadSkillsRequest,
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 204, description = "置き換えた"),
        (status = 400, description = "ミニアプリ経由のスレッド等・変更不可"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn set_thread_skills(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<SetThreadSkillsRequest>,
) -> Result<StatusCode, ApiError> {
    let pins = resolve_skill_pins(&state, &ctx, &req.skills, trace.as_deref()).await?;
    chat_store(&state)?
        .set_thread_skills(&ctx, id, &pins, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
