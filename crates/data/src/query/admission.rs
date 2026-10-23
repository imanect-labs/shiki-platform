//! 予測コストによる受付制御とテナントの公平性（query-optimizer.md §5.4・data-platform §16.8）。
//!
//! - **受付制御**: 予測コストが読取予算（既定 2 秒）を超えるクエリは実行せず、構造化エラー
//!   （[`DataError::QueryTooExpensive`]）で「絞り込みの追加」「索引の昇格・複合索引」を提案する。
//!   **推定値・予測コスト・戦略名はエラーに含めない**（PIT-75）。
//! - **公平性**: テナントごとの同時実行の枠を「本数」ではなく**予測コストのトークン**（ミリ秒）で消費する。
//!   重いクエリ 1 本と軽いクエリ 100 本を同じ物差しで測る。枠はテナントごとに独立しているので、
//!   あるテナントの重いクエリ・統計収集は**そのテナントの枠の中で待つ**（他テナントの枠を使わない）。
//!   枠が空かないまま待ち時間を過ぎたら [`DataError::ReadOverloaded`]（429・再試行可）。
//! - トークンはプロセス内（レプリカごと）。全体の上限は `statement_timeout` と接続プールが担う。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::DataError;

/// 読取予算（予測コストの上限・ミリ秒）。
pub const DEFAULT_READ_BUDGET_MS: u64 = 2_000;
/// テナントあたりの同時実行の枠（予測コストのミリ秒）。
pub const DEFAULT_TENANT_READ_TOKENS: u32 = 8_000;
/// 枠が空くのを待つ上限。
pub(crate) const ADMISSION_WAIT: Duration = Duration::from_secs(3);
/// 予測コストの無い読取（O1・固定戦略）が消費する最小のトークン。
pub(crate) const BASE_TOKENS: u32 = 1;

/// 受付制御の提案（値・件数・推定を含まない語彙だけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuerySuggestion {
    /// 絞り込みの条件を足す。
    NarrowFilter,
    /// 並べ替えの組み合わせの複合索引（data-platform §5.3）。
    CompositeIndex,
    /// 未索引の列の索引を昇格して、その列で絞り込めるようにする（data-platform §10・query-optimizer §5.4）。
    /// 閲覧者が読める・昇格できる未索引の列があるときだけ出す（列名・件数・推定は含めない）。
    PromoteIndex,
}

impl QuerySuggestion {
    pub fn as_str(self) -> &'static str {
        match self {
            QuerySuggestion::NarrowFilter => "narrow_filter",
            QuerySuggestion::CompositeIndex => "composite_index",
            QuerySuggestion::PromoteIndex => "promote_index",
        }
    }
}

/// テナントごとのトークンの枠。
pub(crate) struct ReadGate {
    capacity: u32,
    wait: Duration,
    tenants: Mutex<HashMap<String, Arc<Semaphore>>>,
}

/// 確保したトークン（ドロップで返す）。
pub(crate) struct ReadPermit {
    sem: Arc<Semaphore>,
    held: Option<OwnedSemaphorePermit>,
    capacity: u32,
    wait: Duration,
}

impl ReadGate {
    pub(crate) fn new(capacity: u32) -> Self {
        ReadGate {
            capacity: capacity.max(1),
            wait: ADMISSION_WAIT,
            tenants: Mutex::new(HashMap::new()),
        }
    }

