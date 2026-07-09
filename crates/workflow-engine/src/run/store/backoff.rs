//! リトライ backoff の決定的遅延計算（engine.md §7.4）。

use uuid::Uuid;

/// backoff（指数＋full jitter・engine.md §7.4）。jitter の乱数は (run_id, step_path, attempt) の
/// FNV ハッシュから決定的に導く（Math.random 不使用・リプレイ安全・thundering herd 回避）。
/// **run_id を種に含める**ことで、共有障害・429 storm でも run ごとに遅延が分散する（同一ノードの
/// 全 run が同時に起きない）。
pub(super) fn next_retry_delay_secs(run_id: Uuid, step_path: &str, attempt: i32) -> i64 {
    let base: i64 = 2;
    let cap: i64 = 300;
    let rand01 = deterministic_rand01(run_id, step_path, attempt);
    crate::retry::backoff_with_jitter(attempt, base, cap, rand01)
}

/// (run_id, step_path, attempt) から `[0, 1)` の決定的乱数を導く（FNV-1a → 正規化）。
fn deterministic_rand01(run_id: Uuid, step_path: &str, attempt: i32) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in run_id
        .as_bytes()
        .iter()
        .copied()
        .chain(step_path.bytes())
        .chain(attempt.to_le_bytes())
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // 上位 53 bit を [0,1) へ。
    #[allow(clippy::cast_precision_loss)]
    let v = (h >> 11) as f64 / (1u64 << 53) as f64;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_bounded_and_deterministic() {
        // full jitter: [1, ceiling]。ceiling は base*2^attempt を cap=300 で頭打ち。
        let rid = Uuid::nil();
        assert!((1..=2).contains(&next_retry_delay_secs(rid, "a", 0)));
        assert!(next_retry_delay_secs(rid, "a", 20) <= 300);
        // 同じ (step, attempt) は同じ遅延（リプレイ安全）。
        assert_eq!(
            next_retry_delay_secs(rid, "a", 3),
            next_retry_delay_secs(rid, "a", 3)
        );
    }
}
