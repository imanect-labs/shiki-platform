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
//! `fair_seq = max(そのテナントのキュー内の最大 fair_seq + 1, キューの仮想時刻)`
//!
//! 各テナントは 1 ジョブにつき fair_seq を 1 しか進められないので、洪水テナントのジョブは先へ
//! 伸び、後から来たテナントは仮想時刻から始まって洪水と交互に取り出される。保証は
//! **ジョブ件数**の公平（ジョブの重さは見ない）で、テナント内の順序は投入順のまま。
//! 同時投入で同じ値になったら `id` 順。
//!
//! **仮想時刻はキューごとに単調増加する値として `job_queue_vtime` に保持する**: [`claim`] が
//! 確保した行の最大 fair_seq **＋ 1**（処理に入ったジョブの終了タグ）まで進める（下がらない）。
//! ＋1 が要るのは、キューが空になったテナントの再投入のため: ack で行が消えるとテナント末尾は
//! 参照できず、仮想時刻が「確保した fair_seq」止まりだと、1 件ずつ投入しては処理されるテナントが
//! 毎回同じ値を振られて仮想時刻を進めず、他テナントの滞留を無期限に追い越せた。終了タグまで
//! 進めておけば、空になったテナントも直前に処理された自分のジョブの次から始まる。キュー内の行から毎回求める方式（最小
//! fair_seq・可視な先頭の最小 fair_seq）は、バックオフ明け・defer 明け・consumer のクラッシュで
//! **古い fair_seq の行が可視に戻った瞬間に過去へ巻き戻り**、その直後に投入した後着テナントが
//! 先行テナントの残りを丸ごと追い越せた（失敗ジョブを 1 件抱えるだけで繰り返せる）。保持した
//! 仮想時刻なら、可視に戻った古い行は先に処理されるだけで、後着の開始位置を引きずらない。
//! [`fail`] / [`defer`] は fair_seq を**据え置く**（テナント内の順序を崩さない）。その代わり、可視に
//! 戻った再試行は仮想時刻より小さい値のまま新規ジョブより先に出るので、失敗を繰り返すテナントの
//! 取り分は最大 `max_attempts` 倍まで上振れする（有界。id 順の旧方式より悪くはならない）。
//!
//! **近似として許容していること**: 同じテナントが同じキューへ**並行トランザクション**で投入すると、
//! 互いの未 commit 行が見えず同じ fair_seq 列を重ねて振る（取り分がその並行数倍になる）。
//! テナント単位の advisory lock で直列化すると、投入元のドメイントランザクション（チャットの発話・
//! relay のバッチ）が commit まで同じテナントの他の書込を待たせ、複数キューへ投入する txn 同士で
//! デッドロックし得る。上振れは「同時に投入しているトランザクション数」倍で有界なので、公平性の
//! 近似として受け入れる（rag の relay / consumer はリーダー選出で 1 プロセス・並列 2）。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use std::time::Duration;

