//! 自前クエリオプティマイザ（最小構成 O1・query-optimizer.md §10・Task 15.3）。
//!
//! ```text
//! ① 束縛・検証           … crate::query::bound
//! ② 正規化               … normalize
//! ③ 認可の合成 Q ∧ Guard(P) … guard（P は弱められない型で包む）
//! ④ 物理計画             … exec（上限付き件数探索 stats ＋ コスト）
//! ⑤ 降下                 … lower（唯一の SQL 生成箇所）
//! ⑥ 実行                 … exec（予算付き・適応切替）
//! ⑦ 事後検査             … verify（Rust で行述語を再評価）
//! ⑧ フィードバック       … feedback（走査効率）
//! ```
//!
//! `stats`（件数探索とキャッシュ）は**このモジュールの外から見えない**（PIT-75）。

mod composite;
pub(crate) mod exec;
pub(crate) mod feedback;
pub(crate) mod guard;
pub(crate) mod keys;
pub(crate) mod lower;
pub(crate) mod normalize;
mod stats;
pub(crate) mod verify;

pub(crate) use stats::ProbeCache;

/// 計画器の外（実行入口）が件数探索器を組むための入口（統計値そのものは返さない）。
pub(crate) fn prober(cache: &ProbeCache, keys: crate::model::TableKeys) -> stats::Prober<'_> {
    stats::Prober {
        cache,
        keys,
        used: 0,
        statements: Vec::new(),
    }
}

/// 件数探索で使った文（デバッグ取得用）。
pub(crate) fn probe_statements<'p>(
    p: &'p stats::Prober<'_>,
) -> impl Iterator<Item = (&'p String, &'p Vec<crate::query::sql::Bind>)> {
    p.statements.iter().map(|(s, b)| (s, b))
}
