//! 入口の資源上限（data-platform §16.1.1・Task 15.10）。
//!
//! `statement_timeout` は DB の実行時間の上限でしかなく、パーサ・AST・コンパイルの CPU / メモリを
//! 制限しない。巨大な入力・深い入れ子・大量のノードやリテラルによる資源枯渇をここで止める。
//! **SQL 前段面（[`crate::sql`]）と IR 直投入（[`crate::DataQuery`]）の両方に同じ上限を課す**。
//! 超過は 422（[`QueryRejection::limit`]・どの上限に当たったかを含める）。
//!
//! コンパイルの費用は**仕事量（単位）で決定的に数える**（[`MAX_COMPILE_WORK`]）。壁時計
//! （[`MAX_COMPILE_MS`]）は、仕事量の数え漏れに備えた外側の安全網としてだけ残す（負荷で止められた
//! スレッドが小さいクエリを誤って弾かないよう、通常の入力では届かない値にしてある）。

use std::cell::Cell;
use std::time::{Duration, Instant};

use super::ir::{Condition, DataQuery, HavingCond};
use super::reject::QueryRejection;

/// 入口の資源上限（既定値は data-platform §16.1.1 の表・設定で変えられる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryLimits {
    /// SQL テキストの長さ（IR は JSON 表現の長さ）。パーサへ渡す前に弾く。
    pub max_sql_bytes: usize,
    /// 入れ子の深さ（括弧・NOT・副式・条件木）。
    pub max_ast_depth: usize,
    /// ノード数（横に広い式・巨大な `IN` リスト）。
    pub max_ast_nodes: usize,
    /// リテラル数（`IN (...)` の要素を含む）。
    pub max_literals: usize,
    /// パース・検証・束縛・IR 変換・正規化のコンパイル全体の仕事量（単位。I/O を除く・決定的）。
    pub max_compile_work: u64,
    /// 同じ段の経過時間の外側の安全網（壁時計。仕事量の数え漏れに備える）。
    pub max_compile: Duration,
    /// JOIN / リンクの半結合の段数（自己結合・多段結合を含む）。
    pub max_join_depth: usize,
}

/// `MAX_SQL_BYTES`。
pub const MAX_SQL_BYTES: usize = 64 * 1024;
/// `MAX_AST_DEPTH`。
pub const MAX_AST_DEPTH: usize = 32;
/// `MAX_AST_NODES`。
pub const MAX_AST_NODES: usize = 4_096;
/// `MAX_LITERALS`。
pub const MAX_LITERALS: usize = 1_024;
/// `MAX_COMPILE_WORK`（コンパイルの仕事量の上限・単位）。
///
/// 1 単位は「ノード 1 個の訪問」「値・式の比較 1 回」「スキーマのフィールド 1 個の走査」程度の
/// 定数時間の仕事。業務的なクエリは数百単位（評価用 50 問の正解 SQL で最大 411）、静的な上限内の
/// 最悪形（同じ列の `IN` を `OR` で束ねた約 1,000 値の重複除去）でも約 55 万単位に収まる。
/// 静的な上限を緩めると正規化の重複除去・集合の交差が二乗で増え、これを超える
/// （`sql_it::compile_work_budget_passes_the_static_worst_case_and_cuts_superlinear_cost`）。
pub const MAX_COMPILE_WORK: u64 = 2_000_000;
/// `MAX_COMPILE_MS`（外側の安全網。旧 50 ms の 10 倍）。
pub const MAX_COMPILE_MS: u64 = 500;
/// `MAX_JOIN_DEPTH`（JOIN の数・リンクの半結合の入れ子の深さ）。
pub const MAX_JOIN_DEPTH: usize = 3;

impl Default for QueryLimits {
    fn default() -> Self {
        QueryLimits {
            max_sql_bytes: MAX_SQL_BYTES,
            max_ast_depth: MAX_AST_DEPTH,
            max_ast_nodes: MAX_AST_NODES,
            max_literals: MAX_LITERALS,
            max_compile_work: MAX_COMPILE_WORK,
            max_compile: Duration::from_millis(MAX_COMPILE_MS),
            max_join_depth: MAX_JOIN_DEPTH,
        }
    }
}