/// 行に振る `fair_seq` の SQL 式（モジュール冒頭の「テナント間の公平性」）。
///
/// `$queue` / `$tenant` はキュー名・テナントを表す SQL 式。参照はどちらも 1 行だけ:
/// テナント末尾は `job_queue_tenant_idx` の後端、仮想時刻は `job_queue_vtime` の主キー。
macro_rules! fair_seq_expr {
    ($queue:literal, $tenant:literal) => {
        concat!(
            "greatest(",
            "coalesce((select q.fair_seq + 1 from job_queue q where q.tenant_id = ",
            $tenant,
            " and q.queue = ",
            $queue,
            " order by q.fair_seq desc limit 1), 0), ",
            "coalesce((select v.vt from job_queue_vtime v where v.queue = ",
            $queue,
            "), 0))"
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

/// 同じキュー・同じ payload の**未着手**ジョブが無いときだけ投入する（間引き）。投入したら id を返す。
///
/// 「処理側が実行時に台帳を読み直す」型のジョブ（再計算待ちを全部さらう等）は、未着手の 1 件があれば
/// 後から積まれた分もそれが処理するので、同じ payload を何件も積む必要が無い。
///
/// 未着手（`attempts = 0`）のジョブ行を `FOR UPDATE SKIP LOCKED` で押さえるので、**この Tx の
/// コミットまで [`claim`] はそのジョブを取れない**＝ジョブが走るときには呼び出し側の書込（台帳）が
/// 必ず見える。着手済み（配信中）のジョブは数えない（走査を終えた後の書込を取りこぼすため、新しく積む）。
pub async fn enqueue_unless_pending_on(
    conn: &mut PgConnection,
    job: NewJob<'_>,
) -> Result<Option<i64>, JobqError> {
    let pending: Option<i64> = sqlx::query_scalar(
        "select id from job_queue \
         where queue = $1 and tenant_id = $2 and payload = $3 and attempts = 0 \
         limit 1 for update skip locked",
    )
    .bind(job.queue)
    .bind(job.tenant_id)
    .bind(job.payload)
    .fetch_optional(&mut *conn)
    .await?;
    if pending.is_some() {
        return Ok(None);
    }
    enqueue_on(conn, job).await.map(Some)
}

/// 可視なジョブを最大 `limit` 件確保し、`visible_at` を `now() + vt` へ進める。
///
/// 単一文（確保する行を `FOR UPDATE SKIP LOCKED` で選ぶ CTE ＋ UPDATE）なので複数 consumer が
/// 競合しても同一ジョブを二重確保しない。`attempts` はここで +1 される。
///
/// **確保する行は `MATERIALIZED` の CTE で 1 回だけ選ぶ**。`where id in (select … limit $2 for update
/// skip locked)` の形は、統計が古い（キューが空に近いときに analyze された）と計画が
/// 「外側の seq scan × 内側のサブクエリを行ごとに再実行」の Nested Loop Semi Join になり、再実行の
/// たびに直前に自分で更新した行を飛ばして次の行を返すため、**`limit` を超える行を確保していた**。
/// 呼び出し側は返った行のうち処理する分しか見ないので、残りは可視期限が切れるまで誰にも処理されない
/// （索引のバックフィルが 15 分止まった原因）。
///
/// 取り出し順は `fair_seq, id`（テナント間の公平性・モジュール冒頭）。`job_queue_fair_idx` を
/// キュー内で先頭から辿り、読み飛ばすのは処理中・バックオフ中の行だけ。
///
/// 同じ文でキューの仮想時刻（`job_queue_vtime`）を確保した行の最大 fair_seq ＋ 1 まで進める。
/// 更新するのは**上回るときだけ**（可視に戻った古い行を取っても下げない。上回らない claim は
/// この行のロックも取らない）。
/// 行が無ければ作る（初回の同時作成で負けた側の値は捨てる＝次の claim で追いつく近似）。
pub async fn claim(
    conn: &mut PgConnection,
    queue: &str,
    vt: Duration,
    limit: i64,
) -> Result<Vec<ClaimedJob>, JobqError> {
    let jobs = sqlx::query_as::<_, ClaimedJob>(
        "with picked as materialized ( \
             select id from job_queue \
             where queue = $1 and visible_at <= now() \
             order by fair_seq, id \
             limit $2 \
             for update skip locked \
         ), claimed as ( \
             update job_queue j set visible_at = now() + $3 * interval '1 second', \
                                    attempts = j.attempts + 1 \
             from picked where j.id = picked.id \
             returning j.id, j.queue, j.tenant_id, j.payload, j.attempts, j.max_attempts, \
                       j.trace_id, j.enqueued_at, j.fair_seq \
         ), top as (select max(fair_seq) + 1 as mx from claimed), \
         bumped as ( \
             update job_queue_vtime v set vt = top.mx from top \
             where v.queue = $1 and v.vt < top.mx returning 1 \
         ), created as ( \
             insert into job_queue_vtime (queue, vt) select $1, top.mx from top \
             where top.mx is not null \
               and not exists (select 1 from job_queue_vtime where queue = $1) \
             on conflict (queue) do nothing returning 1 \
         ) \
         select id, queue, tenant_id, payload, attempts, max_attempts, trace_id, enqueued_at \
         from claimed",
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

/// 処理中のジョブの可視期限を `now() + vt` まで延ばす（ハートビート）。
///
/// 長いジョブは可視期限を短く取り、処理の区切りごとにこれを呼んで延ばす。consumer が落ちれば延長が
/// 止まり、短い期限のうちに他の consumer（再起動後の自分）へ再配信される。ジョブが既に無い
/// （ack 済み・DLQ へ移送済み）なら `false`。
pub async fn extend(conn: &mut PgConnection, id: i64, vt: Duration) -> Result<bool, JobqError> {
    let n = sqlx::query(
        "update job_queue set visible_at = now() + $2 * interval '1 second' where id = $1",
    )
    .bind(id)
    .bind(vt.as_secs_f64())
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n > 0)
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

    // fair_seq は据え置く（仮想時刻は保持しているので巻き戻らない・モジュール冒頭）。
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
    // fair_seq は据え置く: 振り直すと先送りのたびにテナント末尾へ回り、テナント内の投入順と
    // 取り分を崩す（chat は数秒ごとに先送りする）。
    sqlx::query(
        "update job_queue \
         set visible_at = now() + $2 * interval '1 second', \
             attempts = greatest(attempts - 1, 0) \
         where id = $1",
    )
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

/// キューに待機中（配信中を含む）のジョブがあるか。単発の保守ジョブを二重投入しないための判定。
///
/// 判定と投入を原子的にしたい場合は、呼び出し側が同じトランザクションで advisory lock を取ること。
pub async fn has_pending(conn: &mut PgConnection, queue: &str) -> Result<bool, JobqError> {
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from job_queue where queue = $1)")
            .bind(queue)
            .fetch_one(conn)
            .await?;
    Ok(exists)
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
