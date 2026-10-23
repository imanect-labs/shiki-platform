//! クエリ実行チョークポイント（Task 9.3・Task 15.3 でクエリコンパイラ v2 へ）。
//!
//! `data_record` と索引テーブルへの**全読取 SQL はこのモジュールだけが組み立てる**。行述語
//! （[`crate::policy`]）は全ての読取文に無条件に合成され（索引経路は `Guard(P)` を型で強制・
//! 本体経路と id 指定の読取は v1 のコンパイラ）、合成しない SELECT を書ける場所を構造的に無くす
//! （モジュール可視性 pub(crate)・crate 外非公開）。
//!
//! - [`ir`] — 外部契約（条件木・複数ソート・keyset・集計・件数）
//! - [`bound`] — 束縛・検証（索引到達可能性・マスク 403）。結合・半結合化・HAVING は [`bound_query`]
//! - [`scopes`] — リンクの半結合・結合の参照先テーブル（閲覧者本人の権限で開く・PIT-20）
//! - [`limits`] — 入口の資源上限（SQL 面と IR 直投入の共通・data-platform §16.1.1）
//! - [`reject`] — 入口での拒否理由（422・構造化・存在オラクルを作らないエラー契約）
//! - [`planner`] — 自前オプティマイザ O1〜O3（正規化・Guard・統計と件数探索・コスト・戦略・降下・
//!   予算付き実行・事後検査・フィードバック）
//! - [`admission`] — 予測コストによる受付制御とテナントの公平性（トークン）
//! - [`v1`] — 本体経路（機能フラグの切戻し先・索引未完成時・parity の基準）
//! - [`run`] — 実行入口（読取 Tx・本体取得・カーソル）
//! - [`executor`] — id 指定の読取・書込対象のロック・lookup 解決（v1 の述語）

pub(crate) mod admission;
pub(crate) mod aggregate;
pub(crate) mod bound;
pub(crate) mod bound_query;
#[cfg(test)]
pub(crate) mod eval;
pub(crate) mod executor;
pub(crate) mod ir;
pub(crate) mod join_run;
pub(crate) mod limits;
pub(crate) mod optimizer_ops;
pub(crate) mod planner;
pub(crate) mod prepare;
pub(crate) mod reject;
pub(crate) mod run;
pub(crate) mod scopes;
#[cfg(test)]
mod semantics_tests;
pub(crate) mod snapshot;
pub(crate) mod sql;
pub(crate) mod v1;
