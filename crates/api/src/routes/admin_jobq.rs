//! jobq の DLQ の運用 API（運用者の provisioner プレーン・#1335）。
//!
//! DLQ に落ちたジョブはテナントをまたいだ運用の対象（下流の障害で一斉に落ちる等）なので、テナント
//! 管理者ではなく運用者のプレーンに置く。一覧は payload を返さない（原因は `last_error` で分かる。
//! payload にはテナントのデータの識別子が入り得る）。件数の監視はメトリクス（`jobq.dead_jobs`）が担い、
//! この API は原因を直した後の確認と再投入に使う。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::{
    error::ApiError,
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

/// 一覧の既定件数と上限。
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/admin/jobq/dead",
            &["GET"],
            AccessPolicy::Provisioner,
            || get(list_dead_jobs),
        ),
        RouteDecl::new(
            "/admin/jobq/dead/{id}/requeue",
            &["POST"],
            AccessPolicy::Provisioner,
            || post(requeue_dead_job),
        ),
    ]
}

/// DLQ 一覧の絞り込み。
#[derive(Debug, Deserialize, IntoParams)]
pub struct DeadJobsQuery {
    /// キュー名（省略時は全キュー）。
    pub queue: Option<String>,
    /// 返す件数（1〜200・既定 50）。
    pub limit: Option<i64>,
}

/// キューごとの DLQ の状況。
#[derive(Debug, Serialize, ToSchema)]
pub struct DeadQueueStatsDto {
    pub queue: String,
    pub count: i64,
    /// 最も古い行が DLQ に落ちた時刻。
    pub oldest_died_at: DateTime<Utc>,
}

/// DLQ の 1 件（payload は含めない）。
#[derive(Debug, Serialize, ToSchema)]
pub struct DeadJobDto {
    pub id: i64,
    pub queue: String,
    pub tenant_id: String,
    pub attempts: i32,
    pub max_attempts: i32,
    /// 最後の失敗の理由。
    pub last_error: String,
    pub enqueued_at: DateTime<Utc>,
    pub died_at: DateTime<Utc>,
}

/// DLQ の一覧（キュー別の件数 ＋ 新しい順の行）。
#[derive(Debug, Serialize, ToSchema)]
pub struct DeadJobsResponse {
    /// キュー別の件数（`queue` の絞り込みに依らず全キュー）。
    pub queues: Vec<DeadQueueStatsDto>,
    /// 新しい順の行（`queue` で絞り込み・最大 `limit` 件）。
    pub jobs: Vec<DeadJobDto>,
}

/// DLQ の一覧を返す（キュー別件数と、新しい順の行。payload は返さない）。
#[utoipa::path(
    get,
    path = "/admin/jobq/dead",
    params(DeadJobsQuery),
    responses(
        (status = 200, description = "DLQ の一覧", body = DeadJobsResponse),
        (status = 400, description = "limit が範囲外"),
        (status = 401, description = "provisioner トークンが無効"),
    ),
    security(("provisioner_token" = [])),
)]
pub async fn list_dead_jobs(
    State(state): State<AppState>,
    Query(q): Query<DeadJobsQuery>,
) -> Result<Json<DeadJobsResponse>, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::BadRequest(format!(
            "limit は 1〜{MAX_LIMIT} で指定してください（指定値: {limit}）"
        )));
    }
    let queues = state
        .dead_letters
        .stats()
        .await?
        .into_iter()
        .map(|s| DeadQueueStatsDto {
            queue: s.queue,
            count: s.count,
            oldest_died_at: s.oldest_died_at,
        })
        .collect();
    let jobs = state
        .dead_letters
        .list(q.queue.as_deref(), limit)
        .await?
        .into_iter()
        .map(|j| DeadJobDto {
            id: j.id,
            queue: j.queue,
            tenant_id: j.tenant_id,
            attempts: j.attempts,
            max_attempts: j.max_attempts,
            last_error: j.last_error,
            enqueued_at: j.enqueued_at,
            died_at: j.died_at,
        })
        .collect();
    Ok(Json(DeadJobsResponse { queues, jobs }))
}

/// DLQ の 1 件を元のキューへ戻す（試行回数はリセット・新規投入と同じ順番に並ぶ）。
///
/// 原因（下流の障害・設定・データ）を直してから戻すこと。直さずに戻すと同じ回数だけ失敗して
/// また DLQ に落ちる。
#[utoipa::path(
    post,
    path = "/admin/jobq/dead/{id}/requeue",
    params(("id" = i64, Path, description = "DLQ の行の id")),
    responses(
        (status = 204, description = "元のキューへ戻した"),
        (status = 401, description = "provisioner トークンが無効"),
        (status = 404, description = "DLQ に該当の行が無い（再投入済み・保持期間で削除済みを含む）"),
    ),
    security(("provisioner_token" = [])),
)]
pub async fn requeue_dead_job(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if !state.dead_letters.requeue(id).await? {
        return Err(ApiError::NotFound);
    }
    tracing::info!(dead_job_id = id, "DLQ のジョブを元のキューへ戻しました");
    Ok(StatusCode::NO_CONTENT)
}
