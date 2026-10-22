//! 物理計画と予算付き実行（query-optimizer.md §4.3・§5・§6.2・O1）。
//!
//! 戦略は 4 つ: **ソート駆動**（任意列は 2 区間）・**フィルタ駆動**・**認可駆動**・**OR 駆動**。
//! O1 は統計表を持たず、上限付き件数探索（[`super::stats`]）で「絞れているか」を数えて選ぶ。
//!
//! ```text
//! 1. 候補集合の戦略（フィルタ/認可/OR 駆動）の件数 c を探索する（上限 1,000・1 計画 3 本まで）
//! 2. ソート駆動を走査予算 B で実行する。c が分かれば B = 候補戦略の実コスト ÷ ソート駆動の 1 エントリ単価、
//!    分からなければ B = L × K（K = 8）から倍々
//! 3. 予算切れ: 観測した選択率でソート駆動の残りコストを見積もり、候補戦略の方が安ければ切り替える
//!    （途中結果は捨てる・1 リクエスト 1 回まで）。まだソート駆動が安ければ予算を 2 倍にして続ける
//!    （同じ全順序の上を先へ進むだけなので途中結果は保持してよい）
//! 4. 切替先も上限付きで実行し、超えたら安全計画（予算なしのソート駆動・全述語検査）に落とす
//! ```
//!
//! どの戦略も同じ全順序（[`super::keys`]）の上で「位置より後ろの先頭 L 件」を返すので、
//! 結果の集合と順序は戦略に依存しない（コストモデルは速さにだけ効く・PIT-76）。

use sqlx::{PgConnection, Row};

use super::feedback::QueryExecStats;
use super::guard::{Guard, GuardSql};
use super::keys::{cmp_pos, Pos, SortVal};
use super::lower::{order_checks, Lowerer, Source};
use super::stats::{Prober, PROBE_CAP};
use crate::query::bound::{Atom, BSort, Expr, Leaf};
use crate::query::sql::{bind_query, Bind, SqlBuf};
use crate::{map_db, DataError};

/// 索引タプル 1 件の走査（µs・§5.2 の初期値）。
const C_TUPLE: f64 = 1.0;
/// 点検索 1 回。
const C_PROBE: f64 = 5.0;
/// 本体 1 行の取得。
const C_FETCH: f64 = 10.0;
/// 複数ソートの同値群の先読み倍率・予算の初期倍率（K）。
pub(crate) const K_PREFETCH: i64 = 8;
/// 予算切れのたびに件数を測り直す回数の上限。
const MAX_REPROBES: u32 = 8;

/// 強制する戦略（検証用・計画の形の回帰と全戦略一致テスト）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForcedStrategy {
    /// ソート駆動（予算なし・切替なし）。
    Sort,
    /// フィルタ駆動（条件の最初の索引可能な項）。
    Filter,
    /// 認可駆動（行述語の選言の和集合）。
    Policy,
    /// OR 駆動（別の列どうしの OR の和集合）。
    Union,
    /// 安全計画（予算なしのソート駆動。`Sort` と同じ形）。
    Safe,
}

/// 件数探索器（実行入口が組んで渡す）。
pub(crate) type ProberRef<'a> = Prober<'a>;

/// 実行した文（デバッグ取得用・`record_int` を含み得るので crate 外の運用経路へは出さない）。
#[derive(Debug, Clone)]
pub(crate) struct Captured {
    pub label: &'static str,
    pub sql: String,
    pub binds: Vec<Bind>,
}

/// 実行の文脈（1 リクエスト）。
pub(crate) struct Exec<'a, 'c> {
    pub conn: &'c mut PgConnection,
    pub low: &'a Lowerer<'a>,
    pub guard: &'a Guard<'a>,
    pub keys: crate::model::TableKeys,
    pub prober: Prober<'a>,
    pub stats: QueryExecStats,
    pub captured: Vec<Captured>,
    pub force: Option<ForcedStrategy>,
    pub budget_override: Option<i64>,
}

