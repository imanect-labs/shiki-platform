//! 自前クエリオプティマイザ（O1: Task 15.3・O2 / O3: Task 15.12・query-optimizer.md §10）。
//!
//! ```text
//! ① 束縛・検証           … crate::query::bound
//! ② 正規化               … normalize
//! ③ 認可の合成 Q ∧ Guard(P) … guard（P は弱められない型で包む）
//! ④ 物理計画             … strategy（統計 catalog・推定 estimate・コスト cost・頑健な選択）／
//!                          exec（O1: 上限付き件数探索 stats だけで選ぶ）・plans（候補の列挙）
//! ⑤ 降下                 … lower（唯一の SQL 生成箇所）
//! ⑥ 実行                 … exec（予算付き・適応切替）・composite（複合索引）・batch（O3 バッチ駆動）
//! ⑦ 事後検査             … verify（Rust で行述語を再評価）
//! ⑧ フィードバック       … feedback（走査効率・補正係数・予測と実測の相関・索引の候補）
//! ```
//!
//! `stats`（件数探索とキャッシュ）と `catalog`（統計表の読み出し）は**このモジュールの外から見えない**
//! （PIT-75: 統計の読取経路を planner に閉じる）。

pub(crate) mod batch;
mod catalog;
mod composite;
pub(crate) mod cost;
mod estimate;
pub(crate) mod exec;
pub(crate) mod feedback;
pub(crate) mod guard;
pub(crate) mod join;
pub(crate) mod keys;
pub(crate) mod lower;
pub(crate) mod normalize;
pub(crate) mod plan_cache;
mod plans;
pub(crate) mod shape;
mod stats;
mod strategy;
pub(crate) mod verify;

pub(crate) use catalog::StatsCache;
pub(crate) use stats::ProbeCache;

/// 計画器の外（実行入口）が件数探索器を組むための入口（統計値そのものは返さない）。
pub(crate) fn prober<'a>(
    cache: &'a ProbeCache,
    keys: crate::model::TableKeys,
    plan: Option<plan_cache::PlanHandle<'a>>,
) -> stats::Prober<'a> {
    stats::Prober {
        cache,
        keys,
        used: 0,
        statements: Vec::new(),
        plan,
    }
}

/// 件数探索で使った文（デバッグ取得用）。
pub(crate) fn probe_statements<'p>(
    p: &'p stats::Prober<'_>,
) -> impl Iterator<Item = (&'p String, &'p Vec<crate::query::sql::Bind>)> {
    p.statements.iter().map(|(s, b)| (s, b))
}
