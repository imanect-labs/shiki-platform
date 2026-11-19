//! 集計（`llm_provider_health`）からプロバイダの状態を判定する。
//!
//! 外へ出すのは率と時間だけ（件数は他テナントの利用規模を映すので出さない）。

use std::collections::HashMap;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::{minute_of, BUCKETS, TTFT_BOUNDS_MS};

/// 状態判定の閾値（デプロイ設定 `llm.health`）。初期値は運用しながら詰める前提。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthThresholds {
    /// 現在の状態を判定する窓（分）。
    pub window_minutes: u32,
    /// これ以上の障害率で「不安定」。
    pub degraded_failure_rate: f64,
    /// これ以上の障害率で「停止」。
    pub down_failure_rate: f64,
    /// 障害がこの件数に満たなければ、率によらず障害とみなさない（単発の失敗で揺らさない）。
    pub min_failures: u32,
    /// 試行に占めるレート制限の割合がこれ以上で「不安定」。
    pub rate_limited_rate: f64,
    /// TTFT の p95 がこれ（ミリ秒）を超えたら「不安定」。
    pub slow_ttft_p95_ms: u64,
    /// 試行がこれに満たなければ「停止」にせず「不安定」に留める（少数の失敗、たとえば 1 人の
    /// 要求が続けて失敗しただけで、全テナントのモデル選択に「停止中」を出さない）。
    pub min_attempts_for_down: u32,
    /// 試行がこれに満たない窓・時間は、率と時間（障害率・稼働率・TTFT）を返さない。
    /// 集計は全テナント共有なので、分母が小さい率は他テナントの件数そのものになる。
    pub min_attempts_for_rates: u32,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            window_minutes: 5,
            degraded_failure_rate: 0.02,
            down_failure_rate: 0.2,
            min_failures: 2,
            rate_limited_rate: 0.2,
            slow_ttft_p95_ms: 15_000,
            min_attempts_for_down: 5,
            min_attempts_for_rates: 20,
        }
    }
}

/// プロバイダの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Operational,
    Degraded,
    Down,
    /// 窓の中に試行が無い（判定できない）。
    Idle,
}

/// 「不安定」「停止」の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthReason {
    /// 障害率が閾値を超えた。
    Errors,
    /// レート制限が続いている。
    RateLimited,
    /// 最初の応答が遅い。
    Slow,
}

/// 1 時間ぶんの稼働。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HourBar {
    /// 時間の頭（UTC）。
    pub start: DateTime<Utc>,
    pub state: HealthState,
    /// 稼働率（障害でない試行の割合・レート制限は除く）。試行が無ければ `None`。
    pub availability: Option<f64>,
}

/// プロバイダ 1 件の状態。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderHealth {
    pub provider: String,
    pub state: HealthState,
    pub reasons: Vec<HealthReason>,
    /// 窓の中の障害率（レート制限は除く）。試行が無ければ `None`。
    pub failure_rate: Option<f64>,
    /// 窓の中の試行に占めるレート制限の割合。
    pub rate_limited_rate: Option<f64>,
    /// 窓の中の TTFT の p95（度数分布の区間の上端・ミリ秒）。
    pub ttft_p95_ms: Option<u64>,
    /// 最後に成功した試行の分（保持期間 7 日の中で）。
    pub last_ok_at: Option<DateTime<Utc>>,
    /// 直近 24 時間の稼働率。
    pub availability_24h: Option<f64>,
    /// 直近 24 時間を 1 時間ずつ（古い順・最後が今の時間）。
    pub hours: Vec<HourBar>,
}

/// 件数（判定用・外へは出さない）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    ok: i64,
    failed: i64,
    rate_limited: i64,
}

impl Counts {
    fn add(&mut self, o: Counts) {
        self.ok += o.ok;
        self.failed += o.failed;
        self.rate_limited += o.rate_limited;
    }

    fn attempts(self) -> i64 {
        self.ok + self.failed + self.rate_limited
    }

    #[allow(clippy::cast_precision_loss)] // 件数は f64 の仮数に十分収まる
    fn failure_rate(self) -> Option<f64> {
        let n = self.ok + self.failed;
        (n > 0).then(|| self.failed as f64 / n as f64)
    }

