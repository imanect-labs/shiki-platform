//! `/me/approvals`・`/me/workflow-runs`・`/me/usage` — ホームの「今日のこと」（#965）。
//!
//! どれも本人の分だけを返す読み取り API。範囲は `AuthContext` から決め、他人の分は出さない。
//! - 承認待ち: 自分が actor の run（承認できるのは本人だけ）を SQL で絞り、thread の editor を
//!   OpenFGA でまとめて確かめる（`ChatStore::list_pending_approvals`）。
//! - ワークフロー: 本人に見えるワークフロー（一覧 API と同じ所有∪共有）を先に確定し、その
//!   集合だけを集計する（PIT-13・`visible_workflow_ids`）。
//! - 利用量: 上限の判定と同じ集計（日本時間の暦月・本人の分）。

use axum::{extract::State, Json};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use super::chat::chat_store;
use super::llm_models::gateway;
use crate::{error::ApiError, extract::AuthContextExt, state::AppState, workflow_directory};

/// ホームに出す承認待ちの件数（全件は取らない）。
const PENDING_LIMIT: i64 = 5;
/// ワークフローの集計期間（時間）。
const WORKFLOW_WINDOW_HOURS: i64 = 24;
/// 失敗の明細の件数。
const FAILURE_LIMIT: i64 = 3;

/// 自分の承認を待っている run（新しい順）。
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingApprovalsResponse {
    pub items: Vec<chat::PendingApproval>,
}

/// 自分の承認を待っている run（ホームの「今日のこと」）。
#[utoipa::path(
    get, path = "/me/approvals",
    responses(
        (status = 200, description = "自分の承認を待っている run（新しい順・最大 5 件）", body = PendingApprovalsResponse),
        (status = 401, description = "未認証"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn list_pending_approvals(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<PendingApprovalsResponse>, ApiError> {
    let items = chat_store(&state)?
        .list_pending_approvals(&ctx, PENDING_LIMIT)
        .await?;
    Ok(Json(PendingApprovalsResponse { items }))
}

/// 失敗した実行 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct FailedWorkflowRun {
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    /// ワークフローの名前（artifact 名）。
    pub workflow_name: String,
    /// 表示名（最新版の定義に書かれたもの）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 失敗の理由（先頭 200 文字まで）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_reason: Option<String>,
    pub finished_at: DateTime<Utc>,
}

/// 直近に終わったワークフロー実行の集計。
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkflowRunDigestResponse {
    /// 集計した期間（時間・今から遡る）。
    pub window_hours: i64,
    pub succeeded: i64,
    pub failed: i64,
    /// 期間内に失敗した実行（終了の新しい順・最大 3 件）。
    pub recent_failures: Vec<FailedWorkflowRun>,
}

/// 本人に見えるワークフローの、直近 24 時間に終わった実行の集計（ホームの「今日のこと」）。
#[utoipa::path(
    get, path = "/me/workflow-runs",
    responses(
        (status = 200, description = "直近 24 時間の成功・失敗の件数と失敗の明細", body = WorkflowRunDigestResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn workflow_run_digest(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<WorkflowRunDigestResponse>, ApiError> {
    let (ids, _owned) = workflow_directory::visible_workflow_ids(&state.artifacts, &ctx).await?;
    let since = Utc::now() - Duration::hours(WORKFLOW_WINDOW_HOURS);
    let digest = state
        .workflow_summaries
        .run_digest(&ctx.tenant_id, &ids, since, FAILURE_LIMIT)
        .await
        .map_err(|e| ApiError::Internal(format!("ワークフロー実行の集計: {e}")))?;
    Ok(Json(WorkflowRunDigestResponse {
        window_hours: WORKFLOW_WINDOW_HOURS,
        succeeded: digest.succeeded,
        failed: digest.failed,
        recent_failures: digest
            .recent_failures
            .into_iter()
            .map(|f| FailedWorkflowRun {
                run_id: f.run_id,
                workflow_id: f.workflow_id,
                workflow_name: f.workflow_name,
                display_name: f.display_name,
                fail_reason: f.fail_reason,
                finished_at: f.finished_at,
            })
            .collect(),
    }))
}

/// 本人の当月（日本時間）の利用額と上限。
#[derive(Debug, Serialize, ToSchema)]
pub struct MyUsageResponse {
    /// 当月の利用額（マイクロ USD）。上限の判定と同じ集計（全プロバイダ合算）。
    pub used_usd_micros: i64,
    /// 本人の上限（マイクロ USD）。上限が無い環境では `null`。
    pub limit_usd_micros: Option<i64>,
    /// 集計期間の始まり（日本時間の今月 1 日 00:00）。
    pub period_start: DateTime<Utc>,
    /// 集計期間の終わり（日本時間の来月 1 日 00:00・上限がリセットされる時刻）。
    pub period_end: DateTime<Utc>,
}

/// 本人の当月の利用額と上限（ホームの「今日のこと」）。
#[utoipa::path(
    get, path = "/me/usage",
    responses(
        (status = 200, description = "本人の当月の利用額と上限", body = MyUsageResponse),
        (status = 401, description = "未認証"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn my_usage(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<MyUsageResponse>, ApiError> {
    let usage = gateway(&state)?
        .month_usage(&ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("利用量の集計: {e}")))?;
    Ok(Json(MyUsageResponse {
        used_usd_micros: usage.used_usd_micros,
        limit_usd_micros: usage.limit_usd_micros,
        period_start: usage.period_start,
        period_end: usage.period_end,
    }))
}