    /// 枠が空くのを待つ上限を変える。
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    fn sem(&self, tenant: &str) -> Arc<Semaphore> {
        let mut m = self
            .tenants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.entry(tenant.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.capacity as usize)))
            .clone()
    }

    /// 読取の入口でトークンを確保する（計画の前・最小量）。
    pub(crate) async fn admit(&self, tenant: &str, tokens: u32) -> Result<ReadPermit, DataError> {
        let sem = self.sem(tenant);
        let n = tokens.clamp(1, self.capacity);
        let p = tokio::time::timeout(self.wait, sem.clone().acquire_many_owned(n))
            .await
            .map_err(|_| DataError::ReadOverloaded)?
            .map_err(|_| DataError::ReadOverloaded)?;
        Ok(ReadPermit {
            sem,
            held: Some(p),
            capacity: self.capacity,
            wait: self.wait,
        })
    }

    /// 統計収集などの背景処理の 1 文分（テナントの枠の 1/4 を使う＝同じテナントの読取と分け合う）。
    pub(crate) async fn background(&self, tenant: &str) -> Result<ReadPermit, DataError> {
        self.admit(tenant, (self.capacity / 4).max(1)).await
    }
}

impl ReadPermit {
    /// 計画後に予測コストの分まで積み増す（既に持つ分を差し引く）。
    pub(crate) async fn top_up(&mut self, tokens: u32) -> Result<(), DataError> {
        let want = tokens.clamp(1, self.capacity);
        let have = self
            .held
            .as_ref()
            .map_or(0, |p| u32::try_from(p.num_permits()).unwrap_or(u32::MAX));
        if want <= have {
            return Ok(());
        }
        // 持っている分を先に返してから全量を取り直す（持ったまま待つと同じテナントの別の読取と
        // 互いに待ち合う）。
        self.held = None;
        let p = tokio::time::timeout(self.wait, self.sem.clone().acquire_many_owned(want))
            .await
            .map_err(|_| DataError::ReadOverloaded)?
            .map_err(|_| DataError::ReadOverloaded)?;
        self.held = Some(p);
        Ok(())
    }
}

/// 予測コスト（µs）→ トークン（ms・切り上げ）。
pub(crate) fn tokens_for(cost_us: f64) -> u32 {
    if !cost_us.is_finite() || cost_us <= 0.0 {
        return BASE_TOKENS;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ms = (cost_us / 1_000.0).ceil().min(f64::from(u32::MAX)) as u32;
    ms.max(BASE_TOKENS)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn tenants_have_independent_budgets() {
        let g = ReadGate::new(10);
        let a = g.admit("a", 10).await.unwrap();
        // 同じテナントは枠が空くまで待つ（ここでは待たずに確かめる）。
        let sem = g.sem("a");
        assert_eq!(sem.available_permits(), 0);
        // 別テナントは影響を受けない。
        let b = g.admit("b", 10).await.unwrap();
        drop(a);
        assert_eq!(sem.available_permits(), 10);
        drop(b);
    }

    #[tokio::test]
    async fn top_up_reacquires_to_the_predicted_amount() {
        let g = ReadGate::new(100);
        let mut p = g.admit("t", 1).await.unwrap();
        p.top_up(40).await.unwrap();
        assert_eq!(g.sem("t").available_permits(), 60);
        p.top_up(10).await.unwrap();
        assert_eq!(g.sem("t").available_permits(), 60, "減らさない");
        p.top_up(1_000).await.unwrap();
        assert_eq!(g.sem("t").available_permits(), 0, "上限で頭打ち");
        drop(p);
        assert_eq!(g.sem("t").available_permits(), 100);
    }

    #[tokio::test]
    async fn exhausted_budget_fails_with_overloaded() {
        let g = ReadGate::new(5).with_wait(Duration::from_millis(20));
        let _hold = g.admit("t", 5).await.unwrap();
        let r = g.admit("t", 1).await;
        assert!(matches!(r, Err(DataError::ReadOverloaded)));
    }

    #[test]
    fn cost_maps_to_millisecond_tokens() {
        assert_eq!(tokens_for(0.0), 1);
        assert_eq!(tokens_for(f64::NAN), 1);
        assert_eq!(tokens_for(1.0), 1);
        assert_eq!(tokens_for(2_500.0), 3);
        assert_eq!(QuerySuggestion::NarrowFilter.as_str(), "narrow_filter");
    }
}