impl QueryLimits {
    pub(crate) fn bytes(&self) -> QueryRejection {
        QueryRejection::limit("max_sql_bytes", self.max_sql_bytes as u64)
    }
    pub(crate) fn depth(&self) -> QueryRejection {
        QueryRejection::limit("max_ast_depth", self.max_ast_depth as u64)
    }
    pub(crate) fn nodes(&self) -> QueryRejection {
        QueryRejection::limit("max_ast_nodes", self.max_ast_nodes as u64)
    }
    pub(crate) fn literals(&self) -> QueryRejection {
        QueryRejection::limit("max_literals", self.max_literals as u64)
    }
    pub(crate) fn work(&self) -> QueryRejection {
        QueryRejection::limit("max_compile_work", self.max_compile_work)
    }
    pub(crate) fn compile(&self) -> QueryRejection {
        QueryRejection::limit(
            "max_compile_ms",
            u64::try_from(self.max_compile.as_millis()).unwrap_or(u64::MAX),
        )
    }
    pub(crate) fn joins(&self) -> QueryRejection {
        QueryRejection::limit("max_join_depth", self.max_join_depth as u64)
    }
}

thread_local! {
    /// 実行中の段の仕事量の計（使った量, この段で使える量）。段の外では上限なし。
    static METER: Cell<(u64, u64)> = const { Cell::new((0, u64::MAX)) };
}

/// 仕事量を `n` 単位積む。この段の予算を使い切ったら `false`（呼び出し側は省ける仕事を省いてよい。
/// 段の終わりで [`CompileBudget::stage`] が 422 にする）。段の外で呼んでも害はない。
pub(crate) fn charge(n: usize) -> bool {
    METER.with(|m| {
        let (used, cap) = m.get();
        let used = used.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        m.set((used, cap));
        used <= cap
    })
}

/// コンパイルの予算（同期の段ごとに仕事量と経過時間を積む。I/O の待ちは段の外なので数えない）。
///
/// 判定の主は**仕事量**（決定的・負荷に左右されない）。経過時間は壁時計（`Instant`）で、
/// スレッドが止められた時間も積むため、通常の入力では届かない外側の安全網にしてある。
#[derive(Debug)]
pub(crate) struct CompileBudget {
    work: u64,
    max_work: u64,
    spent: Duration,
    max: Duration,
}

impl CompileBudget {
    pub(crate) fn new(l: &QueryLimits) -> Self {
        CompileBudget {
            work: 0,
            max_work: l.max_compile_work,
            spent: Duration::ZERO,
            max: l.max_compile,
        }
    }

    /// 1 段を測って積み、予算を超えたら 422（仕事量を先に判定する）。
    pub(crate) fn stage<T>(
        &mut self,
        l: &QueryLimits,
        f: impl FnOnce() -> T,
    ) -> Result<T, QueryRejection> {
        let left = self.max_work.saturating_sub(self.work);
        let prev = METER.with(|m| m.replace((0, left)));
        let t = Instant::now();
        let out = f();
        self.spent += t.elapsed();
        let (used, _) = METER.with(|m| m.replace(prev));
        self.work = self.work.saturating_add(used);
        if self.work > self.max_work {
            return Err(l.work());
        }
        if self.spent > self.max {
            return Err(l.compile());
        }
        Ok(out)
    }

    /// これまでに積んだ仕事量（検証用）。
    #[cfg(test)]
    pub(crate) fn work(&self) -> u64 {
        self.work
    }
}

/// IR の大きさの集計。
#[derive(Debug, Default)]
struct Count {
    nodes: usize,
    literals: usize,
    depth: usize,
    links: usize,
}

fn value_literals(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Array(a) => a.iter().map(value_literals).sum::<usize>().max(1),
        serde_json::Value::Object(m) => m.values().map(value_literals).sum::<usize>().max(1),
        _ => 1,
    }
}

fn walk(c: &Condition, depth: usize, link_depth: usize, n: &mut Count) {
    n.nodes += 1;
    n.depth = n.depth.max(depth);
    n.links = n.links.max(link_depth);
    match c {
        Condition::All(v) | Condition::Any(v) => {
            for x in v {
                walk(x, depth + 1, link_depth, n);
            }
        }
        Condition::Not(x) => walk(x, depth + 1, link_depth, n),
        Condition::Cmp(cmp) => {
            n.nodes += 1;
            if let Some(v) = &cmp.value {
                n.literals += value_literals(v);
                if let serde_json::Value::Array(a) = v {
                    n.nodes += a.len();
                }
            }
        }
        Condition::Link(l) => {
            n.links = n.links.max(link_depth + 1);
            if let Some(f) = &l.filter {
                walk(f, depth + 1, link_depth + 1, n);
            }
        }
    }
}

fn walk_having(h: &HavingCond, depth: usize, n: &mut Count) {
    n.nodes += 1;
    n.depth = n.depth.max(depth);
    match h {
        HavingCond::All(v) | HavingCond::Any(v) => {
            for x in v {
                walk_having(x, depth + 1, n);
            }
        }
        HavingCond::Not(x) => walk_having(x, depth + 1, n),
        HavingCond::Cmp(_) => {
            n.nodes += 1;
            n.literals += 1;
        }
    }
}

