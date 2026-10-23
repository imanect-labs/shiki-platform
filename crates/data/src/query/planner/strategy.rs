//! 戦略の選択（O2・query-optimizer.md §5.2〜§5.4）: 統計から各戦略のコスト区間（µs）を見積もり、
//! **頑健な選択**で 1 つ選ぶ。統計が無いテーブル（O1）は [`super::exec`] の件数探索だけで選ぶ。
//!
//! 1. 各戦略（ソート駆動・フィルタ駆動・OR 駆動・交差駆動・認可駆動）のコストを、選択率の推定区間の
//!    両端と中央で評価する（[`super::cost`]）。ソート駆動の走査件数には補正係数（§8）を掛ける。
//! 2. 推定が「推測」の候補は上限付き件数探索（§4.3・1 計画 3 本まで）で区間を狭める。
//! 3. 中央値のコストが最小の戦略 A の最悪コストが、全戦略の最悪コストの最小値の **4 倍**を超えるなら、
//!    最悪コストが最小の戦略を選ぶ（§5.3）。
//! 4. 予測コストが読取予算を超え、推定が「推測」でなければ実行しない（受付制御・§5.4）。
//!
//! 推定・コスト・戦略名は利用者へ返さない（PIT-75）。運用指標（フィードバック）にだけ値を含まない数値で残す。

use std::sync::Arc;

use super::catalog::{StatsCache, TableStats};
use super::estimate::{Conf, Est, Estimator};
use super::exec::{Exec, K_PREFETCH};
use super::lower::Source;
use super::plans::{all_cand_plans, CandPlan};
use super::stats::PROBE_CAP;
use crate::query::admission::QuerySuggestion;
use crate::query::bound::{BSort, Expr};
use crate::DataError;

/// 頑健な選択の倍率 R（§5.3）。
pub(crate) const ROBUST_RATIO: f64 = 4.0;

/// コストの区間（µs）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CostRange {
    pub lo: f64,
    pub mid: f64,
    pub hi: f64,
}

impl CostRange {
    fn of(a: f64, b: f64, c: f64) -> CostRange {
        let lo = a.min(b).min(c);
        let hi = a.max(b).max(c);
        CostRange { lo, mid: b, hi }
    }

    /// 最悪側（推定が外れたときの悪い側）。
    pub(crate) fn worst(self) -> f64 {
        self.hi
    }
}

/// 選んだ戦略。
pub(crate) enum Pick<'q> {
    /// ソート駆動（予算付き・切替先の案と、予算の換算に使うその予測コスト）。
    Sort { alt: Option<(CandPlan<'q>, f64)> },
    /// 候補駆動（上限 = 推定の上端の 2 倍）。
    Cand { plan: CandPlan<'q>, cap: i64 },
}

/// O2 の計画。
pub(crate) struct Planned<'q> {
    pub pick: Pick<'q>,
    /// 選んだ戦略の予測コスト（µs・中央値）。
    pub predicted_us: f64,
    /// 推定に「推測」を含まないか（受付制御で拒否してよいか）。
    pub confident: bool,
}

/// 頑健な選択（§5.3）: 中央値のコストが最小の案の最悪コストが、全案の最悪コストの最小値の
/// R 倍を超えるなら、最悪コストが最小の案を選ぶ。
fn robust_pick(costed: &[Costed<'_>]) -> usize {
    let best_mid = costed
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.cost.mid.total_cmp(&b.1.cost.mid))
        .map_or(0, |(i, _)| i);
    let best_worst = costed
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.cost.worst().total_cmp(&b.1.cost.worst()))
        .map_or(0, |(i, _)| i);
    if costed[best_mid].cost.worst() > ROBUST_RATIO * costed[best_worst].cost.worst() {
        best_worst
    } else {
        best_mid
    }
}

/// 1 案の見積もり。
struct Costed<'q> {
    plan: Option<CandPlan<'q>>,
    cost: CostRange,
    conf: Conf,
    /// 候補件数の上端・中央（候補駆動のみ）。
    cand_hi: f64,
    cand_mid: f64,
}

impl Exec<'_, '_> {
    /// 統計を読み込む（O2）。統計が無い・数え直し前なら O1 のまま。値頻度表は計画に要る (列, 値) だけ。
    pub(crate) async fn load_stats(&mut self, cache: &StatsCache) -> Result<(), DataError> {
        let stats = super::catalog::load(self.conn, cache, self.keys).await?;
        if let Some(s) = &stats {
            let drivers = self.guard.drivers().unwrap_or_default();
            let wants = Estimator::wanted(s, self.low.q, &drivers);
            self.counts = super::catalog::value_counts(self.conn, self.keys, &wants).await?;
        }
        self.stats.planned_with_stats = stats.is_some();
        self.catalog = stats;
        Ok(())
    }

