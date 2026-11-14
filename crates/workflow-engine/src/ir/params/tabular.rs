//! CSV 表データノードの params 契約（Task 11P.9・ir.md §7）。
//!
//! 実体は隔離 DuckDB 経由の `TabularService`（Task 11P.7）。認可は**操作別のファイル ReBAC**
//! （query=viewer / patch=editor / write=作成権限）で、engine 側は scope 天井のみを担保する。
//! `csv.patch` / `csv.write` は書込のため EngineDedup（effect_journal 冪等キーで at-least-once
//! 重複排除・PIT-31）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tabular_protocol::tables::{
    table_name_issue, too_many_tables, TableNameIssue, MAX_EXTRA_TABLES, PRIMARY_TABLE,
};
use ts_rs::TS;

use super::{CsvEncoding, NameTakenPolicy, ParamsIssue};
use crate::ir::expr::ValueExpr;

/// `csv.query` — CSV への読み取り専用 SQL（テーブル名 `data`・viewer・pure）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CsvQueryParams {
    /// 対象 CSV ファイル id（UUID に解決される値）。
    #[serde(default = "super::unset")]
    pub file: ValueExpr,
    /// 読み取り専用 SELECT（`file` はテーブル `data`、`tables` の各 CSV は指定名）。
    pub sql: ValueExpr,
    /// JOIN 用の追加 CSV（#1037）。`[{ "name": "<テーブル名>", "file": "<UUID>" }, ...]` に
    /// 解決される値（最大 8 個）。各ファイルにも実行主体の閲覧権限が要る。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tables: Option<ValueExpr>,
}

impl CsvQueryParams {
    /// つなぐ CSV（`tables`）の数と、リテラルで書いた名前（形・`data` 以外・重ねない）。
    ///
    /// 判定は CSV の集計の入口（tabular の `validate_tables`）と同じ関数（[`too_many_tables`]・[`table_name_issue`]）で
    /// 行い、実行で必ず断られる指定を保存で止める（GAP-126）。`tables` 自体の値式・要素の値式・名前の値式は実行まで
    /// 分からないので確かめない（重なりも、リテラルの名前どうしだけを比べる）。空の名前は、有効化と実行の前に止める
    /// 必須の欄の空（`readiness`）として扱う。
    pub(crate) fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        let Some(ValueExpr::Literal(Value::Array(items))) = &self.tables else {
            return Ok(());
        };
        if too_many_tables(items.len()) {
            return Err(ParamsIssue {
                path: "/params/tables".to_string(),
                message: format!(
                    "つなぐ CSV は {MAX_EXTRA_TABLES} 個までです（{} 個あります）。減らしてください。",
                    items.len()
                ),
            });
        }
        let mut earlier: Vec<&str> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            if name.trim().is_empty() {
                continue;
            }
            let why = match table_name_issue(name, earlier.iter().copied()) {
                None => {
                    earlier.push(name);
                    continue;
                }
                Some(TableNameIssue::Shape) => {
                    "英小文字・数字・_ で、先頭は数字以外・63 文字までにしてください".to_string()
                }
                Some(TableNameIssue::Reserved) => {
                    format!("{PRIMARY_TABLE} は集計するファイル（file）の名前なので、ほかの名前にしてください")
                }
                Some(TableNameIssue::Duplicate) => {
                    "前のつなぐ CSV と同じ名前なので、ほかの名前にしてください".to_string()
                }
            };
            return Err(ParamsIssue {
                path: format!("/params/tables/{i}/name"),
                message: format!(
                    "{} つ目のつなぐ CSV（tables）の SQL での名前「{name}」は使えません。{why}。",
                    i + 1
                ),
            });
        }
        Ok(())
    }
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
    /// CSV 本文（ヘッダ行＋データ行の文字）。表（`csv.query` の結果 `{columns, rows}`）と項目のまとまりのリストは、
    /// 実行時に CSV に組み立てる（JSON の文字のまま保存しない・BUG-178）。
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::ir::params::check_params;
    use crate::vocab::NodeType;

    const FILE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";

    /// GAP-126: つなぐ CSV の名前（形・data 以外・重ねない）と数は、CSV の集計の入口と同じ判定で保存時に止める。
    /// 値式の名前・`tables` 自体の値式・空の名前（有効化の前に止める）は確かめない。
    #[test]
    fn csv_table_names_and_count_are_checked_like_the_runner() {
        let params = |tables: serde_json::Value| json!({ "file": FILE, "sql": "SELECT * FROM data", "tables": tables });
        let t = |name: serde_json::Value| json!({ "name": name, "file": FILE });
        let ok = [
            params(json!([t(json!("master")), t(json!("_tmp2"))])),
            params(json!([
                t(json!("master")),
                t(json!({ "$from": "input", "path": "/n" }))
            ])),
            params(json!([t(json!("")), t(json!(" "))])),
            params(json!({ "$from": "input", "path": "/tables" })),
            params(json!((0..tabular_protocol::tables::MAX_EXTRA_TABLES)
                .map(|i| t(json!(format!("t{i}"))))
                .collect::<Vec<_>>())),
        ];
        for p in ok {
            assert!(check_params(NodeType::CsvQuery, &p).is_ok(), "{p}");
        }
        let bad = [
            (
                json!([t(json!("2024_sales"))]),
                "/params/tables/0/name",
                "英小文字",
            ),
            (
                json!([t(json!("Master"))]),
                "/params/tables/0/name",
                "英小文字",
            ),
            (
                json!([t(json!("m".repeat(64)))]),
                "/params/tables/0/name",
                "63 文字",
            ),
            (
                json!([t(json!("a")), t(json!("data"))]),
                "/params/tables/1/name",
                "2 つ目のつなぐ CSV（tables）の SQL での名前「data」は使えません。data は集計するファイル（file）の名前なので",
            ),
            (
                json!([t(json!("a")), t(json!("b")), t(json!("a"))]),
                "/params/tables/2/name",
                "同じ名前",
            ),
            (
                json!((0..=tabular_protocol::tables::MAX_EXTRA_TABLES)
                    .map(|i| t(json!(format!("t{i}"))))
                    .collect::<Vec<_>>()),
                "/params/tables",
                "個まで",
            ),
        ];
        for (tables, path, says) in bad {
            let issue = check_params(NodeType::CsvQuery, &params(tables.clone())).unwrap_err();
            assert_eq!(issue.path, path, "{tables}");
            assert!(issue.message.contains(says), "{}", issue.message);
        }
    }
}
