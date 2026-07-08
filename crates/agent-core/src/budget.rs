//! 予算ガード（Task 5.7）。
//!
//! 長ホライズンの暴走を防ぐ: **最大ステップ・最大経過時間・トークン上限・コスト上限**を設定し、
//! llm-gateway のトークン会計（`Usage`）と連動して累積、超過時に安全停止する。上限接近では
//! 警告（[`BudgetCheck::Warn`]）を出し、UI/承認（5.6）で続行可否を促せるようにする。
//!
//! 判定は**純粋関数**（[`Budget::check`]）に閉じ、単体＋proptest で網羅する。時刻は呼び出し側が
//! [`Instant`] を渡す（テスト可能性・決定性のため内部で now を取らない）。

use std::time::Instant;

/// 予算の種別（どの軸で警告/超過したか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetKind {
    Steps,
    Time,
    Tokens,
    Cost,
}

impl BudgetKind {
    /// 監査・UI 表示用の安定文字列。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetKind::Steps => "steps",
            BudgetKind::Time => "time",
            BudgetKind::Tokens => "tokens",
            BudgetKind::Cost => "cost",
        }
    }
}

/// 累積消費（1 セッション）。チェックポイントに載るため直列化可能。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Spent {
    pub steps: usize,
    pub tokens: u64,
    pub cost_usd_micros: i64,
}

impl Spent {
    /// 1 ステップ分の消費を足し込む（step は +1）。
    pub fn add_step(&mut self, tokens: u64, cost_usd_micros: i64) {
        self.steps = self.steps.saturating_add(1);
        self.tokens = self.tokens.saturating_add(tokens);
        self.cost_usd_micros = self.cost_usd_micros.saturating_add(cost_usd_micros);
    }
}

/// 予算上限。`None` は当該軸を無制限にする（Chat プロファイルは token/cost を付けない）。
#[derive(Debug, Clone)]
pub struct Budget {
    /// LLM 呼び出し回数の上限（安全停止・必須）。
    pub max_steps: usize,
    /// 全体デッドライン（超えたらステップ境界で停止）。
    pub deadline: Option<Instant>,
    /// 累積トークン（prompt+completion）上限。
    pub max_tokens: Option<u64>,
    /// 累積コスト（マイクロ USD）上限。
    pub max_cost_usd_micros: Option<i64>,
    /// 警告を出す割合（0.0..=1.0）。既定 0.8。上限の当該割合到達で `Warn`。
    pub warn_at_fraction: f64,
}

impl Budget {
    /// Chat プロファイル既定（短ホライズン・token/cost 無制限・現行挙動と互換）。
    #[must_use]
    pub fn chat(max_steps: usize) -> Self {
        Budget {
            max_steps,
            deadline: None,
            max_tokens: None,
            max_cost_usd_micros: None,
            warn_at_fraction: 0.8,
        }
    }

    /// Autonomous プロファイル既定（長ホライズン・全軸に上限）。
    #[must_use]
    pub fn autonomous(
        max_steps: usize,
        deadline: Option<Instant>,
        max_tokens: u64,
        max_cost_usd_micros: i64,
    ) -> Self {
        Budget {
            max_steps,
            deadline,
            max_tokens: Some(max_tokens),
            max_cost_usd_micros: Some(max_cost_usd_micros),
            warn_at_fraction: 0.8,
        }
    }

    /// 現在の消費・時刻に対する予算判定。**次のステップに入る前**に呼ぶ。
    ///
    /// 超過は `Exceeded`（安全停止）、上限の `warn_at_fraction` 到達は `Warn`（続行可）、
    /// いずれも無ければ `Ok`。複数軸が同時該当なら「超過 > 警告」かつ steps→time→tokens→cost の順で 1 つ返す。
    // cost は `.max(0)` 後に u64 化するため符号消失は起きない（金額は非負）。
    #[allow(clippy::cast_sign_loss)]
    #[must_use]
    pub fn check(&self, spent: &Spent, now: Instant) -> BudgetCheck {
        // --- 超過（安全停止）を最優先で検出する。 ---
        if spent.steps >= self.max_steps {
            return BudgetCheck::Exceeded(BudgetKind::Steps);
        }
        if self.deadline.is_some_and(|d| now >= d) {
            return BudgetCheck::Exceeded(BudgetKind::Time);
        }
        if let Some(max) = self.max_tokens {
            if spent.tokens >= max {
                return BudgetCheck::Exceeded(BudgetKind::Tokens);
            }
        }
        if let Some(max) = self.max_cost_usd_micros {
            if spent.cost_usd_micros >= max {
                return BudgetCheck::Exceeded(BudgetKind::Cost);
            }
        }
        // --- 警告（上限接近）。steps→time→tokens→cost の順で最初の 1 軸。 ---
        let frac = self.warn_at_fraction.clamp(0.0, 1.0);
        if fraction_reached(spent.steps as u64, self.max_steps as u64, frac) {
            return BudgetCheck::Warn(BudgetKind::Steps, spent.steps as u64, self.max_steps as u64);
        }
        if let Some(max) = self.max_tokens {
            if fraction_reached(spent.tokens, max, frac) {
                return BudgetCheck::Warn(BudgetKind::Tokens, spent.tokens, max);
            }
        }
        if let Some(max) = self.max_cost_usd_micros {
            let (used, lim) = (spent.cost_usd_micros.max(0) as u64, max.max(0) as u64);
            if fraction_reached(used, lim, frac) {
                return BudgetCheck::Warn(BudgetKind::Cost, used, lim);
            }
        }
        BudgetCheck::Ok
    }
}

