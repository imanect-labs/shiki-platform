//! DLQ（`job_queue_dead`）の運用: 件数の監視（メトリクス）・保持期間での削除・管理用の一覧（#1335）。
//!
//! DLQ はジョブが試行上限に達したとき（[`crate::fail`]）と恒久エラー（[`crate::kill`]）の終着点で、
//! 落ちたジョブは誰かが原因を直して [`crate::requeue_dead`] で戻すまで処理されない。顧客から見ると
//! 「文書が検索に出ない」「返事が来ない」として静かに現れるので、件数をメトリクスに出して気づけるように
//! する（[`DeadLetterMonitor`]）。
//!
//! payload にはテナントのデータ（識別子など）が入り得るので、期限なく持たない（[`purge_dead_older_than`]）。
//! 管理用の一覧（[`list_dead`]）も payload を返さない（原因は `last_error` で分かる）。
//! メトリクスのラベルはキュー名だけで、テナントは入れない（カーディナリティとテナント情報の両方の理由）。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use opentelemetry::KeyValue;
use sqlx::{PgConnection, PgPool};

use crate::JobqError;

/// DLQ の保持期間の既定値。
pub const DEFAULT_DEAD_RETENTION: Duration = Duration::from_hours(24 * 30);

/// 監視（件数の集計と期限切れの削除）の間隔の既定値。
pub const DEFAULT_MONITOR_INTERVAL: Duration = Duration::from_mins(1);

/// 1 文で消す DLQ 行の上限（長い DELETE で他の書込を待たせない）。
const PURGE_BATCH: i64 = 1000;

/// キューごとの DLQ の状況。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DeadQueueStats {
    pub queue: String,
    pub count: i64,
    /// 最も古い行が DLQ に落ちた時刻。
    pub oldest_died_at: DateTime<Utc>,
}

/// 管理用の DLQ の 1 件（payload は含めない・モジュール冒頭）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeadJobSummary {
    pub id: i64,
    pub queue: String,
    pub tenant_id: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub last_error: String,
    pub enqueued_at: DateTime<Utc>,
    pub died_at: DateTime<Utc>,
}

/// キューごとの DLQ の件数と最古の時刻（DLQ が空のキューは含まない）。
pub async fn dead_queue_stats(conn: &mut PgConnection) -> Result<Vec<DeadQueueStats>, JobqError> {
    let rows = sqlx::query_as::<_, DeadQueueStats>(
        // tenant-sql: allow(全テナント横断の DLQ 件数（運用メトリクス・キュー単位の集計だけを返す）)
        "select queue, count(*) as count, min(died_at) as oldest_died_at \
         from job_queue_dead group by queue order by queue",
    )
    .fetch_all(conn)
    .await?;
    Ok(rows)
}

/// DLQ を新しい順に最大 `limit` 件返す（`queue` を渡すとそのキューだけ）。payload は返さない。
pub async fn list_dead(
    conn: &mut PgConnection,
    queue: Option<&str>,
    limit: i64,
) -> Result<Vec<DeadJobSummary>, JobqError> {
    let rows = sqlx::query_as::<_, DeadJobSummary>(
        // tenant-sql: allow(運用者の provisioner プレーンの DLQ 一覧・payload は返さない)
        "select id, queue, tenant_id, attempts, max_attempts, last_error, enqueued_at, died_at \
         from job_queue_dead where ($1::text is null or queue = $1) \
         order by died_at desc, id desc limit $2",
    )
    .bind(queue)
    .bind(limit)
    .fetch_all(conn)
    .await?;
    Ok(rows)
}

