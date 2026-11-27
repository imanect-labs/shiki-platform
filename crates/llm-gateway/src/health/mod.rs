//! プロバイダの稼働状況（管理コンソール「AI > プロバイダ」の状態表示）。
//!
//! - **計測**: [`Monitored`] がプロバイダを包み、試行ごと（送り直しも 1 回）の結果と最初の
//!   デルタまでの時間（TTFT）を [`HealthRecorder`] に数える。トレイトには手を入れない。
//! - **集計**: 各インスタンスはメモリで分単位に数え、[`HealthRecorder::run_flusher`] が
//!   `llm_provider_health` へ加算で upsert する（複数インスタンスでも足し合わせで揃う）。
//! - **判定**: [`status`] が直近の窓から 4 段階（正常・不安定・停止・利用なし）に落とす。
//!
//! プロバイダはデプロイに属し全テナントで共有するので、記録にテナント・利用者・本文は載せない。
//! 件数は他テナントの利用規模を映すため、外へ出すのは率と時間だけにし、試行の少ない窓では
//! それも伏せる（[`status`]）。

pub mod status;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use futures::Stream;
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::KeyValue;
use sqlx::PgPool;

use crate::model::{GenerateRequest, StreamDelta};
use crate::provider::{DeltaStream, LlmError, LlmProvider};

pub use status::{HealthReason, HealthState, HealthThresholds, HourBar, ProviderHealth};

/// TTFT の度数分布の境界（ミリ秒・各区間の上端）。最後の区間は最大の境界を超えたもの。
///
/// 変えると `llm_provider_health.ttft_hist` の既存行と区間がずれる（保持は 7 日なので、
/// 変えた後 7 日は p95 が少しずれるだけで壊れはしない）。
pub const TTFT_BOUNDS_MS: [u64; 16] = [
    250, 500, 750, 1_000, 1_500, 2_000, 3_000, 4_000, 6_000, 8_000, 12_000, 16_000, 24_000, 32_000,
    48_000, 64_000,
];

/// 分布の区間数（境界の数 ＋ 上限超え）。
const BUCKETS: usize = TTFT_BOUNDS_MS.len() + 1;

/// 書き出しの間隔。画面の更新（30 秒）より短くする。
const FLUSH_INTERVAL: Duration = Duration::from_secs(15);

/// 保持期間。これより古い分の行は書き出し側が消す。
const RETENTION: TimeDelta = TimeDelta::days(7);

/// 古い行を消す間隔（書き出しのたびではなく、この間隔ごとに 1 回）。
const PURGE_EVERY: Duration = Duration::from_hours(1);

/// 試行 1 回の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 最後まで流れた（または流れ始めてから呼び出し側が打ち切った）。
    Ok,
    /// プロバイダ側の障害。
    Failed,
    /// レート制限・利用枠超過。
    RateLimited,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::RateLimited => "rate_limited",
        }
    }

    /// エラーをプロバイダの稼働状況として数えるか。呼び出し側の不備（`BadRequest`・`Config`）は
    /// プロバイダの障害ではないので数えない（`None`）。
    ///
    /// ただし 4xx のうち、どの呼び出しも通らなくなるもの（401/402/403 = キーの失効・残高切れ・
    /// 権限、404 = 接続先やモデル ID の誤り）は、利用者から見ればプロバイダが使えないのと同じなので
    /// 障害として数える。アダプタは 4xx を `"<種別> <状態コード> …: 本文"` の形で `BadRequest` にする。
    fn of_error(e: &LlmError) -> Option<Self> {
        match e {
            LlmError::Unavailable(_) | LlmError::Internal(_) => Some(Outcome::Failed),
            LlmError::RateLimited(_) => Some(Outcome::RateLimited),
            LlmError::BadRequest(m) => {
                matches!(client_status(m), Some(401..=404)).then_some(Outcome::Failed)
            }
            LlmError::Config(_) => None,
        }
    }
}

/// `BadRequest` の文言（`"anthropic 401 Unauthorized: …"`）から状態コードを読む。
fn client_status(message: &str) -> Option<u16> {
    message.split_whitespace().nth(1)?.parse().ok()
}

/// 1 プロバイダ × 1 分の集計。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Bucket {
    ok: i64,
    failed: i64,
    rate_limited: i64,
    ttft: [i64; BUCKETS],
}

/// TTFT を度数分布の区間番号に落とす。
fn ttft_bucket(ttft: Duration) -> usize {
    let ms = u64::try_from(ttft.as_millis()).unwrap_or(u64::MAX);
    TTFT_BOUNDS_MS
        .iter()
        .position(|&b| ms <= b)
        .unwrap_or(TTFT_BOUNDS_MS.len())
}

/// メモリ上の集計（プロバイダ ID・分 → 件数）。
type Pending = HashMap<(String, DateTime<Utc>), Bucket>;