    #[allow(clippy::cast_precision_loss)]
    fn rate_limited_rate(self) -> Option<f64> {
        let n = self.attempts();
        (n > 0).then(|| self.rate_limited as f64 / n as f64)
    }
}

/// 件数と TTFT から状態を決める（純関数）。
fn classify(c: Counts, slow: bool, t: &HealthThresholds) -> (HealthState, Vec<HealthReason>) {
    if c.attempts() == 0 {
        return (HealthState::Idle, Vec::new());
    }
    let mut reasons = Vec::new();
    let mut state = HealthState::Operational;
    let failure = c.failure_rate().unwrap_or(0.0);
    if c.failed >= i64::from(t.min_failures) && failure >= t.degraded_failure_rate {
        reasons.push(HealthReason::Errors);
        state = if failure >= t.down_failure_rate
            && c.attempts() >= i64::from(t.min_attempts_for_down)
        {
            HealthState::Down
        } else {
            HealthState::Degraded
        };
    }
    if c.rate_limited_rate().unwrap_or(0.0) >= t.rate_limited_rate {
        reasons.push(HealthReason::RateLimited);
    }
    if slow {
        reasons.push(HealthReason::Slow);
    }
    if state == HealthState::Operational && !reasons.is_empty() {
        state = HealthState::Degraded;
    }
    (state, reasons)
}

/// 度数分布の p95 が入る区間（`None` は標本なし）。
fn p95_bucket(hist: &[i64; BUCKETS]) -> Option<usize> {
    let n: i64 = hist.iter().sum();
    if n == 0 {
        return None;
    }
    // ceil(0.95 n) 番目が入る区間。整数で計算する。
    let target = (n * 95 + 99) / 100;
    let mut acc = 0;
    for (i, c) in hist.iter().enumerate() {
        acc += c;
        if acc >= target {
            return Some(i);
        }
    }
    Some(BUCKETS - 1)
}

/// 区間の上端（表示用の「〜以内」）。最後の区間（上限超え）は最大の境界。
fn upper(i: usize) -> Option<u64> {
    TTFT_BOUNDS_MS.get(i).or(TTFT_BOUNDS_MS.last()).copied()
}

/// 区間の下端（遅さの判定用。p95 は少なくともこれを超えている）。
fn lower(i: usize) -> u64 {
    i.checked_sub(1)
        .and_then(|j| TTFT_BOUNDS_MS.get(j))
        .copied()
        .unwrap_or(0)
}

/// 度数分布の p95（区間の上端）。
fn p95(hist: &[i64; BUCKETS]) -> Option<u64> {
    p95_bucket(hist).and_then(upper)
}

/// p95 が閾値を確実に超えているか（区間の下端で比べる。上端で比べると、閾値を含む区間に
/// 入っただけで閾値より速い p95 まで「遅い」にしてしまう）。
fn p95_exceeds(hist: &[i64; BUCKETS], threshold_ms: u64) -> bool {
    p95_bucket(hist).is_some_and(|i| lower(i) >= threshold_ms)
}

/// 時間ごとの合計（プロバイダ・時間・成功・障害・レート制限）。
type HourRow = (String, DateTime<Utc>, i64, i64, i64);
/// 窓の中の分ごとの行（プロバイダ・成功・障害・レート制限・TTFT 分布）。
type WindowRow = (String, i64, i64, i64, Vec<i64>);

/// 「最後の成功」の粒度。分単位で返すと、他テナントの要求の時刻が読み取れる。
const LAST_OK_GRAIN: TimeDelta = TimeDelta::minutes(15);

fn coarse(t: DateTime<Utc>) -> DateTime<Utc> {
    t.duration_trunc(LAST_OK_GRAIN).unwrap_or(t)
}

fn hour_of(t: DateTime<Utc>) -> DateTime<Utc> {
    t.duration_trunc(TimeDelta::hours(1)).unwrap_or(t)
}