/// DLQ に落ちてから `age` を過ぎた行を消す。消した件数を返す。
///
/// 1 文 [`PURGE_BATCH`] 件ずつ、空になるまで繰り返す。`SKIP LOCKED` なので複数レプリカが同時に
/// 回しても待ち合わない（同じ行を二重に消そうとしない）。
pub async fn purge_dead_older_than(
    conn: &mut PgConnection,
    age: Duration,
) -> Result<u64, JobqError> {
    let mut total = 0;
    loop {
        let n = sqlx::query(
            // tenant-sql: allow(全テナント横断の DLQ 保持期間（期限切れの行だけを消す）)
            "delete from job_queue_dead where id in ( \
                 select id from job_queue_dead \
                 where died_at < now() - $1 * interval '1 second' \
                 order by died_at limit $2 for update skip locked)",
        )
        .bind(age.as_secs_f64())
        .bind(PURGE_BATCH)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        total += n;
        if n < u64::try_from(PURGE_BATCH).unwrap_or(u64::MAX) {
            return Ok(total);
        }
    }
}

/// 運用者向けの DLQ の窓口（管理 API が使う・テナントのプールが分かれても全プールを見る）。
///
/// API 層へ生のプールを渡さないためのチョークポイント（`AppState` は `PgPool` を公開しない）。
pub struct DeadLetterAdmin {
    pools: Vec<PgPool>,
}

impl DeadLetterAdmin {
    pub fn new(pools: Vec<PgPool>) -> Self {
        DeadLetterAdmin { pools }
    }

    /// キューごとの件数と最古の時刻（全プールの合算）。
    pub async fn stats(&self) -> Result<Vec<DeadQueueStats>, JobqError> {
        let mut merged: HashMap<String, DeadQueueStats> = HashMap::new();
        for pool in &self.pools {
            let mut conn = pool.acquire().await?;
            for s in dead_queue_stats(&mut conn).await? {
                merged
                    .entry(s.queue.clone())
                    .and_modify(|m| {
                        m.count += s.count;
                        m.oldest_died_at = m.oldest_died_at.min(s.oldest_died_at);
                    })
                    .or_insert(s);
            }
        }
        let mut stats: Vec<DeadQueueStats> = merged.into_values().collect();
        stats.sort_by(|a, b| a.queue.cmp(&b.queue));
        Ok(stats)
    }

    /// 新しい順に最大 `limit` 件（全プールから集めて並べ直す）。payload は返さない。
    pub async fn list(
        &self,
        queue: Option<&str>,
        limit: i64,
    ) -> Result<Vec<DeadJobSummary>, JobqError> {
        let mut all = Vec::new();
        for pool in &self.pools {
            let mut conn = pool.acquire().await?;
            all.extend(list_dead(&mut conn, queue, limit).await?);
        }
        all.sort_by(|a, b| b.died_at.cmp(&a.died_at).then(b.id.cmp(&a.id)));
        all.truncate(usize::try_from(limit).unwrap_or(0));
        Ok(all)
    }

    /// 1 件を元のキューへ戻す（[`crate::requeue_dead`]）。どのプールにも無ければ `false`。
    pub async fn requeue(&self, id: i64) -> Result<bool, JobqError> {
        for pool in &self.pools {
            let mut conn = pool.acquire().await?;
            if crate::requeue_dead(&mut conn, id).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// DLQ の監視: 一定間隔で期限切れの行を消し、キューごとの件数と最古の経過時間をメトリクスに出す。
///
/// メトリクス（OTel のゲージ・ラベルは `queue` だけ）:
/// - `jobq.dead_jobs` — DLQ の件数
/// - `jobq.dead_oldest_age_seconds` — 最も古い行が DLQ に落ちてからの秒数
///
/// 空になったキューは 0 を出し続ける（アラートを解除させるため）。全レプリカで動かしてよい
/// （削除は `SKIP LOCKED`・ゲージはレプリカごとに同じ値を出すので、集計側は `max` で見る）。
pub struct DeadLetterMonitor {
    pools: Vec<PgPool>,
    interval: Duration,
    retention: Duration,
}

impl DeadLetterMonitor {
    pub fn new(pools: Vec<PgPool>) -> Self {
        DeadLetterMonitor {
            pools,
            interval: DEFAULT_MONITOR_INTERVAL,
            retention: DEFAULT_DEAD_RETENTION,
        }
    }

    #[must_use]
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    #[must_use]
    pub fn with_retention(mut self, retention: Duration) -> Self {
        self.retention = retention;
        self
    }

    /// バックグラウンドで回す（プロセス生存中は走り続ける）。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        register_gauges();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(self.interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(e) = self.run_once().await {
                    tracing::warn!(error = %e, "jobq: DLQ の監視に失敗（次の周期で再試行）");
                }
            }
        })
    }

    /// 1 周期ぶん（期限切れの削除 → 集計 → メトリクスの更新）。集計結果を返す（テスト・運用用）。
    pub async fn run_once(&self) -> Result<Vec<DeadQueueStats>, JobqError> {
        for pool in &self.pools {
            let mut conn = pool.acquire().await?;
            let purged = purge_dead_older_than(&mut conn, self.retention).await?;
            if purged > 0 {
                tracing::info!(purged, "jobq: 保持期間を過ぎた DLQ の行を消しました");
            }
        }
        let stats = DeadLetterAdmin::new(self.pools.clone()).stats().await?;
        publish(&stats, Utc::now());
        Ok(stats)
    }
}

