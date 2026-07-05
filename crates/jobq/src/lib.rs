//! shiki-jobq — Postgres ネイティブの汎用ジョブキュー（Task 2.8）。
//!
//! pgmq は採用せず vanilla Postgres（`FOR UPDATE SKIP LOCKED` ＋ visibility timeout）で
//! 自作する。拡張依存ゼロ＝オンプレ持込 Postgres・マネージド PG・エアギャップの全てで
//! 動き、Phase 10 workflow-engine（Postgres 上の自作 Durable Execution）と同一系譜の
//! 自前プリミティブとして育てる（採用理由の正本: docs/design.md §4.3）。
//!
//! 配信セマンティクスは **at-least-once**:
//! - [`enqueue_on`] はドメイン書込と同一トランザクションで投入できる（トランザクショナル
//!   enqueue がこのキューの本質価値。outbox → queue の relay も単一 txn で exactly-once）。
//! - [`claim`] は可視（`visible_at <= now()`）な行を SKIP LOCKED で確保し、`visible_at` を
//!   `now() + vt` へ進める。consumer がクラッシュしても vt 経過で自動再配信される。
//! - 処理成功 = [`ack`]（DELETE）。失敗 = [`fail`]（バックオフ延長。試行上限に達したら
//!   `job_queue_dead` へ移送＝DLQ）。DLQ からは [`requeue_dead`] で再投入する。
//!
//! 消費側は payload の冪等キー（RAG なら `(node_id, version, op)`）で二重処理を防ぐこと。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use std::time::Duration;

/// jobq のエラー。現状は DB エラーの薄い包み（呼び出し側でリトライ判断する）。
#[derive(Debug, thiserror::Error)]
pub enum JobqError {
    #[error("jobq db error: {0}")]
    Db(#[from] sqlx::Error),
}

/// 投入するジョブ。`payload` の中身はキューごとの契約（型は消費側クレートが持つ）。
pub struct NewJob<'a> {
    /// キュー名（例: `rag_ingest`）。
    pub queue: &'a str,
    pub tenant_id: &'a str,
    pub payload: &'a Value,
    /// OTel と共有するトレース ID（イベント発生元から引き継ぐ）。
    pub trace_id: Option<&'a str>,
    /// 配信試行の上限。超えると DLQ へ。
    pub max_attempts: i32,
}

/// [`claim`] が返す配信中ジョブ。
#[derive(Debug, sqlx::FromRow)]
pub struct ClaimedJob {
    pub id: i64,
    pub queue: String,
    pub tenant_id: String,
    pub payload: Value,
    /// 今回を含む配信試行回数（claim 時にインクリメント済みの値）。
    pub attempts: i32,
    pub max_attempts: i32,
    pub trace_id: Option<String>,
    pub enqueued_at: DateTime<Utc>,
}

/// [`fail`] の結果。
#[derive(Debug, PartialEq, Eq)]
pub enum FailOutcome {
    /// バックオフ後に再配信される。
    Retry { attempts: i32 },
    /// 試行上限に達し `job_queue_dead` へ移送した。
    Dead,
}

/// DLQ の 1 件（運用可視化・再投入用）。
#[derive(Debug, sqlx::FromRow)]
pub struct DeadJob {
    pub id: i64,
    pub queue: String,
    pub tenant_id: String,
    pub payload: Value,
    pub attempts: i32,
    pub last_error: String,
    pub died_at: DateTime<Utc>,
}

/// ジョブを 1 件投入する。ドメイン書込と同一トランザクション上で呼べる。
pub async fn enqueue_on(conn: &mut PgConnection, job: NewJob<'_>) -> Result<i64, JobqError> {
    let id: i64 = sqlx::query_scalar(
        "insert into job_queue (queue, tenant_id, payload, max_attempts, trace_id) \
         values ($1, $2, $3, $4, $5) returning id",
    )
    .bind(job.queue)
    .bind(job.tenant_id)
    .bind(job.payload)
    .bind(job.max_attempts)
    .bind(job.trace_id)
    .fetch_one(conn)
    .await?;
    Ok(id)
}

/// 可視なジョブを最大 `limit` 件確保し、`visible_at` を `now() + vt` へ進める。
///
/// 単一 UPDATE 文（サブクエリに `FOR UPDATE SKIP LOCKED`）なので複数 consumer が
/// 競合しても同一ジョブを二重確保しない。`attempts` はここで +1 される。
pub async fn claim(
    conn: &mut PgConnection,
    queue: &str,
    vt: Duration,
    limit: i64,
) -> Result<Vec<ClaimedJob>, JobqError> {
    let jobs = sqlx::query_as::<_, ClaimedJob>(
        "update job_queue set visible_at = now() + $3 * interval '1 second', \
                              attempts = attempts + 1 \
         where id in ( \
             select id from job_queue \
             where queue = $1 and visible_at <= now() \
             order by id \
             limit $2 \
             for update skip locked \
         ) \
         returning id, queue, tenant_id, payload, attempts, max_attempts, trace_id, enqueued_at",
    )
    .bind(queue)
    .bind(limit)
    .bind(vt.as_secs_f64())
    .fetch_all(conn)
    .await?;
    Ok(jobs)
}