    fn estimator<'s>(&'s self, stats: &'s TableStats) -> Estimator<'s> {
        Estimator {
            stats,
            counts: &self.counts,
            q: self.low.q,
        }
    }

    /// 入口の推定（推測なら件数探索で狭める）。
    #[allow(clippy::many_single_char_names)] // 推定（e）・原子式（a）・件数（n）・案（p）の慣用名。
    async fn entrance(&mut self, stats: &TableStats, p: &CandPlan<'_>) -> Result<Est, DataError> {
        let n = stats.n_rows;
        let est = match &p.src {
            Source::Atom(a) => {
                let e = self.estimator(stats).atom(a);
                if e.conf == Conf::Guess {
                    let low = self.low;
                    match self
                        .prober
                        .atom(self.conn, low, a, PROBE_CAP, false)
                        .await?
                    {
                        Some(r) => Est::probed(r.count, r.cap, n),
                        None => e,
                    }
                } else {
                    e
                }
            }
            Source::Union(atoms) => {
                let mut parts = Vec::with_capacity(atoms.len());
                for a in atoms {
                    let e = self.estimator(stats).atom(a);
                    let e = if e.conf == Conf::Guess {
                        let low = self.low;
                        match self
                            .prober
                            .atom(self.conn, low, a, PROBE_CAP, false)
                            .await?
                        {
                            Some(r) => Est::probed(r.count, r.cap, n),
                            None => e,
                        }
                    } else {
                        e
                    };
                    parts.push(e);
                }
                Est::or(&parts)
            }
            Source::Intersect(a, b) => {
                let ea = self.estimator(stats).atom(a);
                let eb = self.estimator(stats).atom(b);
                Est::and(&[ea, eb])
            }
            Source::Policy(_) => {
                let drivers = self.guard.drivers().unwrap_or_default();
                let mut parts = Vec::with_capacity(drivers.len());
                for d in &drivers {
                    let e = self.estimator(stats).driver(d);
                    // 行述語の選言の件数探索はキャッシュしない（材料由来・PIT-18）。
                    let e = if e.conf == Conf::Guess {
                        match self.prober.driver(self.conn, d, PROBE_CAP).await? {
                            Some(r) => Est::probed(r.count, r.cap, n),
                            None => e,
                        }
                    } else {
                        e
                    };
                    parts.push(e);
                }
                Est::or(&parts)
            }
            Source::Window { .. } | Source::All | Source::Ids(_) => Est::exact(1.0),
        };
        Ok(est)
    }

    /// 入口が読む索引エントリ数（交差は両方の範囲を読む・多値は要素ごとに行がある）。
    #[allow(clippy::many_single_char_names)] // 推定（e）・原子式（a, b）・件数（n）・案（p）の慣用名。
    fn entries(&self, stats: &TableStats, p: &CandPlan<'_>, e: f64) -> f64 {
        let n = stats.n_rows;
        let elems = |a: &crate::query::bound::Atom| {
            let f = self.low.q.field(a.field);
            stats
                .field(a.field, f.value_table())
                .map_or(1.0, |s| s.avg_elems.max(1.0))
        };
        match &p.src {
            Source::Intersect(a, b) => {
                let est = self.estimator(stats);
                n * (est.atom(a).mid * elems(a) + est.atom(b).mid * elems(b)).max(e)
            }
            Source::Atom(a) => n * e * elems(a),
            _ => n * e,
        }
    }

    /// 行述語の選択率（全行可視なら 1）。
    fn guard_est(&self, stats: &TableStats) -> Est {
        if self.guard.is_unrestricted() {
            return Est::exact(1.0);
        }
        match self.guard.drivers() {
            Some(ds) => {
                let est = self.estimator(stats);
                Est::or(&ds.iter().map(|d| est.driver(d)).collect::<Vec<_>>())
            }
            // 索引で引けない行述語（`ne` 等）: 推測。
            None => Est {
                lo: 0.01,
                mid: 0.5,
                hi: 1.0,
                conf: Conf::Guess,
            },
        }
    }

