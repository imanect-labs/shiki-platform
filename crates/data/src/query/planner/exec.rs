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

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::{PgConnection, Row};

use super::catalog::TableStats;
use super::cost::UnitCosts;
use super::feedback::QueryExecStats;
use super::guard::{Guard, GuardSql};
use super::keys::{cmp_pos, Pos, SortVal};
use super::lower::{Lowerer, Source};
use super::plans::{cand_plans, conjuncts, CandPlan};
use super::stats::{Prober, PROBE_CAP};
use super::strategy::Pick;
use crate::model::FieldId;
use crate::query::admission::ReadPermit;
use crate::query::bound::{BSort, Expr, Leaf};
use crate::query::sql::{bind_query, Bind, SqlBuf};
use crate::{map_db, DataError};

/// 複数ソートの同値群の先読み倍率・予算の初期倍率（K）。
pub(crate) const K_PREFETCH: i64 = 8;

/// active な複合索引（sort_set_id と、ソートキーごとの値の持ち方・向き）。
pub(crate) type CompositeSet = (i16, Vec<(crate::query::bound::FieldKind, bool)>);
/// 予算切れのたびに件数を測り直す回数の上限。
const MAX_REPROBES: u32 = 8;

/// 計画の変種の文の種類（プランキャッシュ・[`Variant`] の先頭）。
pub(crate) const V_CANDIDATES: u8 = 1;
pub(crate) const V_SORT_DRIVE: u8 = 2;
pub(crate) const V_MISSING_DRIVE: u8 = 3;
pub(crate) const V_COUNT: u8 = 4;
pub(crate) const V_AGGREGATE: u8 = 5;
pub(crate) const V_FETCH: u8 = 6;

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
    /// 交差駆動（O3: 別の列の等値 2 つの範囲の交わり）。
    Intersect,
    /// バッチ駆動（O3: 駆動から段ごとに `record_int = ANY($ids)` で一括検査し、Roaring で合わせる）。
    Batch,
}

impl ForcedStrategy {
    /// 環境変数・設定の名前から（`SHIKI_DATA_FORCE_STRATEGY`）。
    pub fn parse(s: &str) -> Option<ForcedStrategy> {
        Some(match s {
            "sort" => ForcedStrategy::Sort,
            "filter" => ForcedStrategy::Filter,
            "policy" => ForcedStrategy::Policy,
            "union" => ForcedStrategy::Union,
            "safe" => ForcedStrategy::Safe,
            "intersect" => ForcedStrategy::Intersect,
            "batch" => ForcedStrategy::Batch,
            _ => return None,
        })
    }
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
    /// このソートの組み合わせの複合索引（active のときだけ・(sort_set_id, 各キーの持ち方と向き)）。
    pub composite: Option<CompositeSet>,
    /// 第 1 キーの同値群が K × ページを超えた（複合索引の材料化を要求する・利用者へは返さない）。
    pub wants_composite: bool,
    /// 可視行で見て同値群が K × ページを超えた（複合索引の準備中は深いページで遅くなる・API で明示）。
    pub sort_degraded: bool,
    /// 直前の候補 SQL で検査を通った行数。
    pub last_pass: i64,
    /// テーブルの統計（O2。無ければ O1 の件数探索だけで選ぶ・固定戦略のテーブルは読まない）。
    pub catalog: Option<Arc<TableStats>>,
    /// 値頻度表から引いた件数（この計画の分だけ）。
    pub counts: HashMap<(FieldId, String), u64>,
    /// 単価（較正済み）。
    pub units: UnitCosts,
    /// ソート駆動の走査件数の補正係数（フィードバック・§8）。
    pub correction: f64,
    /// 読取予算（予測コスト µs の上限・受付制御）。
    pub read_budget_us: f64,
    /// テナントの同時実行の枠のトークン（計画後に予測コストまで積み増す）。
    pub permit: Option<ReadPermit>,
    /// 選んだ戦略の予測コスト（µs）。
    pub predicted_us: Option<f64>,
    /// ソート駆動の予想走査件数（補正前）と観測した走査件数（補正係数の更新）。
    pub sort_walk_pred: Option<f64>,
    pub sort_walk_obs: Option<f64>,
    /// 選んだ候補駆動の推定候補件数（推定外れの検出）。
    pub est_cand: Option<f64>,
    /// プランキャッシュ（同一形状の文は同じテキストを引く・data-platform §16.3）。
    pub plan: Option<super::plan_cache::PlanHandle<'a>>,
    /// 閲覧者が読める・昇格できる未索引の列がある（受付制御の提案「索引の昇格」を出す・§5.4）。
    pub promotable: bool,
    /// プランキャッシュの文脈キー（降下のテキストを変え得る索引の状態・認可駆動の入口の形・複合索引の有無。
    /// 値・認可材料は含まない・[`Variant::new`]）。
    pub ctx_key: [u8; 32],
}

