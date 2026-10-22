//! フィードバック（query-optimizer.md §8・data-platform §14）: 実行ごとの記録とテーブル単位の集計。
//!
//! 第一の指標は**走査効率**（走査した索引エントリ数 ÷ 返した行数）。値・内部キー・推定は持たない
//! （運用向けの数値指標だけ・PIT-75）。メモリ上の集計で、OTel への抽出は後続タスク。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use uuid::Uuid;

/// 1 リクエストの記録。
#[allow(clippy::struct_excessive_bools)] // 実行の事実（件数・切替・安全計画・本体経路）の記録そのもの。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryExecStats {
    /// 走査した索引エントリ数（駆動の走査＋候補集合）。
    pub scanned: u64,
    /// 返した行数。
    pub returned: u64,
    /// 発行した文の数（読取 Tx の設定を除く）。
    pub statements: u32,
    /// 件数クエリを発行したか。
    pub count_issued: bool,
    /// 件数探索の本数（キャッシュ命中は数えない）。
    pub probes: u32,
    /// 別戦略へ切り替えたか（1 リクエスト 1 回まで）。
    pub switched: bool,
    /// 同じ戦略で予算を倍にして続けた回数。
    pub doublings: u32,
    /// 安全計画に落ちたか。
    pub safe_fallback: bool,
    /// 最後に使った戦略（運用指標。利用者へは返さない）。
    pub strategy: &'static str,
    /// 本体経路（v1）で実行したか。
    pub body_path: bool,
    /// 事後検査で落とした行数（0 であるべき）。
    pub verify_dropped: u64,
}

/// テーブル単位の集計（運用指標）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableScanMetrics {
    pub table_id: Uuid,
    pub requests: u64,
    pub scanned: u64,
    pub returned: u64,
    pub count_queries: u64,
    pub switches: u64,
    pub verify_dropped: u64,
    /// 走査効率（走査 ÷ max(返却, 1)）の p95（直近の記録から）。
    pub scan_ratio_p95: f64,
    /// 戦略ごとの回数。
    pub strategies: Vec<(&'static str, u64)>,
}

const RECENT: usize = 256;

#[derive(Debug, Default)]
struct Agg {
    requests: u64,
    scanned: u64,
    returned: u64,
    count_queries: u64,
    switches: u64,
    verify_dropped: u64,
    ratios: VecDeque<f64>,
    strategies: HashMap<&'static str, u64>,
}

/// フィードバックの記録器（プロセス内）。
#[derive(Debug, Default)]
pub(crate) struct Feedback {
    tables: Mutex<HashMap<Uuid, Agg>>,
}

impl Feedback {
    pub(crate) fn record(&self, table_id: Uuid, s: &QueryExecStats) {
        let Ok(mut t) = self.tables.lock() else {
            return;
        };
        let a = t.entry(table_id).or_default();
        a.requests += 1;
        a.scanned += s.scanned;
        a.returned += s.returned;
        a.count_queries += u64::from(s.count_issued);
        a.switches += u64::from(s.switched);
        a.verify_dropped += s.verify_dropped;
        #[allow(clippy::cast_precision_loss)]
        let ratio = s.scanned as f64 / s.returned.max(1) as f64;
        if a.ratios.len() == RECENT {
            a.ratios.pop_front();
        }
        a.ratios.push_back(ratio);
        *a.strategies.entry(s.strategy).or_default() += 1;
    }

    pub(crate) fn snapshot(&self, table_id: Uuid) -> Option<TableScanMetrics> {
        let t = self.tables.lock().ok()?;
        let a = t.get(&table_id)?;
        let mut r: Vec<f64> = a.ratios.iter().copied().collect();
        r.sort_by(f64::total_cmp);
        let p95 = if r.is_empty() {
            0.0
        } else {
            let i = ((r.len() as f64) * 0.95).ceil() as usize;
            r[i.clamp(1, r.len()) - 1]
        };
        let mut strategies: Vec<(&'static str, u64)> =
            a.strategies.iter().map(|(k, v)| (*k, *v)).collect();
        strategies.sort_unstable();
        Some(TableScanMetrics {
            table_id,
            requests: a.requests,
            scanned: a.scanned,
            returned: a.returned,
            count_queries: a.count_queries,
            switches: a.switches,
            verify_dropped: a.verify_dropped,
            scan_ratio_p95: p95,
            strategies,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::float_cmp)]
    use super::*;

    #[test]
    fn aggregates_per_table() {
        let f = Feedback::default();
        let t = Uuid::new_v4();
        for i in 0..10u64 {
            f.record(
                t,
                &QueryExecStats {
                    scanned: 100 * (i + 1),
                    returned: 10,
                    strategy: "sort",
                    ..Default::default()
                },
            );
        }
        let s = f.snapshot(t).unwrap();
        assert_eq!(s.requests, 10);
        assert_eq!(s.scanned, 5500);
        assert_eq!(s.scan_ratio_p95, 100.0);
        assert_eq!(s.strategies, vec![("sort", 10)]);
        assert!(f.snapshot(Uuid::new_v4()).is_none());
    }
}
