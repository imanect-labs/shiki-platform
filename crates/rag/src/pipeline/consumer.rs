//! jobq consumer（Task 2.8）: claim → 冪等判定 → indexer 実行 → ack / fail / kill。

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use jobq::{ClaimedJob, FailOutcome};

use super::indexer::{self, IndexOutcome};
use super::job_state;
use super::{IngestMessage, PipelineDeps, RAG_INGEST_QUEUE};
use crate::error::RagError;

/// ジョブを 1 バッチ消費する。処理した件数を返す（0 = キューが空）。
pub async fn consume_once(deps: &Arc<PipelineDeps>) -> Result<usize, RagError> {
    let concurrency = deps.config.consumer_concurrency.max(1);
    let vt = Duration::from_secs(deps.config.job_vt_secs);
    let jobs = {
        let mut conn = deps.pool.acquire().await?;
        jobq::claim(&mut conn, RAG_INGEST_QUEUE, vt, concurrency as i64).await?
    };
    let count = jobs.len();
    futures::stream::iter(jobs)
        .for_each_concurrent(concurrency, |job| {
            let deps = Arc::clone(deps);
            async move {
                process_job(&deps, job).await;
            }
        })
        .await;
    Ok(count)
}

/// 1 ジョブの実行と結果整理（ack / バックオフ再配信 / DLQ）。
async fn process_job(deps: &Arc<PipelineDeps>, job: ClaimedJob) {
    let result = run_job(deps, &job).await;
    let Ok(mut conn) = deps.pool.acquire().await else {
        // 接続すら取れない場合は何もしない（vt 経過で自動再配信される）。
        tracing::error!(job_id = job.id, "ジョブ結果の記録用接続が取得できません");
        return;
    };
    match result {
        Ok(outcome) => {
            if let Err(e) = jobq::ack(&mut conn, job.id).await {
                tracing::error!(job_id = job.id, error = %e,
                    "ack に失敗（vt 経過後に再配信され冪等処理される）");
            }
            tracing::info!(job_id = job.id, trace_id = ?job.trace_id, outcome = %outcome,
                "インジェスト完了");
        }
        Err(e) if e.is_transient() => retry_or_dead(deps, &mut conn, &job, &e).await,
        Err(e) => kill_permanent(deps, &mut conn, &job, &e).await,
    }
}

/// 恒久エラー（パース失敗・版不一致など）: リトライせず即 DLQ。
async fn kill_permanent(
    deps: &Arc<PipelineDeps>,
    conn: &mut sqlx::PgConnection,
    job: &ClaimedJob,
    error: &RagError,
) {
    if let Err(qe) = jobq::kill(conn, job.id, &error.to_string()).await {
        tracing::error!(job_id = job.id, error = %qe, "kill に失敗");
    }
    tracing::error!(job_id = job.id, trace_id = ?job.trace_id, error = %error,
        "恒久エラー。DLQ へ移送");
    job_state::mark_job_dead(&deps.pool, job, &error.to_string()).await;
}

/// 一時エラーのバックオフ再配信（試行上限で DLQ）。
async fn retry_or_dead(
    deps: &Arc<PipelineDeps>,
    conn: &mut sqlx::PgConnection,
    job: &ClaimedJob,
    error: &RagError,
) {
    let backoff = jobq::backoff_for(job.attempts);
    match jobq::fail(conn, job.id, &error.to_string(), backoff).await {
        Ok(FailOutcome::Dead) => {
            tracing::error!(job_id = job.id, trace_id = ?job.trace_id, error = %error,
                "試行上限に達し DLQ へ移送");
            job_state::mark_job_dead(&deps.pool, job, &error.to_string()).await;
        }
        Ok(FailOutcome::Retry { .. }) => {
            tracing::warn!(job_id = job.id, trace_id = ?job.trace_id, error = %error,
                backoff_secs = backoff.as_secs(), "一時エラー。バックオフ後に再試行");
        }
        Err(qe) => tracing::error!(job_id = job.id, error = %qe, "fail 記録に失敗"),
    }
}

/// メッセージ検証 → 冪等判定 → 実処理。
async fn run_job(deps: &Arc<PipelineDeps>, job: &ClaimedJob) -> Result<IndexOutcome, RagError> {
    let message: IngestMessage = serde_json::from_value(job.payload.clone())?;
    // tenant はキュー行（第一級カラム）とメッセージの二重持ち。食い違いは越境の兆候
    // なので fail-closed（恒久エラー）で止める。
    if message.tenant_id != job.tenant_id {
        return Err(RagError::Config(format!(
            "tenant 不一致: queue={} message={}",
            job.tenant_id, message.tenant_id
        )));
    }
    indexer::handle(deps, &message, job.trace_id.as_deref()).await
}
