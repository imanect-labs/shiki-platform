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
//!
//! # テナント間の公平性（#816）
//!
//! フルプールでは 1 キューを全テナントが共有する。取り出しを到着順（`id`）にすると、1 テナントの
//! 大量投入が捌けるまで他テナントが待たされる。そこで投入時に **`fair_seq`**（start-time fair
//! queuing の開始タグ）を振り、[`claim`] は `fair_seq` 順に取る:
//!
//! `fair_seq = max(そのテナントのキュー内の最大 fair_seq + 1, 仮想時刻)`
//!
//! 各テナントは 1 ジョブにつき fair_seq を 1 しか進められないので、洪水テナントのジョブは先へ
//! 伸び、後から来たテナントは仮想時刻から始まって洪水と交互に取り出される。保証は
//! **ジョブ件数**の公平（ジョブの重さは見ない）で、テナント内の順序は投入順のまま。
//! 同時投入で同じ値になったら `id` 順（厳密な公平ではなく近似で足りる）。
//!
//! **仮想時刻 = 次に取り出される行（可視な行の最小 fair_seq）**。可視な行が無ければキューの
//! 最大 fair_seq（いま不可視の行の後ろに付ける）。不可視の行（処理中・バックオフ中・defer 中）を
//! 仮想時刻に**数えない**のが要点: 数えると、古い fair_seq の行が不可視のまま最小値を押さえ、
//! 後着テナントにその古い値が振られて先行テナントの残りを丸ごと追い越す（一時エラーで落ちる
//! ジョブを 1 件抱え続けるだけで毎回割り込める）。同じ理由で [`fail`] / [`defer`] は再配信する行の
//! fair_seq を**振り直す**（そのテナントの末尾＝新規投入と同じ扱い）。古い値のまま可視に戻すと、
//! 戻った瞬間に仮想時刻が過去へ巻き戻る。例外は consumer のクラッシュで vt 切れになった行で、
//! 振り直す機会が無いため一時的に仮想時刻を戻し得る（その間の後着が少し前に入るだけで有界）。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use std::time::Duration;

/// 行に振る `fair_seq` の SQL 式（モジュール冒頭の「テナント間の公平性」）。
///
/// `$queue` / `$tenant` はキュー名・テナントを表す SQL 式。参照はいずれも索引の端点から:
/// テナント末尾は `job_queue_tenant_idx` の後端、仮想時刻は `job_queue_fair_idx` の先頭から
/// 可視な行まで（読み飛ばすのは不可視の行だけ）、フォールバックは同索引の後端。
macro_rules! fair_seq_expr {
    ($queue:literal, $tenant:literal) => {
        concat!(
            "greatest(",
            "coalesce((select q.fair_seq + 1 from job_queue q where q.tenant_id = ",
            $tenant,
            " and q.queue = ",
            $queue,
            " order by q.fair_seq desc limit 1), 0), ",
            "coalesce((select q.fair_seq from job_queue q where q.queue = ",
            $queue,
            " and q.visible_at <= now() order by q.fair_seq limit 1), ",
            "(select q.fair_seq from job_queue q where q.queue = ",
            $queue,
            " order by q.fair_seq desc limit 1), 0))"
        )
    };
}

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
///
/// `fair_seq`（テナント間の公平性・モジュール冒頭）を同じ文で振る。
pub async fn enqueue_on(conn: &mut PgConnection, job: NewJob<'_>) -> Result<i64, JobqError> {
    let id: i64 = sqlx::query_scalar(concat!(
        "insert into job_queue (queue, tenant_id, payload, max_attempts, trace_id, fair_seq) \
         values ($1, $2, $3, $4, $5, ",
        fair_seq_expr!("$1", "$2"),
        ") returning id"
    ))
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
///
/// 取り出し順は `fair_seq, id`（テナント間の公平性・モジュール冒頭）。`job_queue_fair_idx` を
/// キュー内で先頭から辿り、読み飛ばすのは処理中・バックオフ中の行だけ。
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
             order by fair_seq, id \
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

    // 再配信は fair_seq を振り直す（モジュール冒頭: 古い値のまま戻すと仮想時刻が巻き戻る）。
    let attempts = sqlx::query_scalar::<_, i32>(concat!(
        "update job_queue set visible_at = now() + $2 * interval '1 second', fair_seq = ",
        fair_seq_expr!("job_queue.queue", "job_queue.tenant_id"),
        " where id = $1 returning attempts"
    ))
    .bind(id)
    .bind(backoff.as_secs_f64())
    .fetch_optional(conn)
    .await?
    .unwrap_or(0);
    Ok(FailOutcome::Retry { attempts })
}

/// **まだ処理できない**ジョブを後回しにする（失敗ではない）。
///
/// [`fail`] との違いは `attempts` を消費しないこと。claim が +1 した分を戻すので、何度
/// 先送りしても DLQ へ落ちない。「順番待ち」のように、待てば必ず処理できる条件で使う
/// （consumer 側の都合で処理できないだけなら、それは失敗ではない）。
///
/// 待ち条件が永久に解けないと無限に先送りされるため、**その条件自体が有限時間で
/// 解けること**を呼び出し側が保証すること（chat の run 直列化なら、先行 run は
/// 完了・キャンセル・DLQ のいずれかで必ず端末状態になる）。
pub async fn defer(conn: &mut PgConnection, id: i64, delay: Duration) -> Result<(), JobqError> {
    // 先送りも fair_seq を振り直す（[`fail`] と同じ理由）。
    sqlx::query(concat!(
        "update job_queue \
         set visible_at = now() + $2 * interval '1 second', \
             attempts = greatest(attempts - 1, 0), fair_seq = ",
        fair_seq_expr!("job_queue.queue", "job_queue.tenant_id"),
        " where id = $1"
    ))
    .bind(id)
    .bind(delay.as_secs_f64())
    .execute(conn)
    .await?;
    Ok(())
}

/// ジョブを試行回数に関わらず即 DLQ へ移送する（恒久エラー: リトライしても直らない
/// パース失敗・設定不整合など）。移送したら `true`、既に無ければ `false`。
pub async fn kill(conn: &mut PgConnection, id: i64, error: &str) -> Result<bool, JobqError> {
    let moved = sqlx::query_scalar::<_, i64>(
        "with dead as (delete from job_queue where id = $1 returning *) \
         insert into job_queue_dead \
             (id, queue, tenant_id, payload, attempts, max_attempts, trace_id, enqueued_at, last_error) \
         select id, queue, tenant_id, payload, attempts, max_attempts, trace_id, enqueued_at, $2 \
         from dead returning id",
    )
    .bind(id)
    .bind(error)
    .fetch_optional(conn)
    .await?;
    Ok(moved.is_some())
}

/// DLQ の 1 件を元キューへ再投入する（attempts はリセット）。無ければ `false`。
///
/// `fair_seq` は [`enqueue_on`] と同じ式で振り直す（新規投入と同じ扱い。DLQ に落ちた時点の
/// 古い値で割り込ませない）。
pub async fn requeue_dead(conn: &mut PgConnection, id: i64) -> Result<bool, JobqError> {
    let requeued = sqlx::query_scalar::<_, i64>(concat!(
        "with revived as (delete from job_queue_dead where id = $1 returning *) \
         insert into job_queue (queue, tenant_id, payload, max_attempts, trace_id, fair_seq) \
         select r.queue, r.tenant_id, r.payload, r.max_attempts, r.trace_id, ",
        fair_seq_expr!("r.queue", "r.tenant_id"),
        " from revived r returning id"
    ))
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
