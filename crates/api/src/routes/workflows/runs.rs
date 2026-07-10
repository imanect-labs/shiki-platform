//! run の起動（対話トリガ）・状態取得（Task 10.2・Stage A W3）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use workflow_engine::{RunStatus, StepStatus};

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 対話トリガの run 起動リクエスト（起動入力）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct StartRunRequest {
    /// run 起動入力（`$from input` の源）。
    #[serde(default)]
    #[schema(value_type = Object)]
    pub input: serde_json::Value,
}

/// run 起動レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct StartRunResponse {
    /// 起動した run の id（起動されなかった場合は null）。
    pub run_id: Option<Uuid>,
}

/// step 状態の 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct StepStatusItem {
    pub step_path: String,
    pub status: String,
}

/// run 状態レスポンス（実行履歴の要約）。
#[derive(Debug, Serialize, ToSchema)]
pub struct RunStatusResponse {
    pub run_id: Uuid,
    pub status: String,
    pub steps: Vec<StepStatusItem>,
}

/// ワークフローを対話トリガで起動する（実行主体＝起動ユーザーの権限）。
#[utoipa::path(
    post,
    path = "/workflows/{id}/runs",
    params(("id" = Uuid, Path, description = "ワークフロー ID")),
    request_body = StartRunRequest,
    responses(
        (status = 202, description = "起動した", body = StartRunResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    security(("session" = [])),
)]
pub async fn start_workflow_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
    Json(req): Json<StartRunRequest>,
) -> Result<(StatusCode, Json<StartRunResponse>), ApiError> {
    let launcher = state
        .workflow_launcher
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("workflow 実行時が無効です".into()))?;
    // start_interactive 内で workflows.get_latest の OpenFGA 認可を通る。IR 取得失敗（権限なし/不在）は
    // 存在秘匿のため 404 に写す（500 にしない）。
    let run_id = launcher
        .start_interactive(&ctx, id, &req.input)
        .await
        .map_err(|e| match e {
            workflow_engine::run::LauncherError::Ir(_) => ApiError::NotFound,
            other => ApiError::Internal(format!("run 起動: {other}")),
        })?;
    Ok((StatusCode::ACCEPTED, Json(StartRunResponse { run_id })))
}

/// run/step の状態を取得する（実行履歴・テナントスコープ）。
#[utoipa::path(
    get,
    path = "/workflows/{id}/runs/{run_id}",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("run_id" = Uuid, Path, description = "run ID"),
    ),
    responses(
        (status = 200, description = "run 状態", body = RunStatusResponse),
        (status = 401, description = "未認証"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_workflow_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<RunStatusResponse>, ApiError> {
    let runs = state
        .workflow_runs
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("workflow 実行時が無効です".into()))?;
    // ① ワークフロー {id} への閲覧権限を OpenFGA で検証（他人の run 履歴を覗けない）。
    let meta = state.artifacts.get(&ctx, id, trace.as_deref()).await?;
    if meta.kind != artifact::ArtifactKind::Workflow {
        return Err(ApiError::NotFound);
    }
    // ② run を {id} 配下 ＋ テナントスコープで引く（別ワークフローの run_id を渡しても存在秘匿）。
    let status: RunStatus = runs
        .run_status_for_workflow(&ctx.tenant_id, id, run_id)
        .await
        .map_err(|e| ApiError::Internal(format!("run 状態: {e}")))?
        .ok_or(ApiError::NotFound)?;
    let steps = runs
        .step_statuses(&ctx.tenant_id, run_id)
        .await
        .map_err(|e| ApiError::Internal(format!("step 状態: {e}")))?;
    Ok(Json(RunStatusResponse {
        run_id,
        status: status.as_str().to_string(),
        steps: steps
            .into_iter()
            .map(|(step_path, s): (String, StepStatus)| StepStatusItem {
                step_path,
                status: s.as_str().to_string(),
            })
            .collect(),
    }))
}