/// キュー → (件数, 最古の経過秒)。
type Published = HashMap<String, (u64, f64)>;

/// ゲージが読む最新の値。
fn latest() -> &'static Mutex<Published> {
    static LATEST: OnceLock<Mutex<Published>> = OnceLock::new();
    LATEST.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 集計結果を反映する。今回現れなかったキューは 0 にする（消さない・モジュール冒頭）。
fn publish(stats: &[DeadQueueStats], now: DateTime<Utc>) {
    let mut m = latest()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for v in m.values_mut() {
        *v = (0, 0.0);
    }
    for s in stats {
        let age = (now - s.oldest_died_at)
            .to_std()
            .map_or(0.0, |d| d.as_secs_f64());
        m.insert(s.queue.clone(), (u64::try_from(s.count).unwrap_or(0), age));
    }
}

/// 最新の値の写し（ゲージのコールバックとテストが読む）。
fn published_snapshot() -> Published {
    latest()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn register_gauges() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let meter = opentelemetry::global::meter("shiki-jobq");
        // 戻り値の計器を落としても登録したコールバックは残る（SDK がメータープロバイダで保持する）。
        let _count = meter
            .u64_observable_gauge("jobq.dead_jobs")
            .with_description("DLQ（job_queue_dead）の件数（キュー別）")
            .with_callback(|obs| {
                for (queue, (count, _)) in published_snapshot() {
                    obs.observe(count, &[KeyValue::new("queue", queue)]);
                }
            })
            .build();
        let _age = meter
            .f64_observable_gauge("jobq.dead_oldest_age_seconds")
            .with_description("DLQ の最も古い行が落ちてからの秒数（キュー別）")
            .with_callback(|obs| {
                for (queue, (_, age)) in published_snapshot() {
                    obs.observe(age, &[KeyValue::new("queue", queue)]);
                }
            })
            .build();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_keeps_vanished_queues_at_zero() {
        let now = Utc::now();
        let s = |q: &str, n: i64, secs: i64| DeadQueueStats {
            queue: q.into(),
            count: n,
            oldest_died_at: now - chrono::Duration::seconds(secs),
        };
        publish(&[s("unit-a", 3, 120), s("unit-b", 1, 5)], now);
        let snap = published_snapshot();
        assert_eq!(snap.get("unit-a"), Some(&(3, 120.0)));
        assert_eq!(snap.get("unit-b"), Some(&(1, 5.0)));
        // unit-a が空になったら 0 を出す（消さない）。
        publish(&[s("unit-b", 2, 6)], now);
        let snap = published_snapshot();
        assert_eq!(snap.get("unit-a"), Some(&(0, 0.0)));
        assert_eq!(snap.get("unit-b"), Some(&(2, 6.0)));
    }
}
