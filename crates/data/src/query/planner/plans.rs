//! 候補集合の戦略の案（フィルタ駆動・OR 駆動・交差駆動・認可駆動）の列挙と件数探索
//! （query-optimizer.md §5.1・§4.3）。
//!
//! - **フィルタ駆動**: 条件の最上位の積のうち索引で引ける 1 項の範囲から入る。
//! - **OR 駆動**: 別の列どうしの OR（各項が正の葉）の和集合から入る。項が 3 つを超えたら Rust 側の
//!   Roaring ビットマップで合わせる（[`super::batch`]）。
//! - **交差駆動**（O3）: 等値（`Eq` / `In`）の範囲 2 つの交わりから入る（`INTERSECT`・`record_int` で突き合わせ）。
//! - **認可駆動**: 行述語の選言の和集合から入る。
//!
//! 入口で満たされる項（`covered`）は検査から外す。行述語の検査（Guard）は外さない（§7.1）。

use super::exec::{Exec, ForcedStrategy};
use super::lower::{order_checks, Source};
use super::stats::PROBE_CAP;
use crate::query::bound::{Atom, Expr, Leaf};
use crate::DataError;

/// 候補集合の戦略の 1 案。
#[derive(Debug, Clone)]
pub(crate) struct CandPlan<'q> {
    pub src: Source<'q>,
    /// 条件の最上位の項のうち、入口で満たされるもの（検査から外す）。
    pub covered: Vec<usize>,
    /// 件数探索で分かった候補件数（上限内のときだけ）。
    pub count: Option<u64>,
    pub name: &'static str,
}

/// 条件の最上位の積の項。
pub(crate) fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::True => Vec::new(),
        Expr::And(v) => v.iter().collect(),
        other => vec![other],
    }
}

/// 駆動にできる原子式（索引の範囲になる正の葉）。
pub(crate) fn drivable(e: &Expr) -> Option<&Atom> {
    match e {
        Expr::Atom(a) => Some(a),
        _ => None,
    }
}

fn atom_rank(a: &Atom) -> u8 {
    match a.leaf {
        Leaf::Eq(_) => 0,
        Leaf::In(_) => 1,
        Leaf::Range { .. } | Leaf::StartsWith(_) => 2,
        Leaf::Contains(_) | Leaf::EndsWith(_) => 3,
        Leaf::Exists => 4,
    }
}

/// 交差駆動に使える等値の葉か。
fn equality(a: &Atom) -> bool {
    matches!(a.leaf, Leaf::Eq(_) | Leaf::In(_))
}

/// O1 の案（フィルタ駆動は葉の種類で 1 つ・OR 駆動は最初の 1 つ）。
pub(crate) fn cand_plans<'q>(conj: &[&'q Expr]) -> Vec<CandPlan<'q>> {
    let mut out = Vec::new();
    if let Some((i, a)) = conj
        .iter()
        .enumerate()
        .filter_map(|(i, e)| drivable(e).map(|a| (i, a)))
        .min_by_key(|(_, a)| atom_rank(a))
    {
        out.push(CandPlan {
            src: Source::Atom(a),
            covered: vec![i],
            count: None,
            name: "filter",
        });
    }
    if let Some(p) = union_plans(conj).into_iter().next() {
        out.push(p);
    }
    out
}

/// OR 駆動の案（別の列どうしの OR で、各項が正の葉）。
pub(crate) fn union_plans<'q>(conj: &[&'q Expr]) -> Vec<CandPlan<'q>> {
    let mut out = Vec::new();
    for (i, e) in conj.iter().enumerate() {
        if let Expr::Or(terms) = e {
            let atoms: Option<Vec<&Atom>> = terms.iter().map(drivable).collect();
            if let Some(atoms) = atoms.filter(|v| v.len() >= 2) {
                out.push(CandPlan {
                    src: Source::Union(atoms),
                    covered: vec![i],
                    count: None,
                    name: "union",
                });
            }
        }
    }
    out
}

/// O2 の全案: 索引で引ける葉ごとのフィルタ駆動・OR 駆動・等値 2 つの交差駆動。
pub(crate) fn all_cand_plans<'q>(conj: &[&'q Expr]) -> Vec<CandPlan<'q>> {
    let mut out: Vec<CandPlan<'q>> = conj
        .iter()
        .enumerate()
        .filter_map(|(i, e)| drivable(e).map(|a| (i, a)))
        .map(|(i, a)| CandPlan {
            src: Source::Atom(a),
            covered: vec![i],
            count: None,
            name: "filter",
        })
        .collect();
    out.extend(union_plans(conj));
    out.extend(intersect_plans(conj));
    out
}

/// 交差駆動の案（別の列の等値の葉の組・項数が多くても組は最大 6 つ）。
pub(crate) fn intersect_plans<'q>(conj: &[&'q Expr]) -> Vec<CandPlan<'q>> {
    let eqs: Vec<(usize, &Atom)> = conj
        .iter()
        .enumerate()
        .filter_map(|(i, e)| drivable(e).map(|a| (i, a)))
        .filter(|(_, a)| equality(a))
        .take(4)
        .collect();
    let mut out = Vec::new();
    for (x, (i, a)) in eqs.iter().enumerate() {
        for (j, b) in eqs.iter().skip(x + 1) {
            if a.field == b.field {
                continue;
            }
            out.push(CandPlan {
                src: Source::Intersect(a, b),
                covered: vec![*i, *j],
                count: None,
                name: "intersect",
            });
        }
    }
    out
}