/// 窓の中の件数と TTFT 分布（プロバイダごと）。
async fn window_counts(
    db: &PgPool,
    providers: &[String],
    now: DateTime<Utc>,
    t: &HealthThresholds,
) -> Result<HashMap<String, (Counts, [i64; BUCKETS])>, sqlx::Error> {
    let from = minute_of(now) - TimeDelta::minutes(i64::from(t.window_minutes.max(1)) - 1);
    let rows: Vec<WindowRow> = sqlx::query_as(
        "select provider, ok, failed, rate_limited, ttft_hist
           from llm_provider_health
          where provider = any($1) and minute >= $2",
    )
    .bind(providers)
    .bind(from)
    .fetch_all(db)
    .await?;
    let mut out: HashMap<String, (Counts, [i64; BUCKETS])> = HashMap::new();
    for (p, ok, failed, rate_limited, hist) in rows {
        let (c, h) = out.entry(p).or_default();
        c.add(Counts {
            ok,
            failed,
            rate_limited,
        });
        for (acc, n) in h.iter_mut().zip(hist) {
            *acc += n;
        }
    }
    Ok(out)
}

/// 今の状態だけ（利用者のモデル選択の注意表示用・1 クエリ）。並びは `providers` の順。
///
/// # Errors
/// DB の読み取りに失敗したとき。
pub async fn read_states(
    db: &PgPool,
    providers: &[String],
    now: DateTime<Utc>,
    t: &HealthThresholds,
) -> Result<Vec<HealthState>, sqlx::Error> {
    let in_window = window_counts(db, providers, now, t).await?;
    Ok(providers
        .iter()
        .map(|p| {
            let (c, hist) = in_window.get(p).copied().unwrap_or_default();
            classify(c, p95_exceeds(&hist, t.slow_ttft_p95_ms), t).0
        })
        .collect())
}

