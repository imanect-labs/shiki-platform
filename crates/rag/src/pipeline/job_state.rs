//! rag_ingest_job（ドメイン状態）の永続化: 冪等判定・結果記録・DLQ 可視化。

use jobq::ClaimedJob;
use sqlx::PgPool;

use super::IngestMessage;
use crate::error::RagError;

/// 冪等判定つきジョブ開始。既に成功/スキップ済みなら `true`（再処理不要）。
pub(super) async fn begin_job(
    pool: &PgPool,
    message: &IngestMessage,
    trace_id: Option<&str>,
) -> Result<bool, RagError> {
    let status: String = sqlx::query_scalar(
        "insert into rag_ingest_job \
             (tenant_id, org, node_id, version, op, status, attempts, trace_id) \
         values ($1, $2, $3, $4, $5, 'running', 1, $6) \
         on conflict (tenant_id, node_id, version, op) do update set \
             attempts = rag_ingest_job.attempts + 1, \
             updated_at = now(), \
             status = case when rag_ingest_job.status in ('succeeded', 'skipped') \
                           then rag_ingest_job.status else 'running' end \
         returning status",
    )
    .bind(&message.tenant_id)
    .bind(&message.org)
    .bind(message.node_id)
    .bind(message.version)
    .bind(&message.op)
    .bind(trace_id)
    .fetch_one(pool)
    .await?;
    Ok(status == "succeeded" || status == "skipped")
}

/// ジョブ結果の記録。
pub(super) async fn finish_job(
    pool: &PgPool,
    message: &IngestMessage,
    status: &str,
    last_error: Option<&str>,
) -> Result<(), RagError> {
    sqlx::query(
        "update rag_ingest_job set status = $5, last_error = $6, updated_at = now() \
         where tenant_id = $1 and node_id = $2 and version = $3 and op = $4",
    )
    .bind(&message.tenant_id)
    .bind(message.node_id)
    .bind(message.version)
    .bind(&message.op)
    .bind(status)
    .bind(last_error)
    .execute(pool)
    .await?;
    Ok(())
}

/// DLQ へ移送されたジョブのドメイン状態を dead にする（運用可視化）。
///
/// jobq の kill/fail と**同一トランザクション**で呼び、キューと rag_ingest_job の
/// 状態がずれないようにする（`conn` は呼び出し側の tx）。
pub(super) async fn mark_job_dead_on(
    conn: &mut sqlx::PgConnection,
    job: &ClaimedJob,
    error: &str,
) -> Result<(), RagError> {
    let Ok(message) = serde_json::from_value::<IngestMessage>(job.payload.clone()) else {
        return Ok(());
    };
    sqlx::query(
        "update rag_ingest_job set status = 'dead', last_error = $5, updated_at = now() \
         where tenant_id = $1 and node_id = $2 and version = $3 and op = $4",
    )
    .bind(&message.tenant_id)
    .bind(message.node_id)
    .bind(message.version)
    .bind(&message.op)
    .bind(error)
    .execute(conn)
    .await?;
    Ok(())
}