/// 検査に残す項（入口で満たされる項を除き、安くてよく落ちる順）。
pub(crate) fn checks_for<'q>(conj: &[&'q Expr], covered: &[usize]) -> Vec<&'q Expr> {
    order_checks(
        conj.iter()
            .enumerate()
            .filter(|(i, _)| !covered.contains(i))
            .map(|(_, e)| *e)
            .collect(),
    )
}

impl Exec<'_, '_> {
    /// 件数探索で候補戦略の件数を埋める（上限内のものだけが候補になる）。
    pub(super) async fn probe_plans(
        &mut self,
        plans: &mut [CandPlan<'_>],
    ) -> Result<(), DataError> {
        for p in plans.iter_mut() {
            p.count = match &p.src {
                Source::Atom(a) => {
                    let low = self.low;
                    let r = self
                        .prober
                        .atom(self.conn, low, a, PROBE_CAP, false)
                        .await?;
                    r.and_then(super::stats::Probe::exact)
                }
                Source::Union(atoms) => {
                    if atoms.len() as u32 > self.prober.remaining() + 3 {
                        None
                    } else {
                        let mut sum = Some(0u64);
                        for a in atoms {
                            let low = self.low;
                            let r = self
                                .prober
                                .atom(self.conn, low, a, PROBE_CAP, false)
                                .await?
                                .and_then(super::stats::Probe::exact);
                            sum = match (sum, r) {
                                (Some(s), Some(n)) => Some(s + n),
                                _ => None,
                            };
                            if sum.is_none() {
                                break;
                            }
                        }
                        sum
                    }
                }
                _ => None,
            };
        }
        Ok(())
    }

    /// 認可駆動の案（見える行がごく少ない利用者）。
    pub(super) async fn policy_plan(
        &mut self,
        probe: bool,
    ) -> Result<Option<CandPlan<'static>>, DataError> {
        if self.guard.is_unrestricted() {
            return Ok(None);
        }
        let Some(drivers) = self.guard.drivers() else {
            return Ok(None);
        };
        let mut count = Some(0u64);
        if probe {
            for d in &drivers {
                let r = self.prober.driver(self.conn, d, PROBE_CAP).await?;
                count = match (count, r.and_then(super::stats::Probe::exact)) {
                    (Some(s), Some(n)) => Some(s + n),
                    _ => None,
                };
                if count.is_none() {
                    break;
                }
            }
        } else {
            count = None;
        }
        // 入口の SQL は文ごとに組み直す（バインドの番号が文ごとに違うため・[`Exec::materialize`]）。
        Ok(Some(CandPlan {
            src: Source::Policy(Vec::new()),
            covered: Vec::new(),
            count,
            name: "policy",
        }))
    }

    /// 強制された候補戦略の案（適用できなければ `None` = 自然な選択へ戻す）。
    pub(super) async fn forced_plan<'q>(
        &mut self,
        f: ForcedStrategy,
        conj: &[&'q Expr],
    ) -> Result<Option<CandPlan<'q>>, DataError> {
        Ok(match f {
            ForcedStrategy::Policy => self.policy_plan(false).await?,
            ForcedStrategy::Filter => cand_plans(conj).into_iter().find(|p| p.name == "filter"),
            ForcedStrategy::Union => union_plans(conj).into_iter().next(),
            ForcedStrategy::Intersect => intersect_plans(conj).into_iter().next(),
            ForcedStrategy::Sort | ForcedStrategy::Safe | ForcedStrategy::Batch => None,
        })
    }

    /// 件数・集計の入口（候補戦略のうち件数が上限内で最小のもの。無ければ全レコード）。
    pub(super) async fn scan_source<'q>(
        &mut self,
        conj: &[&'q Expr],
    ) -> Result<(Source<'q>, Vec<usize>), DataError> {
        if let Some(f) = self.force {
            if let Some(p) = self.forced_plan(f, conj).await? {
                self.stats.strategy = p.name;
                return Ok((p.src, p.covered));
            }
            if matches!(
                f,
                ForcedStrategy::Sort | ForcedStrategy::Safe | ForcedStrategy::Batch
            ) {
                return Ok((Source::All, Vec::new()));
            }
        }
        if self.catalog.is_some() {
            return self.scan_source_o2(conj).await;
        }
        let mut plans = cand_plans(conj);
        self.probe_plans(&mut plans).await?;
        if let Some(p) = self.policy_plan(true).await? {
            plans.push(p);
        }
        let best = plans
            .into_iter()
            .filter(|p| p.count.is_some())
            .min_by_key(|p| p.count.unwrap_or(u64::MAX));
        Ok(match best {
            None => {
                self.stats.strategy = "scan";
                (Source::All, Vec::new())
            }
            Some(p) => {
                self.stats.strategy = p.name;
                (p.src, p.covered)
            }
        })
    }
}