/// 1 ページの結果（全順序の位置つき）。
#[derive(Debug, Default)]
pub(crate) struct PageOut {
    pub rows: Vec<(i64, Pos)>,
    pub next: Option<Pos>,
}

/// 候補集合の戦略の 1 案。
#[derive(Debug, Clone)]
struct CandPlan<'q> {
    src: Source<'q>,
    /// 条件の最上位の項のうち、入口で満たされるもの（検査から外す）。
    covered: Option<usize>,
    count: Option<u64>,
    name: &'static str,
}

/// ソート駆動の 1 回分の走査結果。
enum Step {
    /// 予算内で `need` 件そろった、または索引の終端。
    Done,
    /// 予算切れ（観測: 走査件数・通過件数）。
    Exhausted { scanned: u64, passed: u64 },
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
fn drivable(e: &Expr) -> Option<&Atom> {
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

impl<'a> Exec<'a, '_> {
    fn sql(&self) -> SqlBuf {
        SqlBuf::scoped(self.keys.tenant_int, self.keys.table_int)
    }

    async fn fetch(
        &mut self,
        label: &'static str,
        text: String,
        binds: Vec<Bind>,
    ) -> Result<Vec<sqlx::postgres::PgRow>, DataError> {
        self.stats.statements += 1;
        let rows = bind_query(sqlx::query(&text), &binds)
            .fetch_all(&mut *self.conn)
            .await
            .map_err(map_db)?;
        self.captured.push(Captured {
            label,
            sql: text,
            binds,
        });
        Ok(rows)
    }

    /// 候補集合の戦略の案（入口と、検査から外せる項）。
    fn cand_plans<'q>(conj: &[&'q Expr]) -> Vec<CandPlan<'q>> {
        let mut out = Vec::new();
        // フィルタ駆動: 最も絞れていそうな正の葉（O1 は葉の種類で順位づける）。
        if let Some((i, a)) = conj
            .iter()
            .enumerate()
            .filter_map(|(i, e)| drivable(e).map(|a| (i, a)))
            .min_by_key(|(_, a)| atom_rank(a))
        {
            out.push(CandPlan {
                src: Source::Atom(a),
                covered: Some(i),
                count: None,
                name: "filter",
            });
        }
        // OR 駆動: 別の列どうしの OR（各項が正の葉）。
        for (i, e) in conj.iter().enumerate() {
            if let Expr::Or(terms) = e {
                let atoms: Option<Vec<&Atom>> = terms.iter().map(drivable).collect();
                if let Some(atoms) = atoms.filter(|v| v.len() >= 2) {
                    out.push(CandPlan {
                        src: Source::Union(atoms),
                        covered: Some(i),
                        count: None,
                        name: "union",
                    });
                    break;
                }
            }
        }
        out
    }

    /// 件数探索で候補戦略の件数を埋める（上限内のものだけが候補になる）。
    async fn probe_plans(&mut self, plans: &mut [CandPlan<'_>]) -> Result<(), DataError> {
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
    async fn policy_plan(&mut self, probe: bool) -> Result<Option<CandPlan<'static>>, DataError> {
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
        // 入口の SQL は文ごとに組み直す（バインドの番号が文ごとに違うため・[`Self::materialize`]）。
        Ok(Some(CandPlan {
            src: Source::Policy(Vec::new()),
            covered: None,
            count,
            name: "policy",
        }))
    }

    fn checks_for<'q>(conj: &[&'q Expr], covered: Option<usize>) -> Vec<&'q Expr> {
        order_checks(
            conj.iter()
                .enumerate()
                .filter(|(i, _)| Some(*i) != covered)
                .map(|(_, e)| *e)
                .collect(),
        )
    }

    /// ページを取る（戦略選択 → 予算付き実行 → 切替）。
    pub(crate) async fn page(
        &mut self,
        filter: &'a Expr,
        sort: &[BSort],
        after: Option<&Pos>,
        limit: i64,
    ) -> Result<PageOut, DataError> {
        if *filter == Expr::False || self.guard.is_empty() {
            self.stats.strategy = "empty";
            return Ok(PageOut::default());
        }
        let conj = conjuncts(filter);
        let mut plans = Self::cand_plans(&conj);
        match self.force {
            Some(ForcedStrategy::Sort | ForcedStrategy::Safe) => {
                return self.sort_drive(&conj, sort, after, limit, i64::MAX).await;
            }
            Some(f @ (ForcedStrategy::Filter | ForcedStrategy::Union | ForcedStrategy::Policy)) => {
                let want = match f {
                    ForcedStrategy::Filter => "filter",
                    ForcedStrategy::Union => "union",
                    _ => "policy",
                };
                let plan = if want == "policy" {
                    self.policy_plan(false).await?
                } else {
                    plans.into_iter().find(|p| p.name == want)
                };
                if let Some(p) = plan {
                    return self
                        .candidates(&p, &conj, after, limit, None)
                        .await
                        .map(Option::unwrap_or_default);
                }
                // 適用できない戦略の強制は自然な選択に戻す。
                plans = Self::cand_plans(&conj);
            }
            None => {}
        }
        self.probe_plans(&mut plans).await?;
        if let Some(p) = self.policy_plan(true).await? {
            plans.push(p);
        }
        let n_checks = conj.len() as f64 + 1.0;
        let best = plans
            .into_iter()
            .filter(|p| p.count.is_some())
            .min_by_key(|p| p.count.unwrap_or(u64::MAX));
        let unit_a = C_TUPLE + n_checks * C_PROBE;
        let cost_of = |c: u64| -> f64 {
            let c = c as f64;
            c * (C_TUPLE + n_checks * C_PROBE)
                + c * sort.len() as f64 * C_PROBE
                + c * (limit as f64).max(2.0).log2() * 0.05
                + limit as f64 * C_FETCH
        };
        let mut budget = match (&self.budget_override, &best) {
            (Some(b), _) => *b,
            (None, Some(p)) => (cost_of(p.count.unwrap_or(0)) / unit_a).ceil() as i64,
            (None, None) => limit * K_PREFETCH,
        }
        .max(1);
        self.adaptive(
            &conj,
            sort,
            after,
            limit,
            &mut budget,
            best,
            unit_a,
            &cost_of,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn adaptive(
        &mut self,
        conj: &[&'a Expr],
        sort: &[BSort],
        after: Option<&Pos>,
        limit: i64,
        budget: &mut i64,
        mut best: Option<CandPlan<'a>>,
        unit_a: f64,
        cost_of: &(dyn Fn(u64) -> f64 + Sync),
    ) -> Result<PageOut, DataError> {
        let mut st = SortState::new(after);
        let mut scanned_total = 0u64;
        let mut passed_total = 0u64;
        let mut reprobes = 0;
        loop {
            match self.sort_step(conj, sort, &mut st, limit, *budget).await? {
                Step::Done => {
                    self.stats.strategy = "sort";
                    return Ok(st.finish(sort, limit));
                }
                Step::Exhausted { scanned, passed } => {
                    scanned_total += scanned;
                    passed_total += passed;
                    // c が分からない間は、走査件数の 10 倍まで上限を広げて測り直す（§6.2 O1）。
                    if best.is_none() && reprobes < MAX_REPROBES {
                        reprobes += 1;
                        let cap = (scanned_total as i64 * 10).max(PROBE_CAP);
                        if let Some(mut p) = Self::cand_plans(conj).into_iter().next() {
                            if let Source::Atom(a) = &p.src {
                                let low = self.low;
                                let r = self.prober.atom(self.conn, low, a, cap, true).await?;
                                p.count = r.and_then(super::stats::Probe::exact);
                                if p.count.is_some() {
                                    best = Some(p);
                                }
                            }
                        }
                    }
                    let need = (limit + 1 - st.collected.len() as i64).max(1) as f64;
                    let sel = (passed_total as f64 / scanned_total.max(1) as f64)
                        .max(1.0 / scanned_total.max(1) as f64);
                    let est_a = need / sel * unit_a;
                    if let Some(p) = best.as_ref().filter(|p| {
                        !self.stats.switched && cost_of(p.count.unwrap_or(u64::MAX)) < est_a
                    }) {
                        // 別戦略へ切替: ソート駆動の途中結果は捨てる（混ぜない・PIT-76）。
                        self.stats.switched = true;
                        let cap = (p.count.unwrap_or(0) as i64 * 2).max(PROBE_CAP);
                        let p = p.clone();
                        if let Some(out) =
                            self.candidates(&p, conj, after, limit, Some(cap)).await?
                        {
                            return Ok(out);
                        }
                        // 切替先も上限を超えた: 安全計画。
                        self.stats.safe_fallback = true;
                        return self.sort_drive(conj, sort, after, limit, i64::MAX).await;
                    }
                    // 同じ戦略の継続: 予算を倍にして途中結果を保持する。
                    self.stats.doublings += 1;
                    *budget = budget.saturating_mul(2);
                }
            }
        }
    }

    /// 予算なし（または固定予算）のソート駆動。
    pub(crate) async fn sort_drive(
        &mut self,
        conj: &[&'a Expr],
        sort: &[BSort],
        after: Option<&Pos>,
        limit: i64,
        budget: i64,
    ) -> Result<PageOut, DataError> {
        let mut st = SortState::new(after);
        loop {
            if let Step::Done = self.sort_step(conj, sort, &mut st, limit, budget).await? {
                self.stats.strategy = if self.stats.safe_fallback {
                    "safe"
                } else {
                    "sort"
                };
                return Ok(st.finish(sort, limit));
            }
        }
    }

    /// 候補集合の戦略を実行する。`cap` を超えたら `None`（呼び出し側が安全計画へ）。
    async fn candidates(
        &mut self,
        p: &CandPlan<'_>,
        conj: &[&Expr],
        after: Option<&Pos>,
        limit: i64,
        cap: Option<i64>,
    ) -> Result<Option<PageOut>, DataError> {
        let checks = Self::checks_for(conj, p.covered);
        let rows = self
            .run_candidates(&p.src, &checks, after, cap, limit + 1)
            .await?;
        let Some(rows) = rows else {
            return Ok(None);
        };
        self.stats.strategy = p.name;
        let mut st = SortState::new(after);
        st.collected = rows;
        Ok(Some(st.finish(&self.low.q.sort, limit)))
    }

    /// 件数・集計の入口（候補戦略のうち件数が上限内で最小のもの。無ければ全レコード）。
    async fn scan_source(
        &mut self,
        conj: &[&'a Expr],
    ) -> Result<(Source<'a>, Option<usize>), DataError> {
        let mut plans = Self::cand_plans(conj);
        if let Some(f) = self.force {
            let want = match f {
                ForcedStrategy::Filter => "filter",
                ForcedStrategy::Union => "union",
                ForcedStrategy::Policy => "policy",
                ForcedStrategy::Sort | ForcedStrategy::Safe => return Ok((Source::All, None)),
            };
            let p = if want == "policy" {
                self.policy_plan(false).await?
            } else {
                plans.iter().find(|p| p.name == want).cloned()
            };
            if let Some(p) = p {
                self.stats.strategy = p.name;
                return Ok((p.src, p.covered));
            }
        }
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
                (Source::All, None)
            }
            Some(p) => {
                self.stats.strategy = p.name;
                (p.src, p.covered)
            }
        })
    }

    /// 上限付き件数（`cap` に達したら `AtLeast(cap)`）。
    pub(crate) async fn count(
        &mut self,
        filter: &'a Expr,
        cap: u64,
    ) -> Result<crate::query::ir::CountResult, DataError> {
        use crate::query::ir::CountResult;
        if *filter == Expr::False || self.guard.is_empty() {
            return Ok(CountResult::Exact(0));
        }
        let conj = conjuncts(filter);
        let (src, covered) = self.scan_source(&conj).await?;
        let checks = Self::checks_for(&conj, covered);
        let mut sql = self.sql();
        let guard = self.guard.lower("c.record_int", &mut sql)?;
        let src = self.materialize(&src, &mut sql);
        let text = self
            .low
            .count_sql(&src, &checks, &guard, cap as i64, &mut sql);
        self.stats.count_issued = true;
        let rows = self.fetch("count", text, sql.binds).await?;
        let n: i64 = rows
            .first()
            .map(|r| r.try_get::<i64, _>(0))
            .transpose()
            .map_err(map_db)?
            .unwrap_or(0);
        let n = n.max(0) as u64;
        Ok(if n > cap {
            CountResult::AtLeast(cap)
        } else {
            CountResult::Exact(n)
        })
    }

    /// 集計の行（列: `grp_count`, `g0..`, `m0..`）。
    pub(crate) async fn aggregate(
        &mut self,
        filter: &'a Expr,
    ) -> Result<Vec<sqlx::postgres::PgRow>, DataError> {
        // 空でも全体集計は 1 行（count = 0）を返す形にそろえるため、SQL は常に発行する。
        let conj = conjuncts(filter);
        let (src, covered) = self.scan_source(&conj).await?;
        let checks = Self::checks_for(&conj, covered);
        let mut sql = self.sql();
        let guard = self.guard.lower("c.record_int", &mut sql)?;
        let src = self.materialize(&src, &mut sql);
        let text = self.low.aggregate_sql(&src, &checks, &guard, &mut sql);
        self.fetch("aggregate", text, sql.binds).await
    }

    /// 認可駆動の入口をこの文のバインド列の上で組む（それ以外はそのまま）。
    pub(crate) fn materialize<'q>(&self, s: &Source<'q>, sql: &mut SqlBuf) -> Source<'q> {
        match s {
            Source::Policy(_) => {
                let drivers = self.guard.drivers().unwrap_or_default();
                Source::Policy(drivers.iter().map(|d| d.source_sql(sql)).collect())
            }
            other => other.clone(),
        }
    }

    /// 候補 SQL を実行し、位置つきの行を返す（上限超過は `None`）。
    pub(crate) async fn run_candidates(
        &mut self,
        src: &Source<'_>,
        checks: &[&Expr],
        after: Option<&Pos>,
        cap: Option<i64>,
        limit: i64,
    ) -> Result<Option<Vec<(i64, Pos)>>, DataError> {
        let mut sql = self.sql();
        let guard = self.guard.lower("c.record_int", &mut sql)?;
        let src = self.materialize(src, &mut sql);
        let text = self
            .low
            .candidates_sql(&src, checks, &guard, after, cap, limit, &mut sql);
        let rows = self.fetch("candidates", text, sql.binds).await?;
        let n_cand: i64 = rows
            .first()
            .map(|r| r.try_get::<i64, _>("n_cand"))
            .transpose()
            .map_err(map_db)?
            .unwrap_or(0);
        self.stats.scanned += n_cand.max(0) as u64;
        if cap.is_some_and(|c| n_cand > c) {
            return Ok(None);
        }
        let sort = &self.low.q.sort;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let Some(rint) = r.try_get::<Option<i64>, _>("record_int").map_err(map_db)? else {
                continue;
            };
            let mut vals = Vec::with_capacity(sort.len());
            for (i, k) in sort.iter().enumerate() {
                let v: Option<String> = r.try_get(format!("k{i}").as_str()).map_err(map_db)?;
                vals.push(SortVal::from_db(self.low.q.field(k.field).kind, v));
            }
            out.push((rint, Pos { vals, rint }));
        }
        Ok(Some(out))
    }

    /// ソート駆動を 1 回（予算 1 つ分）進める。
    async fn sort_step(
        &mut self,
        conj: &[&'a Expr],
        sort: &[BSort],
        st: &mut SortState,
        limit: i64,
        budget: i64,
    ) -> Result<Step, DataError> {
        let checks = order_checks(conj.to_vec());
        let key = sort[0];
        let need = limit + 1 - st.collected.len() as i64;
        if need <= 0 || st.done {
            st.done = true;
            return Ok(Step::Done);
        }
        if st.segment == 2 {
            return self.missing_step(&checks, sort, st, limit, budget).await;
        }
        let kind = self.low.q.field(key.field).kind;
        let mut sql = self.sql();
        let guard: GuardSql = self.guard.lower("d.record_int", &mut sql)?;
        let multi = sort.len() > 1;
        // 単一キー: (値, record_int) の厳密に後ろから。複数ソート: 同値群の先頭から（窓は候補 SQL が並べる）。
        let start = st.scan_from.as_ref().map(|(v, r)| (v, *r));
        let text = self
            .low
            .sort_drive_sql(key, start, &checks, &guard, budget, need + 1, &mut sql);
        let rows = self.fetch("sort_drive", text, sql.binds).await?;
        let (scanned, passed, end, last) = Self::absorb(&rows, kind, multi, st, budget)?;
        self.stats.scanned += scanned;
        if multi {
            if let Some(v_end) = last.clone() {
                // 窓 [開始, v_end] の中を全キーで並べる（同値群は必ず窓に丸ごと入る）。
                let lo = st.window_lo.clone();
                let (lo_b, hi_b) = if key.desc {
                    (Some((v_end.clone(), true)), lo)
                } else {
                    (lo, Some((v_end.clone(), true)))
                };
                let src = Source::Window {
                    key,
                    lo: lo_b,
                    hi: hi_b,
                };
                let after = st.after.clone();
                if let Some(rows) = self
                    .run_candidates(&src, &checks, after.as_ref(), None, need)
                    .await?
                {
                    st.collected.extend(rows);
                }
                st.window_lo = Some((v_end.clone(), false));
                st.scan_from = Some((v_end, Some(if key.desc { i64::MIN } else { i64::MAX })));
            }
        }
        if st.collected.len() as i64 > limit {
            st.done = true;
            return Ok(Step::Done);
        }
        if end {
            // 区間 1 の終端: 値なしの行（区間 2）へ。
            st.segment = 2;
            st.scan_rint = None;
            return self.missing_step(&checks, sort, st, limit, budget).await;
        }
        Ok(Step::Exhausted { scanned, passed })
    }

    /// 走査結果を状態へ取り込む。返り値: (走査件数, 通過件数, 索引の終端か, 最後に読んだ第 1 キー)。
    fn absorb(
        rows: &[sqlx::postgres::PgRow],
        kind: crate::query::bound::FieldKind,
        multi: bool,
        st: &mut SortState,
        budget: i64,
    ) -> Result<Absorbed, DataError> {
        let mut last_pos = 0i64;
        let mut last_is_end = false;
        let mut passed = 0u64;
        let mut last_val = None;
        for r in rows {
            let rint: i64 = r.try_get("record_int").map_err(map_db)?;
            let v: Option<String> = r.try_get("v").map_err(map_db)?;
            let pos: i64 = r.try_get("pos").map_err(map_db)?;
            let pass: bool = r.try_get("pass").map_err(map_db)?;
            let is_last: bool = r.try_get("is_last").map_err(map_db)?;
            let sv = SortVal::from_db(kind, v);
            last_pos = pos;
            last_is_end = is_last;
            last_val = Some(sv.clone());
            if st.segment == 1 {
                st.scan_from = Some((sv.clone(), Some(rint)));
            } else {
                st.scan_rint = Some(rint);
            }
            if pass {
                passed += 1;
                if !multi {
                    st.collected.push((
                        rint,
                        Pos {
                            vals: vec![sv],
                            rint,
                        },
                    ));
                }
            }
        }
        // 0 行 = 位置の後ろに索引行が無い（終端）。最後の行が is_last で pos < 予算なら終端。
        let end = rows.is_empty() || (last_is_end && last_pos < budget);
        let scanned = last_pos.max(0) as u64;
        Ok((scanned, passed, end, last_val))
    }

    /// 区間 2（第 1 キーの値なし）。単一キーは擬似列を歩き、複数ソートは同値群として候補 SQL で並べる。
    async fn missing_step(
        &mut self,
        checks: &[&Expr],
        sort: &[BSort],
        st: &mut SortState,
        limit: i64,
        budget: i64,
    ) -> Result<Step, DataError> {
        let key = sort[0];
        let need = limit + 1 - st.collected.len() as i64;
        if sort.len() > 1 {
            let missing = Expr::Not(Box::new(Expr::atom(key.field, Leaf::Exists)));
            let mut all: Vec<&Expr> = vec![&missing];
            all.extend_from_slice(checks);
            let after = st.after.clone();
            if let Some(rows) = self
                .run_candidates(&Source::All, &all, after.as_ref(), None, need)
                .await?
            {
                st.collected.extend(rows);
            }
            st.done = true;
            return Ok(Step::Done);
        }
        let mut sql = self.sql();
        let guard = self.guard.lower("d.record_int", &mut sql)?;
        let text = self.low.missing_drive_sql(
            key,
            st.scan_rint,
            checks,
            &guard,
            budget,
            need + 1,
            &mut sql,
        );
        let rows = self.fetch("missing_drive", text, sql.binds).await?;
        let kind = self.low.q.field(key.field).kind;
        let (scanned, passed, end, _) = Self::absorb(&rows, kind, false, st, budget)?;
        self.stats.scanned += scanned;
        if st.collected.len() as i64 > limit || end {
            st.done = true;
            return Ok(Step::Done);
        }
        Ok(Step::Exhausted { scanned, passed })
    }
}