/// `providers` の状態を `now` 時点で判定する（並びは `providers` の順）。
///
/// # Errors
/// DB の読み取りに失敗したとき。
pub async fn read(
    db: &PgPool,
    providers: &[String],
    now: DateTime<Utc>,
    t: &HealthThresholds,
) -> Result<Vec<ProviderHealth>, sqlx::Error> {
    let first_hour = hour_of(now) - TimeDelta::hours(23);

    // 3 本は互いに依存しないので並べて読む。
    let hourly_q = sqlx::query_as::<_, HourRow>(
        "select provider, date_trunc('hour', minute) as hour,
                sum(ok)::bigint, sum(failed)::bigint, sum(rate_limited)::bigint
           from llm_provider_health
          where provider = any($1) and minute >= $2
          group by provider, hour",
    )
    .bind(providers)
    .bind(first_hour)
    .fetch_all(db);
    // 最後の成功はプロバイダごとに主キー (provider, minute) を新しい順に 1 行だけ引く。
    let last_ok_q = sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
        "select p, (select h.minute
                      from llm_provider_health h
                     where h.provider = p and h.ok > 0
                     order by h.minute desc
                     limit 1)
           from unnest($1::text[]) as p",
    )
    .bind(providers)
    .fetch_all(db);
    let (hourly, in_window, last_ok) =
        tokio::try_join!(hourly_q, window_counts(db, providers, now, t), last_ok_q)?;

    let mut by_hour: HashMap<(&str, DateTime<Utc>), Counts> = HashMap::new();
    for (p, hour, ok, failed, rate_limited) in &hourly {
        by_hour.entry((p.as_str(), *hour)).or_default().add(Counts {
            ok: *ok,
            failed: *failed,
            rate_limited: *rate_limited,
        });
    }
    let last_ok: HashMap<&str, DateTime<Utc>> = last_ok
        .iter()
        .filter_map(|(p, m)| m.map(|m| (p.as_str(), m)))
        .collect();

    Ok(providers
        .iter()
        .map(|p| {
            let (c, hist) = in_window.get(p).copied().unwrap_or_default();
            let ttft_p95_ms = p95(&hist);
            let (state, reasons) = classify(c, p95_exceeds(&hist, t.slow_ttft_p95_ms), t);
            // 分母が小さい率・時間は返さない（他テナントの件数・要求の時刻が透けるため）。
            let enough = |n: Counts| n.attempts() >= i64::from(t.min_attempts_for_rates);
            let mut day = Counts::default();
            let hours = (0..24)
                .map(|i| {
                    let start = first_hour + TimeDelta::hours(i);
                    let hc = by_hour
                        .get(&(p.as_str(), start))
                        .copied()
                        .unwrap_or_default();
                    day.add(hc);
                    HourBar {
                        start,
                        state: classify(hc, false, t).0,
                        availability: hc.failure_rate().filter(|_| enough(hc)).map(|f| 1.0 - f),
                    }
                })
                .collect();
            ProviderHealth {
                provider: p.clone(),
                state,
                reasons,
                failure_rate: c.failure_rate().filter(|_| enough(c)),
                rate_limited_rate: c.rate_limited_rate().filter(|_| enough(c)),
                ttft_p95_ms: ttft_p95_ms.filter(|_| enough(c)),
                last_ok_at: last_ok.get(p.as_str()).map(|m| coarse(*m)),
                availability_24h: day.failure_rate().filter(|_| enough(day)).map(|f| 1.0 - f),
                hours,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(ok: i64, failed: i64, rate_limited: i64) -> Counts {
        Counts {
            ok,
            failed,
            rate_limited,
        }
    }

    #[test]
    fn no_attempts_is_idle() {
        let t = HealthThresholds::default();
        assert_eq!(classify(c(0, 0, 0), false, &t), (HealthState::Idle, vec![]));
    }

    #[test]
    fn single_failure_does_not_flip_state() {
        let t = HealthThresholds::default();
        assert_eq!(classify(c(3, 1, 0), false, &t).0, HealthState::Operational);
    }

    #[test]
    fn failure_rate_maps_to_degraded_and_down() {
        let t = HealthThresholds::default();
        assert_eq!(
            classify(c(90, 2, 0), false, &t),
            (HealthState::Degraded, vec![HealthReason::Errors])
        );
        assert_eq!(
            classify(c(8, 2, 0), false, &t),
            (HealthState::Down, vec![HealthReason::Errors])
        );
        assert_eq!(classify(c(0, 5, 0), false, &t).0, HealthState::Down);
        // 試行が少なければ停止にはせず不安定に留める。
        assert_eq!(classify(c(1, 3, 0), false, &t).0, HealthState::Degraded);
    }

    #[test]
    fn rate_limits_and_slowness_degrade_without_errors() {
        let t = HealthThresholds::default();
        assert_eq!(
            classify(c(8, 0, 2), false, &t),
            (HealthState::Degraded, vec![HealthReason::RateLimited])
        );
        assert_eq!(
            classify(c(10, 0, 0), true, &t),
            (HealthState::Degraded, vec![HealthReason::Slow])
        );
        // 停止の理由が複数あっても状態は停止のまま。
        assert_eq!(
            classify(c(0, 3, 3), true, &t),
            (
                HealthState::Down,
                vec![
                    HealthReason::Errors,
                    HealthReason::RateLimited,
                    HealthReason::Slow
                ]
            )
        );
    }

    #[test]
    fn p95_returns_upper_bound_of_bucket() {
        let mut h = [0; BUCKETS];
        assert_eq!(p95(&h), None);
        h[3] = 95; // ≤ 1000ms
        h[7] = 5; // ≤ 4000ms
        assert_eq!(p95(&h), Some(1_000));
        h[7] = 6;
        assert_eq!(p95(&h), Some(4_000));
        let mut over = [0; BUCKETS];
        over[BUCKETS - 1] = 1;
        assert_eq!(p95(&over), Some(64_000));
    }

    #[test]
    fn slowness_is_judged_by_the_lower_edge() {
        // 12.5 秒は (12s, 16s] の区間。閾値 15 秒を超えているとは言えないので遅いとしない。
        let mut h = [0; BUCKETS];
        h[11] = 10;
        assert!(!p95_exceeds(&h, 15_000));
        // (16s, 24s] は確実に 15 秒を超えている。
        let mut h = [0; BUCKETS];
        h[12] = 10;
        assert!(p95_exceeds(&h, 15_000));
        assert!(!p95_exceeds(&[0; BUCKETS], 15_000));
    }
}