/// 1 ページの結果（全順序の位置つき）。
#[derive(Debug, Default)]
pub(crate) struct PageOut {
    pub rows: Vec<(i64, Pos)>,
    pub next: Option<Pos>,
}

/// ソート駆動の 1 回分の走査結果。
pub(super) enum Step {
    /// 予算内で `need` 件そろった、または索引の終端。
    Done,
    /// 予算切れ（観測: 走査件数・通過件数）。
    Exhausted { scanned: u64, passed: u64 },
}

impl<'a> Exec<'a, '_> {
    pub(super) fn sql(&self) -> SqlBuf {
        SqlBuf::scoped(self.keys.tenant_int, self.keys.table_int)
    }

    pub(super) async fn fetch(
        &mut self,
        label: &'static str,
        text: String,
        binds: Vec<Bind>,
    ) -> Result<Vec<sqlx::postgres::PgRow>, DataError> {
        self.stats.statements += 1;
        let text = self.intern(label, text);
        let rows = bind_query(sqlx::query(&text), &binds)
            .fetch_all(&mut *self.conn)
            .await
            .map_err(map_db)?;
        self.captured.push(Captured {
            label,
            sql: text.to_string(),
            binds,
        });
        Ok(rows)
    }

    /// プランキャッシュからテキストを引く（同一形状なら同じ `Arc<str>`）。
    pub(crate) fn intern(&self, label: &'static str, text: String) -> Arc<str> {
        match &self.plan {
            Some(p) => p.intern(label, text),
            None => text.into(),
        }
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
        match self.force {
            Some(ForcedStrategy::Sort | ForcedStrategy::Safe) => {
                return self.sort_drive(&conj, sort, after, limit, i64::MAX).await;
            }
            Some(ForcedStrategy::Batch) if sort.len() == 1 => {
                return self.batch_drive(&conj, sort[0], after, limit).await;
            }
            Some(f) => {
                if let Some(p) = self.forced_plan(f, &conj).await? {
                    return self
                        .candidates(&p, &conj, after, limit, None)
                        .await
                        .map(Option::unwrap_or_default);
                }
                // 適用できない戦略の強制は自然な選択に戻す。
            }
            None => {}
        }
        let unit_a = self.units.per_entry(conj.len() + 1);
        let u = self.units;
        let (l, k) = (limit as f64, sort.len() as f64);
        let cost_of = move |c: u64| -> f64 {
            let c = c as f64;
            c * unit_a + c * k * u.c_probe + u.sort(c, l) + l * u.c_fetch
        };
        if let Some(stats) = self.catalog.clone() {
            // O2: 統計とコストで選ぶ（頑健な選択・受付制御）。
            let planned = self.plan_o2(&stats, &conj, sort, limit).await?;
            self.admit(planned.predicted_us, planned.confident, sort)
                .await?;
            return match planned.pick {
                Pick::Cand { plan, cap } => {
                    if let Some(out) = self
                        .candidates(&plan, &conj, after, limit, Some(cap))
                        .await?
                    {
                        return Ok(out);
                    }
                    // 推定を外れて候補が上限を超えた: 予算付きのソート駆動へ（切替はこれで 1 回）。
                    self.stats.switched = true;
                    let mut budget = self.budget_override.unwrap_or(limit * K_PREFETCH).max(1);
                    self.adaptive(
                        &conj,
                        sort,
                        after,
                        limit,
                        &mut budget,
                        None,
                        unit_a,
                        &cost_of,
                    )
                    .await
                }
                Pick::Sort { alt } => {
                    let mut budget = match (&self.budget_override, &alt) {
                        (Some(b), _) => *b,
                        (None, Some((_, c))) => (c / unit_a).ceil() as i64,
                        (None, None) => limit * K_PREFETCH,
                    }
                    .max(1);
                    let best = alt.map(|(p, _)| p);
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
            };
        }
        // O1: 上限付き件数探索だけで選ぶ（統計表なし・§4.3）。
        let mut plans = cand_plans(&conj);
        self.probe_plans(&mut plans).await?;
        if let Some(p) = self.policy_plan(true).await? {
            plans.push(p);
        }
        let best = plans
            .into_iter()
            .filter(|p| p.count.is_some())
            .min_by_key(|p| p.count.unwrap_or(u64::MAX));
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
        let scanned0 = self.stats.scanned;
        loop {
            match self.sort_step(conj, sort, &mut st, limit, *budget).await? {
                Step::Done => {
                    if self.stats.strategy != "composite" {
                        self.stats.strategy = "sort";
                    }
                    self.sort_walk_obs = Some((self.stats.scanned - scanned0) as f64);
                    return Ok(st.finish(sort, limit));
                }
                Step::Exhausted { scanned, passed } => {
                    scanned_total += scanned;
                    passed_total += passed;
                    // c が分からない間は、走査件数の 10 倍まで上限を広げて測り直す（§6.2 O1）。
                    if best.is_none() && reprobes < MAX_REPROBES {
                        reprobes += 1;
                        let cap = (scanned_total as i64 * 10).max(PROBE_CAP);
                        if let Some(mut p) = cand_plans(conj).into_iter().next() {
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
                    // 観測した選択率から見た「L 件そろうまでの走査件数」（補正係数の更新に使う）。
                    self.sort_walk_obs = Some(scanned_total as f64 + need / sel);
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
                } else if self.stats.strategy == "composite" {
                    "composite"
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
        let checks = self.checks(conj, &p.covered);
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
        let checks = self.checks(&conj, &covered);
        let q = self.low.q;
        let v = self
            .variant(V_COUNT)
            .source_with(&src, self.guard)
            .checks(&checks, q)
            .finish();
        let cap_i = cap as i64;
        let (text, binds) = self.compile("count", &v, |sql| {
            let guard = self.guard.lower("c.record_int", sql)?;
            let src = self.materialize(&src, sql);
            Ok(self.low.count_sql(&src, &checks, &guard, cap_i, sql))
        })?;
        self.stats.count_issued = true;
        let rows = self.fetch_compiled("count", text, binds).await?;
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
        let checks = self.checks(&conj, &covered);
        let q = self.low.q;
        let v = self
            .variant(V_AGGREGATE)
            .source_with(&src, self.guard)
            .checks(&checks, q)
            .finish();
        let (text, binds) = self.compile("aggregate", &v, |sql| {
            let guard = self.guard.lower("c.record_int", sql)?;
            let src = self.materialize(&src, sql);
            Ok(self.low.aggregate_sql(&src, &checks, &guard, sql))
        })?;
        self.fetch_compiled("aggregate", text, binds).await
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
        // 項の多い OR 駆動は各項の範囲を Roaring の和で合わせてから候補にする（§5.1）。
        let merged;
        let src = match src {
            Source::Union(atoms) if atoms.len() > super::batch::UNION_BITMAP_TERMS => {
                match self.union_ids(atoms, cap).await? {
                    Some(ids) => {
                        merged = Source::Ids(ids);
                        &merged
                    }
                    None => return Ok(None),
                }
            }
            other => other,
        };
        let q = self.low.q;
        let v = self
            .variant(V_CANDIDATES)
            .source_with(src, self.guard)
            .checks(checks, q)
            .flag(after.is_some())
            .flag(cap.is_some())
            .finish();
        let (text, binds) = self.compile("candidates", &v, |sql| {
            let guard = self.guard.lower("c.record_int", sql)?;
            let src = self.materialize(src, sql);
            Ok(self
                .low
                .candidates_sql(&src, checks, &guard, after, cap, limit, sql))
        })?;
        let rows = self.fetch_compiled("candidates", text, binds).await?;
        let n_cand: i64 = rows
            .first()
            .map(|r| r.try_get::<i64, _>("n_cand"))
            .transpose()
            .map_err(map_db)?
            .unwrap_or(0);
        self.stats.scanned += n_cand.max(0) as u64;
        // 候補件数の推定と実測が 10 倍以上ずれた（続いたら統計収集を前倒しする・§8）。
        if let Some(e) = self.est_cand.take() {
            let a = n_cand.max(0) as f64;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                self.stats.est_rows = Some(e.max(0.0).round() as u64);
            }
            self.stats.actual_rows = Some(n_cand.max(0) as u64);
            if a > 10.0 * e.max(1.0) || (e >= 10.0 && a * 10.0 < e) {
                self.stats.estimate_miss = true;
            }
        }
        self.last_pass = rows
            .first()
            .map(|r| r.try_get::<i64, _>("n_pass"))
            .transpose()
            .map_err(map_db)?
            .unwrap_or(0);
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
        let checks = self.order(conj.to_vec());
        if sort.len() > 1 {
            if let Some(c) = self.composite.clone() {
                return self.composite_step(&checks, st, limit, budget, &c).await;
            }
        }
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
        let multi = sort.len() > 1;
        // 第 1 キーの列の範囲条件で走査の始点・終点を縛る（範囲の外は検査を通らない・検証 D-3）。
        let seek = super::seek::sort_seek(&checks, key, kind);
        if st.scan_from.is_none() {
            if let Some(v) = seek.start.clone() {
                if multi && st.window_lo.is_none() {
                    st.window_lo = Some((v.clone(), true));
                }
                st.scan_from = Some((v, None));
            }
        }
        // 単一キー: (値, record_int) の厳密に後ろから。複数ソート: 同値群の先頭から（窓は候補 SQL が並べる）。
        let start = st.scan_from.as_ref().map(|(v, r)| (v, *r));
        let start_tag = match start {
            None => 0,
            Some((_, Some(_))) => 1,
            Some((_, None)) => 2,
        };
        let q = self.low.q;
        let v = self
            .variant(V_SORT_DRIVE)
            .num(u64::from(key.field))
            .flag(key.desc)
            .tag(start_tag)
            .tag(
                seek.stop
                    .as_ref()
                    .map_or(0, |(_, inc)| if *inc { 1 } else { 2 }),
            )
            .checks(&checks, q)
            .finish();
        let stop = seek.stop.as_ref();
        let (text, binds) = self.compile("sort_drive", &v, |sql| {
            let guard: GuardSql = self.guard.lower("d.record_int", sql)?;
            Ok(self
                .low
                .sort_drive_sql(key, start, stop, &checks, &guard, budget, need + 1, sql))
        })?;
        let rows = self.fetch_compiled("sort_drive", text, binds).await?;
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
                let rows = self
                    .prefetch_group(&src, &checks, after.as_ref(), limit, need)
                    .await?;
                st.collected.extend(rows);
                st.window_lo = Some((v_end.clone(), false));
                st.scan_from = Some((v_end, Some(if key.desc { i64::MIN } else { i64::MAX })));
            }
        }
        if st.collected.len() as i64 > limit {
            st.done = true;
            return Ok(Step::Done);
        }
        if end {
            if seek.needs_value {
                // 第 1 キーに値を要する条件がある: 値の無い行（区間 2）は検査を通らない。
                st.done = true;
                return Ok(Step::Done);
            }
            // 区間 1 の終端: 値なしの行（区間 2）へ。
            st.segment = 2;
            st.scan_rint = None;
            return self.missing_step(&checks, sort, st, limit, budget).await;
        }
        Ok(Step::Exhausted { scanned, passed })
    }

    /// 第 1 キーの同値群（窓）をページの K 倍まで先読みして全キーで並べる（data-platform §5.3 の既定）。
    /// 超えたら複合索引の材料化を要求し、この要求は窓を丸ごと並べて正しく返す
    /// （複合索引が active になるまでの劣化・`statement_timeout` が上限）。
    async fn prefetch_group(
        &mut self,
        src: &Source<'_>,
        checks: &[&Expr],
        after: Option<&Pos>,
        limit: i64,
        need: i64,
    ) -> Result<Vec<(i64, Pos)>, DataError> {
        let cap = K_PREFETCH * limit;
        if let Some(rows) = self
            .run_candidates(src, checks, after, Some(cap), need)
            .await?
        {
            return Ok(rows);
        }
        self.wants_composite = true;
        let rows = self
            .run_candidates(src, checks, after, None, need)
            .await?
            .unwrap_or_default();
        // 利用者へ明示するのは可視行（検査を通った行）で見た超過だけ（見えない行の量を漏らさない・PIT-75）。
        if self.last_pass > cap {
            self.sort_degraded = true;
        }
        Ok(rows)
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
            let rows = self
                .prefetch_group(&Source::All, &all, after.as_ref(), limit, need)
                .await?;
            st.collected.extend(rows);
            st.done = true;
            return Ok(Step::Done);
        }
        let q = self.low.q;
        let v = self
            .variant(V_MISSING_DRIVE)
            .num(u64::from(key.field))
            .flag(key.desc)
            .flag(st.scan_rint.is_some())
            .checks(checks, q)
            .finish();
        let after_rint = st.scan_rint;
        let (text, binds) = self.compile("missing_drive", &v, |sql| {
            let guard = self.guard.lower("d.record_int", sql)?;
            Ok(self
                .low
                .missing_drive_sql(key, after_rint, checks, &guard, budget, need + 1, sql))
        })?;
        let rows = self.fetch_compiled("missing_drive", text, binds).await?;
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
pub(super) struct SortState {
    pub(super) after: Option<Pos>,
    /// 複合索引の走査位置（符号化済みのキー）。
    pub(super) comp_after: Option<Vec<u8>>,
    pub(super) comp_started: bool,
    segment: u8,
    /// 区間 1 の走査位置（値, record_int）。`record_int` が `None` なら値の同値群の先頭から。
    scan_from: Option<(SortVal, Option<i64>)>,
    /// 区間 2 の走査位置。
    scan_rint: Option<i64>,
    /// 複数ソートの窓の開始（値, 含むか）。
    window_lo: Option<(SortVal, bool)>,
    pub(super) collected: Vec<(i64, Pos)>,
    pub(super) done: bool,
}

impl SortState {
    fn new(after: Option<&Pos>) -> Self {
        let mut st = SortState {
            after: after.cloned(),
            comp_after: None,
            comp_started: false,
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
