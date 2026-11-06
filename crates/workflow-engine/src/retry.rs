//! リトライ分類とバックオフ（engine.md §7.4・**純関数**）。
//!
//! - retryable: 一時障害。指数バックオフ＋full jitter で再試行（attempt 消費）。
//! - permanent: 恒久障害。即 terminal（失敗）。
//! - rate_limited: レート超過。再試行するが **attempt を消費しない**（順番待ちに近い扱い）。
//! - wait: 待てば通る失敗（取り込み待ち・効果の記録の予約の残り）。**attempt を消費せず**、間隔を伸ばしながら
//!   やり直し、待つ上限を過ぎたらその誤りで失敗にする（[`WaitPolicy`]・D-125・GAP-28）。

use crate::capability::journal::{EFFECT_IN_PROGRESS, RESERVATION_RECLAIM_SECS};

/// エラーのリトライ分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    /// 一時障害（バックオフ再試行・attempt 消費）。
    Retryable,
    /// 恒久障害（即失敗）。
    Permanent,
    /// レート超過（再試行・attempt 非消費）。
    RateLimited,
    /// 待てば通る失敗（attempt 非消費・待つ上限つき・[`wait_policy`]）。
    Wait,
}

/// 試す回数を使わずに待ってやり直す失敗の、待ち方（D-125・GAP-50）。
///
/// 待つ間隔は、待ち始めてからの時間と同じだけ（おおむね倍々）にし、`first_delay_secs` と `max_delay_secs` で挟む。
/// 待ち始めてから `max_wait_secs` を過ぎたら、その誤りで失敗にする（試す回数の残りは使わない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPolicy {
    /// 最初に待つ秒数。
    pub first_delay_secs: i64,
    /// 1 回に待つ秒数の上限。
    pub max_delay_secs: i64,
    /// 待ち始めてからの上限（秒）。
    pub max_wait_secs: i64,
}

/// 「ファイルを読む」が PDF・Office の本文の取り込み（社内文書の検索）を待つ上限（分）。画面の注記もこの値を出す。
pub const INDEX_WAIT_MAX_MINUTES: i64 = 30;

/// 取り込み待ち（`index_pending`）の待ち方。5 秒から始めて最大 2 分おきに、30 分まで読み直す（およそ 20 回）。
pub const INDEX_WAIT: WaitPolicy = WaitPolicy {
    first_delay_secs: 5,
    max_delay_secs: 120,
    max_wait_secs: INDEX_WAIT_MAX_MINUTES * 60,
};

/// 効果の記録の予約が結果の無いまま残っている（`effect_in_progress`）ときの待ち方（GAP-28）。
///
/// 前の実行（リースが切れた・落ちたワーカー）の予約は、回収の猶予（[`RESERVATION_RECLAIM_SECS`]）を過ぎると取り直せる
/// （外への送信は「送ったか分からない」と分かる）。その前に記録が付けば、記録した結果を返す。5 秒から始めて最大 1 分おきに、
/// 猶予に 30 秒を足した時間まで（およそ 10 回）やり直す。待ち始めたときには予約はもう作られているので、この間に必ず
/// 猶予を過ぎる。
pub const EFFECT_WAIT: WaitPolicy = WaitPolicy {
    first_delay_secs: 5,
    max_delay_secs: 60,
    max_wait_secs: RESERVATION_RECLAIM_SECS as i64 + 30,
};

/// 試す回数を使わずに待つ code と待ち方（ここに無い code は待たない・単一定義）。
#[must_use]
pub fn wait_policy(code: &str) -> Option<WaitPolicy> {
    if code == crate::nodes::ports::INDEX_PENDING {
        Some(INDEX_WAIT)
    } else if code == EFFECT_IN_PROGRESS {
        Some(EFFECT_WAIT)
    } else {
        None
    }
}

impl WaitPolicy {
    /// 待ち始めてから `waited_secs` 秒たったときに次に待つ秒数（上限を過ぎていれば `None`＝失敗にする）。
    ///
    /// 最後の 1 回は上限の時刻ちょうどに読む（上限より先へ待たない）。
    #[must_use]
    pub fn next_delay_secs(self, waited_secs: i64) -> Option<i64> {
        let waited = waited_secs.max(0);
        if waited >= self.max_wait_secs {
            return None;
        }
        let delay = waited.clamp(self.first_delay_secs, self.max_delay_secs);
        Some(delay.min(self.max_wait_secs - waited).max(1))
    }
}

/// エラーコード＋retryable フラグから分類する（engine.md §7.4 の写像）。
///
/// [`wait_policy`] のある code（取り込み待ち）は `Wait`。コードが `rate_limited` / `429` / `throttled` /
/// `concurrency_limited` のいずれかなら `RateLimited`（attempt 非消費・順番待ち）、それ以外は `retryable` フラグで
/// Retryable / Permanent。
pub fn classify(code: &str, retryable: bool) -> RetryClass {
    if wait_policy(code).is_some() {
        return RetryClass::Wait;
    }
    let c = code.to_ascii_lowercase();
    if c == "rate_limited"
        || c == "429"
        || c == "throttled"
        || c == "too_many_requests"
        || c == "concurrency_limited"
    {
        RetryClass::RateLimited
    } else if retryable {
        RetryClass::Retryable
    } else {
        RetryClass::Permanent
    }
}