/// 試行の結果をメモリで分単位に数え、DB へ書き出す（プロセスで 1 つを共有する）。
///
/// gateway はチャット・ワークフロー・管理 API ごとに組まれるので、記録先は gateway の外で 1 つ
/// 作って [`GatewayConfig::health`](crate::GatewayConfig::health) で全 gateway に渡す。
#[derive(Debug, Default)]
pub struct HealthRecorder {
    pending: Mutex<Pending>,
}

impl HealthRecorder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 試行 1 回を数える（メモリだけ・ロックは短い）。
    pub fn record(&self, provider: &str, outcome: Outcome, ttft: Option<Duration>) {
        let minute = minute_of(Utc::now());
        if let Ok(mut pending) = self.pending.lock() {
            let b = pending.entry((provider.to_string(), minute)).or_default();
            match outcome {
                Outcome::Ok => b.ok += 1,
                Outcome::Failed => b.failed += 1,
                Outcome::RateLimited => b.rate_limited += 1,
            }
            if let Some(t) = ttft {
                b.ttft[ttft_bucket(t)] += 1;
            }
        }
        record_metrics(provider, outcome, ttft);
    }

    /// 溜まった分を DB へ加算で書き出す。失敗した分はメモリへ戻して次回に回す。
    ///
    /// # Errors
    /// DB への書き込みに失敗したとき（溜まった分は失わない）。
    pub async fn flush(&self, db: &PgPool) -> Result<(), sqlx::Error> {
        let taken: Vec<_> = match self.pending.lock() {
            Ok(mut p) => p.drain().collect(),
            Err(_) => return Ok(()),
        };
        if taken.is_empty() {
            return Ok(());
        }
        let mut left = taken.into_iter();
        while let Some(((provider, minute), b)) = left.next() {
            if let Err(e) = upsert(db, &provider, minute, &b).await {
                self.restore(std::iter::once(((provider, minute), b)).chain(left));
                return Err(e);
            }
        }
        Ok(())
    }

    /// 書き出せなかった分をメモリへ戻す（その間に数えた分と足し合わせる）。
    fn restore(&self, rows: impl Iterator<Item = ((String, DateTime<Utc>), Bucket)>) {
        if let Ok(mut pending) = self.pending.lock() {
            for (key, b) in rows {
                let cur = pending.entry(key).or_default();
                cur.ok += b.ok;
                cur.failed += b.failed;
                cur.rate_limited += b.rate_limited;
                for (c, n) in cur.ttft.iter_mut().zip(b.ttft) {
                    *c += n;
                }
            }
        }
    }

    /// 定期的に書き出し、保持期間を過ぎた行を消し続ける（戻らない・tokio タスクで回す）。
    pub async fn run_flusher(self: Arc<Self>, db: PgPool) {
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);
        let mut last_purge: Option<Instant> = None;
        loop {
            tick.tick().await;
            if let Err(e) = self.flush(&db).await {
                tracing::warn!(error = %e, "LLM プロバイダの稼働状況を書き出せなかった（次回に回す）");
            }
            if last_purge.is_none_or(|t| t.elapsed() >= PURGE_EVERY) {
                last_purge = Some(Instant::now());
                if let Err(e) = purge_expired(&db, Utc::now()).await {
                    tracing::warn!(error = %e, "LLM プロバイダの稼働状況の古い行を消せなかった");
                }
            }
        }
    }
}

/// 分の頭に丸める。
fn minute_of(t: DateTime<Utc>) -> DateTime<Utc> {
    t.duration_trunc(TimeDelta::minutes(1)).unwrap_or(t)
}

async fn upsert(
    db: &PgPool,
    provider: &str,
    minute: DateTime<Utc>,
    b: &Bucket,
) -> Result<(), sqlx::Error> {
    // 度数分布は要素ごとに足す（境界を変えた直後は長さが違うので、足りない側を 0 とみなす）。
    sqlx::query(
        "insert into llm_provider_health as h (provider, minute, ok, failed, rate_limited, ttft_hist)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (provider, minute) do update set
           ok = h.ok + excluded.ok,
           failed = h.failed + excluded.failed,
           rate_limited = h.rate_limited + excluded.rate_limited,
           ttft_hist = array(
             select coalesce(a, 0) + coalesce(b, 0)
               from unnest(h.ttft_hist, excluded.ttft_hist) as u(a, b))",
    )
    .bind(provider)
    .bind(minute)
    .bind(b.ok)
    .bind(b.failed)
    .bind(b.rate_limited)
    .bind(b.ttft.as_slice())
    .execute(db)
    .await?;
    Ok(())
}

async fn purge_expired(db: &PgPool, now: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let r = sqlx::query("delete from llm_provider_health where minute < $1")
        .bind(now - RETENTION)
        .execute(db)
        .await?;
    Ok(r.rows_affected())
}

struct Metrics {
    attempts: Counter<u64>,
    ttft: Histogram<f64>,
}

