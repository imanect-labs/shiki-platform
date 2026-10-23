//! フィードバック（query-optimizer.md §8・data-platform §14）: 実行ごとの記録とテーブル単位の集計。
//!
//! - 第一の指標は**走査効率**（走査した索引エントリ数 ÷ 返した行数）。値・内部キー・推定値は利用者へ
//!   返さない（運用向けの数値指標だけ・PIT-75）。メモリ上の集計で、OTel への抽出は後続タスク。
//! - **補正係数**: ソート駆動の「予想した走査件数」と「観測した走査件数」の比を、
//!   `(テーブル, フィルタ列, ソート列)` ごとに指数移動平均（対数で・初回はそのまま）で持つ。相関
//!   （§1.2 ④）は次回から吸収される。学習はテーブルと列の組の単位に限り、利用者ごとには持たない。
//! - **予測と実測の相関**: 選んだ戦略の予測コスト（µs）と実際の所要時間を直近 [`SAMPLES`] 件持ち、
//!   対数のピアソン相関を出す（較正の効き目の確認）。
//! - 推定と実測が 10 倍以上ずれることが [`MISS_STREAK`] 回続いたら、統計収集の前倒しを求める。
//! - 深いページで劣化し続ける複数ソートは**複合索引の材料化**、未索引列での絞り込み要求は
//!   **索引の昇格**の候補として管理画面へ出す（列の参照名と回数だけ）。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use uuid::Uuid;

use crate::model::FieldId;

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
    /// 最初に選んだ戦略（切替の前・運用指標）。
    pub first_strategy: &'static str,
    /// 本体経路（v1）で実行したか。
    pub body_path: bool,
    /// 事後検査で落とした行数（0 であるべき）。
    pub verify_dropped: u64,
    /// 複数ソートの同値群が先読みの上限を超え、複合索引の材料化を要求した。
    pub composite_requested: bool,
    /// 統計（O2）で計画したか。
    pub planned_with_stats: bool,
    /// 選んだ戦略の予測コスト（µs）。
    pub predicted_us: Option<u64>,
    /// 実行の所要時間（µs・読取 Tx の中）。
    pub elapsed_us: u64,
    /// 予測コストが読取予算を超えて実行しなかった（受付制御）。
    pub admission_rejected: bool,
    /// 候補件数の推定が実測と 10 倍以上ずれた。
    pub estimate_miss: bool,
}

/// テーブル単位の集計（運用指標・値と推定値は含まない）。
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
    /// 受付制御で断った回数。
    pub rejections: u64,
    /// 推定外れの回数。
    pub estimate_misses: u64,
}

/// 管理画面へ出す索引の候補の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum CandidateKind {
    /// 複数ソートの複合索引の材料化（data-platform §5.3）。
    CompositeIndex,
    /// 未索引列の索引の昇格（data-platform §10）。
    IndexPromotion,
}

/// 補正係数のキー（テーブル, フィルタ列, 第 1 ソート列）。
pub(crate) type ShapeKey = (Uuid, Vec<FieldId>, Option<FieldId>);

const RECENT: usize = 256;
/// 予測と実測の組の保持数。
pub(crate) const SAMPLES: usize = 2_048;
/// 統計収集を前倒しする推定外れの連続回数。
pub(crate) const MISS_STREAK: u32 = 3;
/// 補正係数の指数移動平均の重み（新しい観測の重み）。
const EMA_ALPHA: f64 = 0.3;

#[derive(Debug, Default)]
struct Agg {
    requests: u64,
    scanned: u64,
    returned: u64,
    count_queries: u64,
    switches: u64,
    verify_dropped: u64,
    rejections: u64,
    misses: u64,
    miss_streak: u32,
    ratios: VecDeque<f64>,
    strategies: HashMap<&'static str, u64>,
}

/// フィードバックの記録器（プロセス内）。
#[derive(Debug, Default)]
pub(crate) struct Feedback {
    tables: Mutex<HashMap<Uuid, Agg>>,
    /// 補正係数（対数）。
    factors: Mutex<HashMap<ShapeKey, f64>>,
    /// (予測 µs, 実測 µs)。
    samples: Mutex<VecDeque<(f64, f64)>>,
    candidates: Mutex<HashMap<Uuid, CandidateCounts>>,
}

/// テーブルごとの候補の回数（(種類, 列) → 回数）。
type CandidateCounts = HashMap<(CandidateKind, Vec<String>), u64>;