/// full jitter バックオフ（engine.md §7.4）: `delay = rand[0, min(cap, base * 2^attempt)]`。
///
/// `rand01` は `[0.0, 1.0)` の乱数（呼び出し側が供給・決定的テスト可能）。返す秒は `>= 1`。
/// thundering herd を避けるため上限内で一様乱択する（指数増加は上限に効く）。
pub fn backoff_with_jitter(attempt: i32, base_secs: i64, cap_secs: i64, rand01: f64) -> i64 {
    let base = base_secs.max(1);
    let cap = cap_secs.max(base);
    // base * 2^attempt（オーバーフロー飽和）。
    let exp = 1_i64
        .checked_shl(u32::try_from(attempt.max(0)).unwrap_or(0))
        .unwrap_or(i64::MAX);
    let ceiling = base.saturating_mul(exp).min(cap);
    // rand01 を [0, ceiling] へ写す。
    let r = rand01.clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    let delay = (r * ceiling as f64) as i64;
    delay.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rate_limited_regardless_of_retryable() {
        assert_eq!(classify("rate_limited", false), RetryClass::RateLimited);
        assert_eq!(classify("429", true), RetryClass::RateLimited);
        assert_eq!(classify("THROTTLED", false), RetryClass::RateLimited);
    }

    #[test]
    fn classify_retryable_vs_permanent() {
        assert_eq!(classify("timeout", true), RetryClass::Retryable);
        assert_eq!(classify("bad_request", false), RetryClass::Permanent);
    }

    /// 取り込み待ちは試す回数を使わずに待つ（retryable の値によらない・D-125）。
    #[test]
    fn index_pending_waits() {
        assert_eq!(classify("index_pending", true), RetryClass::Wait);
        assert_eq!(classify("index_pending", false), RetryClass::Wait);
        assert_eq!(wait_policy("index_pending"), Some(INDEX_WAIT));
        assert_eq!(wait_policy("unavailable"), None);
        assert_eq!(INDEX_WAIT.max_wait_secs, 30 * 60);
    }

    /// 効果の記録の予約の残りも試す回数を使わずに待ち、予約を回収できる時刻（猶予）より後まで待つ（GAP-28）。
    #[test]
    fn effect_in_progress_waits_past_the_reclaim_window() {
        assert_eq!(classify(EFFECT_IN_PROGRESS, true), RetryClass::Wait);
        let p = wait_policy(EFFECT_IN_PROGRESS).expect("待つ");
        assert_eq!(p, EFFECT_WAIT);
        assert!(p.max_wait_secs > i64::from(RESERVATION_RECLAIM_SECS));
        let mut waited = 0;
        let mut tries = 0;
        while let Some(d) = p.next_delay_secs(waited) {
            waited += d;
            tries += 1;
        }
        assert_eq!(waited, p.max_wait_secs);
        assert!((5..=15).contains(&tries), "{tries}");
    }

    /// 待つ間隔はおおむね倍々に伸び、2 分で頭打ちになり、30 分の時刻ちょうどに最後の 1 回を読んで終える。
    #[test]
    fn wait_delays_grow_and_stop_at_the_limit() {
        let p = INDEX_WAIT;
        let mut waited = 0;
        let mut delays = Vec::new();
        while let Some(d) = p.next_delay_secs(waited) {
            assert!((1..=p.max_delay_secs).contains(&d), "{d}");
            delays.push(d);
            waited += d;
        }
        assert_eq!(&delays[..7], &[5, 5, 10, 20, 40, 80, 120]);
        assert_eq!(waited, p.max_wait_secs, "上限の時刻ちょうどで止まる");
        assert!(
            delays.len() < 25,
            "読み直しは 30 分でおよそ 20 回: {}",
            delays.len()
        );
        assert_eq!(p.next_delay_secs(p.max_wait_secs), None);
        assert_eq!(p.next_delay_secs(p.max_wait_secs + 5), None);
        // 時計のずれで負になっても最初の間隔で待つ。
        assert_eq!(p.next_delay_secs(-3), Some(5));
    }

    #[test]
    fn jitter_within_ceiling() {
        // attempt=3, base=2, cap=300 → ceiling = min(2*8, 300) = 16。
        for i in 0..=100 {
            let r = f64::from(i) / 100.0;
            let d = backoff_with_jitter(3, 2, 300, r);
            assert!((1..=16).contains(&d), "delay {d} は [1,16]");
        }
    }

    #[test]
    fn jitter_respects_cap() {
        // 大きな attempt でも cap を超えない。
        let d = backoff_with_jitter(40, 2, 300, 1.0);
        assert!(d <= 300);
    }

    #[test]
    fn jitter_min_one() {
        assert_eq!(backoff_with_jitter(0, 2, 300, 0.0), 1);
    }
}