/// 同じ数字を OTel にも出す（運営の Grafana・アラート用）。exporter が無ければ no-op。
fn record_metrics(provider: &str, outcome: Outcome, ttft: Option<Duration>) {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    let m = METRICS.get_or_init(|| {
        let meter = opentelemetry::global::meter("shiki-server");
        Metrics {
            attempts: meter.u64_counter("llm.provider.attempts").build(),
            ttft: meter
                .f64_histogram("llm.provider.ttft")
                .with_unit("s")
                .build(),
        }
    });
    let p = KeyValue::new("provider", provider.to_string());
    m.attempts
        .add(1, &[p.clone(), KeyValue::new("outcome", outcome.as_str())]);
    if let Some(t) = ttft {
        m.ttft.record(t.as_secs_f64(), &[p]);
    }
}

/// アダプタの中で 429 を待って送り直した分（[`note_rate_limit_wait`]）。
#[derive(Debug, Default, Clone, Copy)]
struct RateLimitWaits {
    count: u32,
    waited: Duration,
}

tokio::task_local! {
    static RATE_LIMIT_WAITS: std::cell::Cell<RateLimitWaits>;
}

/// アダプタが 429 の待ち指示に従って送り直す前に呼ぶ。外の [`Monitored`] はその 429 を
/// 1 回の試行（レート制限）として数え、待った時間を TTFT から除く。包まれていなければ何もしない。
pub(crate) fn note_rate_limit_wait(wait: Duration) {
    let _ = RATE_LIMIT_WAITS.try_with(|c| {
        let mut w = c.get();
        w.count += 1;
        w.waited += wait;
        c.set(w);
    });
}

/// プロバイダを包み、試行ごとの結果と TTFT を数える。
pub(crate) struct Monitored {
    inner: Arc<dyn LlmProvider>,
    id: String,
    health: Arc<HealthRecorder>,
}

impl Monitored {
    pub(crate) fn wrap(
        inner: Arc<dyn LlmProvider>,
        id: &str,
        health: Arc<HealthRecorder>,
    ) -> Arc<dyn LlmProvider> {
        Arc::new(Self {
            inner,
            id: id.to_string(),
            health,
        })
    }
}

#[async_trait::async_trait]
impl LlmProvider for Monitored {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        let begun = Instant::now();
        let (result, waits) = RATE_LIMIT_WAITS
            .scope(std::cell::Cell::default(), async {
                let r = self.inner.stream(req).await;
                (r, RATE_LIMIT_WAITS.with(std::cell::Cell::get))
            })
            .await;
        // 中で待って送り直した 429 も 1 回の試行として数える（送り直した側は下で数える）。
        for _ in 0..waits.count {
            self.health.record(&self.id, Outcome::RateLimited, None);
        }
        // 待った時間はプロバイダの応答の遅さではないので TTFT に含めない。
        let started = begun + waits.waited;
        match result {
            Ok(s) => Ok(Box::pin(Watched {
                inner: s,
                id: self.id.clone(),
                health: Arc::clone(&self.health),
                started,
                ttft: None,
                settled: false,
                outcome: None,
                recorded: false,
            })),
            Err(e) => {
                if let Some(o) = Outcome::of_error(&e) {
                    self.health.record(&self.id, o, None);
                }
                Err(e)
            }
        }
    }
}

/// 流れているストリームを見て、終わり（または破棄）で 1 回だけ数える。
struct Watched {
    inner: DeltaStream,
    id: String,
    health: Arc<HealthRecorder>,
    started: Instant,
    ttft: Option<Duration>,
    /// 結果が決まったか（エラーを見たか、最後まで流れたか）。
    settled: bool,
    /// 決まった結果（数えないエラーは `None`）。
    outcome: Option<Outcome>,
    recorded: bool,
}

impl Watched {
    fn finish(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        // 結果が決まる前に破棄された＝呼び出し側の打ち切り。流れ始めていればプロバイダは
        // 応えていたので正常と数え、流れ始める前なら何も分からないので数えない。
        let outcome = if self.settled {
            self.outcome
        } else {
            self.ttft.map(|_| Outcome::Ok)
        };
        if let Some(o) = outcome {
            self.health.record(&self.id, o, self.ttft);
        }
    }
}

impl Stream for Watched {
    type Item = Result<StreamDelta, LlmError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let item = self.inner.as_mut().poll_next(cx);
        if let Poll::Ready(ref it) = item {
            if !self.settled {
                match it {
                    Some(Ok(_)) => {
                        if self.ttft.is_none() {
                            self.ttft = Some(self.started.elapsed());
                        }
                    }
                    Some(Err(e)) => {
                        self.settled = true;
                        self.outcome = Outcome::of_error(e);
                    }
                    None => {
                        self.settled = true;
                        self.outcome = Some(Outcome::Ok);
                    }
                }
                if self.settled {
                    self.finish();
                }
            }
        }
        item
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests;
