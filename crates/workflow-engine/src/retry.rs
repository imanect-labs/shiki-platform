//! リトライ分類とバックオフ（engine.md §7.4・**純関数**）。
//!
//! - retryable: 一時障害。指数バックオフ＋full jitter で再試行（attempt 消費）。
//! - permanent: 恒久障害。即 terminal（失敗）。
//! - rate_limited: レート超過。再試行するが **attempt を消費しない**（順番待ちに近い扱い）。

/// エラーのリトライ分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    /// 一時障害（バックオフ再試行・attempt 消費）。
    Retryable,
    /// 恒久障害（即失敗）。
    Permanent,
    /// レート超過（再試行・attempt 非消費）。
    RateLimited,
}

/// エラーコード＋retryable フラグから分類する（engine.md §7.4 の写像）。
///
/// コードが `rate_limited` / `429` / `throttled` のいずれかなら `RateLimited`、
/// それ以外は `retryable` フラグで Retryable / Permanent。
pub fn classify(code: &str, retryable: bool) -> RetryClass {
    let c = code.to_ascii_lowercase();
    if c == "rate_limited" || c == "429" || c == "throttled" || c == "too_many_requests" {
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