impl Feedback {
    /// 記録する。統計収集を前倒しすべきなら `true`（推定外れが続いた）。
    pub(crate) fn record(&self, table_id: Uuid, s: &QueryExecStats) -> bool {
        if let (Some(p), true) = (s.predicted_us, s.elapsed_us > 0) {
            if let Ok(mut v) = self.samples.lock() {
                if v.len() == SAMPLES {
                    v.pop_front();
                }
                #[allow(clippy::cast_precision_loss)]
                v.push_back((p as f64, s.elapsed_us as f64));
            }
        }
        let Ok(mut t) = self.tables.lock() else {
            return false;
        };
        let a = t.entry(table_id).or_default();
        a.requests += 1;
        a.scanned += s.scanned;
        a.returned += s.returned;
        a.count_queries += u64::from(s.count_issued);
        a.switches += u64::from(s.switched);
        a.verify_dropped += s.verify_dropped;
        a.rejections += u64::from(s.admission_rejected);
        a.misses += u64::from(s.estimate_miss);
        let mut due = false;
        if s.planned_with_stats {
            if s.estimate_miss {
                a.miss_streak += 1;
                if a.miss_streak >= MISS_STREAK {
                    a.miss_streak = 0;
                    due = true;
                }
            } else {
                a.miss_streak = 0;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let ratio = s.scanned as f64 / s.returned.max(1) as f64;
        if a.ratios.len() == RECENT {
            a.ratios.pop_front();
        }
        a.ratios.push_back(ratio);
        *a.strategies.entry(s.strategy).or_default() += 1;
        due
    }

    pub(crate) fn snapshot(&self, table_id: Uuid) -> Option<TableScanMetrics> {
        let t = self.tables.lock().ok()?;
        let a = t.get(&table_id)?;
        let mut r: Vec<f64> = a.ratios.iter().copied().collect();
        r.sort_by(f64::total_cmp);
        let p95 = if r.is_empty() {
            0.0
        } else {
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
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
            rejections: a.rejections,
            estimate_misses: a.misses,
        })
    }

    /// 補正係数（観測が無ければ 1）。
    pub(crate) fn factor(&self, key: &ShapeKey) -> f64 {
        self.factors
            .lock()
            .ok()
            .and_then(|m| m.get(key).copied())
            .map_or(1.0, f64::exp)
    }

    /// ソート駆動の予想走査件数と観測した走査件数から補正係数を更新する。
    pub(crate) fn observe_sort(&self, key: ShapeKey, predicted: f64, observed: f64) {
        if !(predicted.is_finite() && observed.is_finite()) || predicted <= 0.0 {
            return;
        }
        let r = (observed.max(1.0) / predicted.max(1.0)).ln();
        if let Ok(mut m) = self.factors.lock() {
            if m.len() >= 65_536 {
                m.clear();
            }
            m.entry(key)
                .and_modify(|f| *f = (1.0 - EMA_ALPHA) * *f + EMA_ALPHA * r)
                .or_insert(r);
        }
    }

    /// 予測コストと実測の相関（対数のピアソン）と組の数。
    pub(crate) fn correlation(&self) -> (Option<f64>, usize) {
        let Ok(v) = self.samples.lock() else {
            return (None, 0);
        };
        let pts: Vec<(f64, f64)> = v
            .iter()
            .filter(|(p, a)| *p > 0.0 && *a > 0.0)
            .map(|(p, a)| (p.ln(), a.ln()))
            .collect();
        (pearson(&pts), pts.len())
    }

    /// 予測と実測の組を捨てる（較正の前後を分けて測るため）。
    pub(crate) fn clear_samples(&self) {
        if let Ok(mut v) = self.samples.lock() {
            v.clear();
        }
    }

    /// 管理画面の候補を数える。
    pub(crate) fn candidate(&self, table_id: Uuid, kind: CandidateKind, fields: Vec<String>) {
        if let Ok(mut m) = self.candidates.lock() {
            *m.entry(table_id)
                .or_default()
                .entry((kind, fields))
                .or_default() += 1;
        }
    }

    /// テーブルの候補（回数の多い順）。
    pub(crate) fn candidates(&self, table_id: Uuid) -> Vec<(CandidateKind, Vec<String>, u64)> {
        let Ok(m) = self.candidates.lock() else {
            return Vec::new();
        };
        let mut v: Vec<(CandidateKind, Vec<String>, u64)> = m
            .get(&table_id)
            .map(|c| c.iter().map(|((k, f), n)| (*k, f.clone(), *n)).collect())
            .unwrap_or_default();
        v.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        v
    }
}

#[allow(clippy::cast_precision_loss)]
fn pearson(p: &[(f64, f64)]) -> Option<f64> {
    if p.len() < 3 {
        return None;
    }
    let n = p.len() as f64;
    let (mx, my) = (
        p.iter().map(|x| x.0).sum::<f64>() / n,
        p.iter().map(|x| x.1).sum::<f64>() / n,
    );
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in p {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
        syy += (y - my) * (y - my);
    }
    (sxx > 0.0 && syy > 0.0).then(|| sxy / (sxx * syy).sqrt())
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

    #[test]
    fn correction_factor_learns_from_first_observation_then_ema() {
        let f = Feedback::default();
        let k: ShapeKey = (Uuid::new_v4(), vec![16], Some(1));
        assert_eq!(f.factor(&k), 1.0);
        f.observe_sort(k.clone(), 400.0, 100_000.0);
        assert!((f.factor(&k) - 250.0).abs() < 1e-6, "初回はそのまま");
        f.observe_sort(k.clone(), 400.0, 400.0);
        let g = f.factor(&k);
        assert!(g > 1.0 && g < 250.0, "{g}");
    }

    #[test]
    fn miss_streak_requests_analyze_and_correlation_is_recorded() {
        let f = Feedback::default();
        let t = Uuid::new_v4();
        let miss = QueryExecStats {
            planned_with_stats: true,
            estimate_miss: true,
            ..Default::default()
        };
        assert!(!f.record(t, &miss));
        assert!(!f.record(t, &miss));
        assert!(f.record(t, &miss), "3 回続いたら前倒し");
        for i in 1..=10u64 {
            f.record(
                t,
                &QueryExecStats {
                    predicted_us: Some(100 * i),
                    elapsed_us: 120 * i,
                    ..Default::default()
                },
            );
        }
        let (c, n) = f.correlation();
        assert_eq!(n, 10);
        assert!(c.unwrap() > 0.99);
        f.candidate(
            t,
            CandidateKind::CompositeIndex,
            vec!["a".into(), "b".into()],
        );
        f.candidate(
            t,
            CandidateKind::CompositeIndex,
            vec!["a".into(), "b".into()],
        );
        assert_eq!(f.candidates(t)[0].2, 2);
    }
}