/// `used` が `limit * frac` 以上か（limit=0 は「無制限扱い」で常に false）。
// 予算のトークン/コストは実運用で 2^52 に達しないため、割合判定の f64 変換で精度は問題にならない。
#[allow(clippy::cast_precision_loss)]
fn fraction_reached(used: u64, limit: u64, frac: f64) -> bool {
    if limit == 0 {
        return false;
    }
    // 整数で threshold = ceil(limit * frac) 相当。浮動小数は割合換算にのみ使う。
    let threshold = (limit as f64 * frac).ceil();
    used as f64 >= threshold
}

/// 予算判定の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetCheck {
    /// 余裕あり。
    Ok,
    /// 上限接近（種別・現在値・上限）。続行可・警告イベントを 1 回出す。
    Warn(BudgetKind, u64, u64),
    /// 超過（安全停止すべき）。
    Exceeded(BudgetKind),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::time::Duration;

    fn spent(steps: usize, tokens: u64, cost: i64) -> Spent {
        Spent {
            steps,
            tokens,
            cost_usd_micros: cost,
        }
    }

    #[test]
    fn steps_exceeded_stops() {
        let b = Budget::chat(8);
        assert_eq!(
            b.check(&spent(8, 0, 0), Instant::now()),
            BudgetCheck::Exceeded(BudgetKind::Steps)
        );
    }

    #[test]
    fn tokens_and_cost_exceeded() {
        let b = Budget::autonomous(1000, None, 5000, 10_000);
        assert_eq!(
            b.check(&spent(1, 5000, 0), Instant::now()),
            BudgetCheck::Exceeded(BudgetKind::Tokens)
        );
        assert_eq!(
            b.check(&spent(1, 0, 10_000), Instant::now()),
            BudgetCheck::Exceeded(BudgetKind::Cost)
        );
    }

    #[test]
    fn deadline_exceeded() {
        let now = Instant::now();
        let past = now - Duration::from_secs(1);
        let b = Budget {
            deadline: Some(past),
            ..Budget::chat(100)
        };
        assert_eq!(
            b.check(&spent(0, 0, 0), now),
            BudgetCheck::Exceeded(BudgetKind::Time)
        );
    }

    #[test]
    fn warns_before_limit() {
        let b = Budget::autonomous(1000, None, 100, 1_000_000);
        // 80% で警告。
        assert_eq!(
            b.check(&spent(1, 80, 0), Instant::now()),
            BudgetCheck::Warn(BudgetKind::Tokens, 80, 100)
        );
        // 79% は Ok（steps/cost も未達）。
        assert_eq!(b.check(&spent(1, 79, 0), Instant::now()), BudgetCheck::Ok);
    }

    #[test]
    fn unlimited_axis_never_triggers() {
        let b = Budget::chat(1000); // token/cost は None
        assert_eq!(
            b.check(&spent(1, u64::MAX, i64::MAX), Instant::now()),
            BudgetCheck::Ok
        );
    }

    proptest! {
        // 超過は必ず安全停止（Exceeded）を返す。決して Ok にならない。
        #[test]
        fn exceeded_never_ok(steps in 0usize..50, tokens in 0u64..10_000, cost in 0i64..10_000) {
            let b = Budget::autonomous(10, None, 1000, 500);
            let s = spent(steps, tokens, cost);
            let over = steps >= 10 || tokens >= 1000 || cost >= 500;
            let res = b.check(&s, Instant::now());
            if over {
                prop_assert!(matches!(res, BudgetCheck::Exceeded(_)));
            }
        }

        // check は now を進めても steps/token/cost 判定を壊さない（時間軸のみ now 依存）。
        #[test]
        fn monotonic_in_tokens(t1 in 0u64..1000, t2 in 0u64..1000) {
            let b = Budget::autonomous(1_000_000, None, 1000, 1_000_000);
            let (lo, hi) = (t1.min(t2), t1.max(t2));
            let r_lo = b.check(&spent(1, lo, 0), Instant::now());
            let r_hi = b.check(&spent(1, hi, 0), Instant::now());
            // 消費が多い方が「より逼迫」: Ok<Warn<Exceeded の順序を崩さない。
            let rank = |c: BudgetCheck| match c {
                BudgetCheck::Ok => 0,
                BudgetCheck::Warn(..) => 1,
                BudgetCheck::Exceeded(_) => 2,
            };
            prop_assert!(rank(r_lo) <= rank(r_hi));
        }
    }
}