/// IR 直投入の資源上限（SQL 面と同じ上限・data-platform §16.1.1）。
pub(crate) fn check_ir(q: &DataQuery, l: &QueryLimits) -> Result<(), QueryRejection> {
    let bytes = serde_json::to_vec(q).map_or(usize::MAX, |v| v.len());
    if bytes > l.max_sql_bytes {
        return Err(l.bytes());
    }
    let mut n = Count::default();
    if let Some(f) = &q.filter {
        walk(f, 1, 0, &mut n);
    }
    for j in &q.joins {
        n.nodes += 1;
        if let Some(f) = &j.filter {
            walk(f, 1, 1, &mut n);
        }
    }
    if let Some(h) = &q.having {
        walk_having(h, 1, &mut n);
    }
    n.nodes += q.sort.len()
        + q.select.as_ref().map_or(0, Vec::len)
        + q.group_by.len()
        + q.aggregate.len()
        + q.group_sort.len();
    if q.search.is_some() {
        n.literals += 1;
        n.nodes += 1;
    }
    if n.depth > l.max_ast_depth {
        return Err(l.depth());
    }
    if n.nodes > l.max_ast_nodes {
        return Err(l.nodes());
    }
    if n.literals > l.max_literals {
        return Err(l.literals());
    }
    if q.joins.len() > l.max_join_depth || n.links > l.max_join_depth {
        return Err(l.joins());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::query::ir::CondOp;
    use crate::query::reject::RejectCode;
    use serde_json::json;

    fn limit_name(r: Result<(), QueryRejection>) -> String {
        let e = r.unwrap_err();
        assert_eq!(e.code, RejectCode::LimitExceeded);
        e.limit.unwrap().name
    }

    #[test]
    fn each_limit_rejects_the_ir_path() {
        let lim = QueryLimits::default();
        // バイト数
        let q = DataQuery {
            filter: Some(Condition::cmp(
                "a",
                CondOp::Eq,
                Some(json!("x".repeat(70_000))),
            )),
            ..DataQuery::default()
        };
        assert_eq!(limit_name(check_ir(&q, &lim)), "max_sql_bytes");
        // 深さ
        let mut c = Condition::cmp("a", CondOp::Exists, None);
        for _ in 0..40 {
            c = Condition::Not(Box::new(c));
        }
        let q = DataQuery {
            filter: Some(c),
            ..DataQuery::default()
        };
        assert_eq!(limit_name(check_ir(&q, &lim)), "max_ast_depth");
        // ノード数（リテラルなし・短い名前を横に並べる）
        let q = DataQuery {
            select: Some((0..4_200).map(|_| "a".to_string()).collect()),
            ..DataQuery::default()
        };
        assert_eq!(limit_name(check_ir(&q, &lim)), "max_ast_nodes");
        // リテラル数
        let q = DataQuery {
            filter: Some(Condition::cmp(
                "a",
                CondOp::In,
                Some(json!((0..1_100).collect::<Vec<_>>())),
            )),
            ..DataQuery::default()
        };
        assert_eq!(limit_name(check_ir(&q, &lim)), "max_literals");
        // 仕事量（決定的: 予算 1 の段で 2 単位積めば必ず超える。超えた後の charge は false）
        let tight = QueryLimits {
            max_compile_work: 1,
            ..QueryLimits::default()
        };
        let mut b = CompileBudget::new(&tight);
        let e = b
            .stage(&tight, || {
                assert!(charge(1));
                assert!(!charge(1));
            })
            .unwrap_err();
        assert_eq!(e.limit.unwrap().name, "max_compile_work");
        // 予算内なら段をまたいで積む（段の外の charge は数えない）
        let mut b = CompileBudget::new(&QueryLimits::default());
        b.stage(&QueryLimits::default(), || charge(3)).unwrap();
        assert!(charge(1_000_000_000));
        b.stage(&QueryLimits::default(), || charge(4)).unwrap();
        assert_eq!(b.work(), 7);
        // 時間の安全網（外側）
        let tight = QueryLimits {
            max_compile: Duration::ZERO,
            ..QueryLimits::default()
        };
        let mut b = CompileBudget::new(&tight);
        let e = b
            .stage(&tight, || std::thread::sleep(Duration::from_millis(1)))
            .unwrap_err();
        assert_eq!(e.limit.unwrap().name, "max_compile_ms");
    }
}