/// 処理成功。ジョブを削除する（存在しなくても成功＝冪等）。
pub async fn ack(conn: &mut PgConnection, id: i64) -> Result<(), JobqError> {
    sqlx::query("delete from job_queue where id = $1")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

/// 処理失敗。試行上限内なら `backoff` 後に再配信、上限到達なら DLQ へ移送する。
///
/// 移送は単一 CTE（DELETE → INSERT）で原子的に行う。ジョブが既に無い場合（並行 ack 等）は
/// no-op で `Retry { attempts: 0 }` を返す（再配信は起きない）。
pub async fn fail(
    conn: &mut PgConnection,
    id: i64,
    error: &str,
    backoff: Duration,
) -> Result<FailOutcome, JobqError> {
    // 上限到達分を DLQ へ原子的に移送。
    let moved = sqlx::query_scalar::<_, i64>(
        "with dead as ( \
             delete from job_queue where id = $1 and attempts >= max_attempts returning * \
         ) \
         insert into job_queue_dead \
             (id, queue, tenant_id, payload, attempts, max_attempts, trace_id, enqueued_at, last_error) \
         select id, queue, tenant_id, payload, attempts, max_attempts, trace_id, enqueued_at, $2 \
         from dead returning id",
    )
    .bind(id)
    .bind(error)
    .fetch_optional(&mut *conn)
    .await?;
    if moved.is_some() {
        return Ok(FailOutcome::Dead);
    }

    let attempts = sqlx::query_scalar::<_, i32>(
        "update job_queue set visible_at = now() + $2 * interval '1 second' \
         where id = $1 returning attempts",
    )
    .bind(id)
    .bind(backoff.as_secs_f64())
    .fetch_optional(conn)
    .await?
    .unwrap_or(0);
    Ok(FailOutcome::Retry { attempts })
}

/// DLQ の 1 件を元キューへ再投入する（attempts はリセット）。無ければ `false`。
pub async fn requeue_dead(conn: &mut PgConnection, id: i64) -> Result<bool, JobqError> {
    let requeued = sqlx::query_scalar::<_, i64>(
        "with revived as (delete from job_queue_dead where id = $1 returning *) \
         insert into job_queue (queue, tenant_id, payload, max_attempts, trace_id) \
         select queue, tenant_id, payload, max_attempts, trace_id from revived returning id",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(requeued.is_some())
}

/// DLQ を新しい順に列挙する（運用可視化）。
pub async fn dead_jobs(
    conn: &mut PgConnection,
    queue: &str,
    limit: i64,
) -> Result<Vec<DeadJob>, JobqError> {
    let jobs = sqlx::query_as::<_, DeadJob>(
        "select id, queue, tenant_id, payload, attempts, last_error, died_at \
         from job_queue_dead where queue = $1 order by died_at desc limit $2",
    )
    .bind(queue)
    .bind(limit)
    .fetch_all(conn)
    .await?;
    Ok(jobs)
}

/// テナント消去（SAAS.2）: 当該テナントのジョブを待機列・DLQ から破棄する。削除件数を返す。
pub async fn delete_tenant(conn: &mut PgConnection, tenant_id: &str) -> Result<u64, JobqError> {
    let queued = sqlx::query("delete from job_queue where tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    let dead = sqlx::query("delete from job_queue_dead where tenant_id = $1")
        .bind(tenant_id)
        .execute(conn)
        .await?
        .rows_affected();
    Ok(queued + dead)
}

/// 指数バックオフ（30s → 2m → 8m → 32m → …、上限 1h）。consumer の既定リトライ方針。
pub fn backoff_for(attempts: i32) -> Duration {
    let exp = u32::try_from(attempts.saturating_sub(1).clamp(0, 6)).unwrap_or(0);
    let secs = 30u64.saturating_mul(4u64.saturating_pow(exp));
    Duration::from_secs(secs.min(3600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff_for(1), Duration::from_secs(30));
        assert_eq!(backoff_for(2), Duration::from_mins(2));
        assert_eq!(backoff_for(3), Duration::from_mins(8));
        assert_eq!(backoff_for(10), Duration::from_hours(1));
        // 0 以下でも安全（初回相当）。
        assert_eq!(backoff_for(0), Duration::from_secs(30));
        assert_eq!(backoff_for(-5), Duration::from_secs(30));
    }
}