/// 走査結果の要約（走査件数, 通過件数, 索引の終端か, 最後に読んだ第 1 キー）。
type Absorbed = (u64, u64, bool, Option<SortVal>);

/// ソート駆動の継続状態（同じ戦略を続ける間だけ途中結果を保持する）。
struct SortState {
    after: Option<Pos>,
    segment: u8,
    /// 区間 1 の走査位置（値, record_int）。`record_int` が `None` なら値の同値群の先頭から。
    scan_from: Option<(SortVal, Option<i64>)>,
    /// 区間 2 の走査位置。
    scan_rint: Option<i64>,
    /// 複数ソートの窓の開始（値, 含むか）。
    window_lo: Option<(SortVal, bool)>,
    collected: Vec<(i64, Pos)>,
    done: bool,
}

impl SortState {
    fn new(after: Option<&Pos>) -> Self {
        let mut st = SortState {
            after: after.cloned(),
            segment: 1,
            scan_from: None,
            scan_rint: None,
            window_lo: None,
            collected: Vec::new(),
            done: false,
        };
        if let Some(p) = after {
            if p.segment() == 2 {
                st.segment = 2;
                st.scan_rint = Some(p.rint);
            } else if let Some(v) = p.vals.first() {
                if p.vals.len() > 1 {
                    // 複数ソート: カーソルの同値群の先頭から窓を開く。
                    st.scan_from = Some((v.clone(), None));
                    st.window_lo = Some((v.clone(), true));
                } else {
                    st.scan_from = Some((v.clone(), Some(p.rint)));
                }
            }
        }
        st
    }

    /// 全順序で整列し、先頭 `limit` 件と次の位置を返す。
    fn finish(mut self, sort: &[BSort], limit: i64) -> PageOut {
        self.collected.sort_by(|a, b| cmp_pos(sort, &a.1, &b.1));
        self.collected.dedup_by_key(|r| r.0);
        if let Some(a) = &self.after {
            self.collected
                .retain(|r| cmp_pos(sort, &r.1, a) == std::cmp::Ordering::Greater);
        }
        let more = self.collected.len() as i64 > limit;
        self.collected.truncate(limit.max(0) as usize);
        let next = if more {
            self.collected.last().map(|r| r.1.clone())
        } else {
            None
        };
        PageOut {
            rows: self.collected,
            next,
        }
    }
}
