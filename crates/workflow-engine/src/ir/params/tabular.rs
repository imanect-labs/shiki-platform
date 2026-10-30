//! CSV 表データノードの params 契約（Task 11P.9・ir.md §7）。
//!
//! 実体は隔離 DuckDB 経由の `TabularService`（Task 11P.7）。認可は**操作別のファイル ReBAC**
//! （query=viewer / patch=editor / write=作成権限）で、engine 側は scope 天井のみを担保する。
//! `csv.patch` / `csv.write` は書込のため EngineDedup（effect_journal 冪等キーで at-least-once
//! 重複排除・PIT-31）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{CsvEncoding, NameTakenPolicy};
use crate::ir::expr::ValueExpr;

/// `csv.query` — CSV への読み取り専用 SQL（テーブル名 `data`・viewer・pure）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CsvQueryParams {
    /// 対象 CSV ファイル id（UUID に解決される値）。
    #[serde(default = "super::unset")]
    pub file: ValueExpr,
    /// 読み取り専用 SELECT（テーブル名 `data`）。
    pub sql: ValueExpr,
}

/// `csv.patch` — CSV をパッチ編集して新バージョンを保存（editor・EngineDedup）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CsvPatchParams {
    /// 対象 CSV ファイル id。
    #[serde(default = "super::unset")]
    pub file: ValueExpr,
    /// 編集前の版（node.version・楽観ロックの base）。
    #[serde(default = "super::unset")]
    pub base_rev: ValueExpr,
    /// パッチ操作の配列（tabular の `PatchOp` に解決される JSON 値）。
    #[serde(default = "super::unset")]
    pub ops: ValueExpr,
}

/// `csv.write` — 新規 CSV を保存（保存先フォルダの作成権限・EngineDedup）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CsvWriteParams {
    /// 保存先フォルダ id（省略時は実行主体のルート）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub folder: Option<ValueExpr>,
    /// ファイル名（`.csv` は自動付与）。
    pub name: ValueExpr,
    /// CSV 本文（ヘッダ行＋データ行）。
    pub content: ValueExpr,
    /// 同じ名前のファイルがあるとき（省略時は失敗にする・`storage.write` と同じ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub on_name_taken: Option<NameTakenPolicy>,
    /// 文字コード（省略時は BOM なしの UTF-8・この項目が無かったころの定義の挙動・D-120）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encoding: Option<CsvEncoding>,
}
