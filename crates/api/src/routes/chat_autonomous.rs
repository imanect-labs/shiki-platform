//! 自律エージェントの承認モード API（#350）。
//!
//! thread 単位の承認 3 モード（承認必須/オート/全自動）の取得と切替。切替は editor 権限で、
//! bypass（全自動）は org キャップ（`tenant.allow_autonomous_bypass`）を検査して**明示エラー**で
//! 弾く（黙って降格しない）。実行中トグルはワーカー側の承認ゲートが各破壊系呼び出しの直前に
//! 反映する（緩和は run の actor 本人による設定のみ有効・`chat::autonomous`）。

use axum::{
    extract::{Path, State},
    Json,
};
use chat::AutonomousMode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::chat::chat_store;
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 承認モードの設定リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetAutonomousModeRequest {
    /// require_approval（承認必須・既定）/ auto（オート）/ bypass（全自動・危険）。
    pub mode: AutonomousMode,
}

/// 承認モードの現在値（UI のセレクタ表示・bypass 選択肢の活性判定）。
#[derive(Debug, Serialize, ToSchema)]
pub struct AutonomousModeResponse {
    pub mode: AutonomousMode,
    /// org 管理者ポリシで bypass（全自動）を選べるか（false なら UI は選択肢を無効化する）。
    pub bypass_allowed: bool,
}

/// スレッドの承認モードを取得する（viewer 権限）。
#[utoipa::path(
    get, path = "/threads/{id}/autonomous-mode",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 200, description = "現在の承認モード", body = AutonomousModeResponse),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_autonomous_mode(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<AutonomousModeResponse>, ApiError> {
    let store = chat_store(&state)?;
    // viewer 認可（存在秘匿込み）。モード値は Thread に載っている。
    let thread = store.get_thread(&ctx, id, trace.as_deref()).await?;
    let bypass_allowed = store.autonomous_bypass_allowed(&ctx.tenant_id).await?;
    Ok(Json(AutonomousModeResponse {
        mode: thread.autonomous_mode,
        bypass_allowed,
    }))
}

/// スレッドの承認モードを設定する（editor 権限・実行中トグル可・#350）。
///
/// bypass は org キャップ違反なら 400（明示エラー・黙って降格しない）。
#[utoipa::path(
    put, path = "/threads/{id}/autonomous-mode",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    request_body = SetAutonomousModeRequest,
    responses(
        (status = 200, description = "設定後の承認モード", body = AutonomousModeResponse),
        (status = 400, description = "org ポリシで bypass 禁止"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn set_autonomous_mode(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<SetAutonomousModeRequest>,
) -> Result<Json<AutonomousModeResponse>, ApiError> {
    let store = chat_store(&state)?;
    store
        .set_autonomous_mode(&ctx, id, req.mode, trace.as_deref())
        .await?;
    let bypass_allowed = store.autonomous_bypass_allowed(&ctx.tenant_id).await?;
    Ok(Json(AutonomousModeResponse {
        mode: req.mode,
        bypass_allowed,
    }))
}