    /// ソート駆動の見積もり（予想走査件数 = L / s に補正係数を掛ける・区間の両端で）。
    fn sort_costed(
        &mut self,
        stats: &TableStats,
        sort: &[BSort],
        limit: i64,
        all: &[Est],
    ) -> Costed<'static> {
        let u = self.units;
        #[allow(clippy::cast_precision_loss)]
        let l = (limit + 1) as f64;
        let total = Est::and(all);
        let walk_cap = stats.n_rows.max(1.0) * 2.0;
        let corr = self.correction.max(1e-3);
        let walk = |s: f64| (l / s.max(1.0 / walk_cap)).min(walk_cap) * corr;
        let rates: Vec<f64> = all.iter().map(|e| e.mid).collect();
        let mut cost = CostRange::of(
            u.sort_drive(walk(total.hi), &rates, l),
            u.sort_drive(walk(total.mid), &rates, l),
            u.sort_drive(walk(total.lo), &rates, l),
        );
        // 複数ソートで複合索引が無い: 第 1 キーの同値群を丸ごと先読みする分を足す（§5.2）。
        if sort.len() > 1 && self.composite.is_none() {
            let key = sort[0];
            let table = self.low.q.field(key.field).value_table();
            let group = stats
                .field(key.field, table)
                .and_then(|f| f.mcv.first().map(|(_, fr)| fr * f.n_values))
                .unwrap_or(0.0);
            #[allow(clippy::cast_precision_loss)]
            if group > (limit * K_PREFETCH) as f64 {
                let extra = group * (u.c_tuple + u.probe(&rates) + u.c_probe);
                cost.lo += extra;
                cost.mid += extra;
                cost.hi += extra;
            }
        }
        // 補正係数の更新は「補正前の予想」と観測を比べる。
        self.sort_walk_pred = Some((l / total.mid.max(1.0 / walk_cap)).min(walk_cap));
        Costed {
            plan: None,
            cost,
            conf: total.conf,
            cand_hi: 0.0,
            cand_mid: 0.0,
        }
    }

    /// O2 の計画（統計がある前提）。
    pub(super) async fn plan_o2<'q>(
        &mut self,
        stats: &Arc<TableStats>,
        conj: &[&'q Expr],
        sort: &[BSort],
        limit: i64,
    ) -> Result<Planned<'q>, DataError> {
        let u = self.units;
        #[allow(clippy::cast_precision_loss)]
        let l = (limit + 1) as f64;
        let n = stats.n_rows.max(1.0);
        let est = self.estimator(stats);
        let conj_est: Vec<Est> = conj.iter().map(|e| est.expr(e)).collect();
        let guard = self.guard_est(stats);
        let mut all = conj_est.clone();
        all.push(guard);
        let mut costed = vec![self.sort_costed(stats, sort, limit, &all)];
        let mut plans = all_cand_plans(conj);
        if let Some(p) = self.policy_plan(false).await? {
            plans.push(p);
        }
        for p in plans {
            let e = self.entrance(stats, &p).await?;
            let rest: Vec<f64> = conj
                .iter()
                .enumerate()
                .filter(|(i, _)| !p.covered.contains(i))
                .map(|(i, _)| conj_est[i].mid)
                .chain(std::iter::once(if p.name == "policy" {
                    1.0
                } else {
                    guard.mid
                }))
                .collect();
            let pass_given = Est::and(
                &conj
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !p.covered.contains(i))
                    .map(|(i, _)| conj_est[i])
                    .chain(std::iter::once(guard))
                    .collect::<Vec<_>>(),
            );
            let at = |s: f64| {
                let cand = n * s;
                u.cand_drive(
                    self.entries(stats, &p, s),
                    cand,
                    &rest,
                    cand * pass_given.mid,
                    sort.len(),
                    l,
                )
            };
            costed.push(Costed {
                cost: CostRange::of(at(e.lo), at(e.mid), at(e.hi)),
                conf: e.conf.max(pass_given.conf),
                cand_hi: n * e.hi,
                cand_mid: n * e.mid,
                plan: Some(p),
            });
        }
        let pick = robust_pick(&costed);
        let chosen = costed.swap_remove(pick);
        let predicted_us = chosen.cost.mid;
        let confident = chosen.conf != Conf::Guess;
        self.est_cand = chosen.plan.as_ref().map(|_| chosen.cand_mid);
        self.stats.first_strategy = chosen.plan.as_ref().map_or("sort", |p| p.name);
        let pick = match chosen.plan {
            None => {
                // 切替先: 残りの候補駆動で中央値のコストが最小のもの。
                let alt = costed
                    .into_iter()
                    .filter_map(|c| c.plan.map(|p| (p, c.cost.mid, c.cand_hi)))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(mut p, cost, hi)| {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let c = hi.ceil().max(1.0) as u64;
                        p.count = Some(c);
                        (p, cost)
                    });
                Pick::Sort { alt }
            }
            Some(plan) => {
                #[allow(clippy::cast_possible_truncation)]
                let cap = ((chosen.cand_hi * 2.0).ceil() as i64).max(PROBE_CAP);
                Pick::Cand { plan, cap }
            }
        };
        Ok(Planned {
            pick,
            predicted_us,
            confident,
        })
    }

    /// 件数・集計の入口（O2）: 候補駆動のうち入口のコストが最小のもの。全件より高ければ全件。
    pub(super) async fn scan_source_o2<'q>(
        &mut self,
        conj: &[&'q Expr],
    ) -> Result<(Source<'q>, Vec<usize>), DataError> {
        let Some(stats) = self.catalog.clone() else {
            return Ok((Source::All, Vec::new()));
        };
        let u = self.units;
        let n = stats.n_rows.max(1.0);
        let est = self.estimator(&stats);
        let conj_est: Vec<Est> = conj.iter().map(|e| est.expr(e)).collect();
        let guard = self.guard_est(&stats);
        let all_sels: Vec<f64> = conj_est.iter().map(|e| e.mid).chain([guard.mid]).collect();
        let full = n * (u.c_tuple + u.probe(&all_sels)) + u.c_rtt;
        let mut best: Option<(CandPlan<'q>, f64, Conf)> = None;
        let mut plans = all_cand_plans(conj);
        if let Some(p) = self.policy_plan(false).await? {
            plans.push(p);
        }
        for p in plans {
            let e = self.entrance(&stats, &p).await?;
            let rest: Vec<f64> = conj
                .iter()
                .enumerate()
                .filter(|(i, _)| !p.covered.contains(i))
                .map(|(i, _)| conj_est[i].mid)
                .chain([guard.mid])
                .collect();
            let cost = self.entries(&stats, &p, e.mid) * u.c_tuple + n * e.mid * u.probe(&rest);
            if best.as_ref().is_none_or(|b| cost < b.1) {
                best = Some((p, cost, e.conf));
            }
        }
        let total_conf = conj_est.iter().map(|e| e.conf).max().unwrap_or(Conf::Exact);
        let (src, covered, cost, trust) = match best {
            Some((p, c, trust)) if c < full => {
                self.stats.strategy = p.name;
                (p.src, p.covered, c, trust)
            }
            _ => {
                self.stats.strategy = "scan";
                (Source::All, Vec::new(), full, total_conf)
            }
        };
        self.admit(cost, trust != Conf::Guess, &[]).await?;
        Ok((src, covered))
    }

    /// 受付制御（予算超過の見込みは実行しない）と、予測コストのトークンの積み増し。
    pub(super) async fn admit(
        &mut self,
        predicted_us: f64,
        confident: bool,
        sort: &[BSort],
    ) -> Result<(), DataError> {
        self.predicted_us = Some(predicted_us);
        if confident && predicted_us > self.read_budget_us {
            self.stats.admission_rejected = true;
            let mut suggestions = vec![QuerySuggestion::NarrowFilter];
            if sort.len() > 1 && self.composite.is_none() {
                suggestions.push(QuerySuggestion::CompositeIndex);
            }
            return Err(DataError::QueryTooExpensive { suggestions });
        }
        if let Some(p) = self.permit.as_mut() {
            p.top_up(crate::query::admission::tokens_for(predicted_us))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(lo: f64, mid: f64, hi: f64) -> Costed<'static> {
        Costed {
            plan: None,
            cost: CostRange { lo, mid, hi },
            conf: Conf::Stats,
            cand_hi: 0.0,
            cand_mid: 0.0,
        }
    }

    #[test]
    fn robust_choice_avoids_plans_with_a_bad_worst_case() {
        // A は中央値で最安だが最悪が B の最悪の 4 倍を超える → B。
        let v = [c(1.0, 10.0, 1_000.0), c(50.0, 60.0, 200.0)];
        assert_eq!(robust_pick(&v), 1);
        // 4 倍以内なら中央値で最安の A。
        let v = [c(1.0, 10.0, 700.0), c(50.0, 60.0, 200.0)];
        assert_eq!(robust_pick(&v), 0);
        assert!((CostRange::of(3.0, 2.0, 1.0).worst() - 3.0).abs() < 1e-9);
    }
}
