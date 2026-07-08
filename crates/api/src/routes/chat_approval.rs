//! 自律エージェントの承認ゲート API（Task 5.6）。
//!
//! 破壊系/egress/高コスト操作の承認要求（SSE `approval_requested`）に対し、ユーザーが承認/却下を
//! 下す。待機中の run（`waiting_approval`）が決定を拾って継続/中断する。判定は監査へ記録される。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use super::chat::chat_store;
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 承認要求への決定（承認/却下）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApprovalDecisionRequest {
    /// 承認対象のツール呼び出し ID（SSE の approval_requested で受け取る）。
    pub tool_call_id: String,
    /// 承認要求元のツール名（監査用）。
    pub tool_name: String,
    /// true で承認、false で却下。
    pub approved: bool,
}

/// 自律エージェントの承認要求へ決定を下す（editor 権限・Task 5.6）。
#[utoipa::path(
    post, path = "/threads/{id}/runs/{run_id}/approvals",
    params(
        ("id" = Uuid, Path, description = "スレッド ID"),
        ("run_id" = Uuid, Path, description = "run ID"),
    ),
    request_body = ApprovalDecisionRequest,
    responses(
        (status = 204, description = "決定を受理（先勝ち・既決なら no-op）"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn submit_approval(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .submit_approval(
            &ctx,
            id,
            run_id,
            &req.tool_call_id,
            &req.tool_name,
            req.approved,
            trace.as_deref(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
