//! 結果を画面に返すノード（`workflow.reply`・GAP-09・D-28）の params 契約。
//!
//! interactive の run で成功したこのノードの出力を、起動した本人の画面（ミニアプリのボタン・会話の図の
//! ボタン）に出す。値は値式で、形（`format`）に合わせて executor が整形する（`nodes/reply.rs`）。
//! 画面に出すのは整形した後の節だけで、run の入力や他のノードの出力はそのまま見せない。
//!
//! - `label`・`columns[].label` はリテラルの文字列（作成者が書いた固定の文言・アプリの利用者全員に見える）。
//! - `columns[].path`・`file.*_path` は行・要素の中の位置（JSON Pointer・`""` は行そのもの）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ParamsIssue;
use crate::ir::expr::ValueExpr;

/// 見出しの上限（文字数）。
pub const MAX_REPLY_LABEL_CHARS: usize = 40;
/// 表の列の上限。
pub const MAX_REPLY_COLUMNS: usize = 20;

/// 結果の形。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReplyFormat {
    /// 文章（改行を保つ）。
    Text,
    /// Markdown（画面の操作を持つ図は表示専用に無効化する）。
    Markdown,
    /// 表（配列の各要素が 1 行）。
    Table,
    /// ドライブのファイルへのリンクの一覧（開くときに通常の権限で確かめる）。
    FileLink,
}

/// 結果の色合い（業務上の失敗は error・注意は warning）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReplyTone {
    /// ふつう（既定）。
    #[default]
    Neutral,
    /// うまくいった。
    Success,
    /// 注意。
    Warning,
    /// うまくいかなかった（業務上の失敗）。
    Error,
}

/// 表の 1 列（見出しと、行の中の位置）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ReplyColumn {
    /// 列の見出し（1〜40 字）。
    pub label: String,
    /// 行の中の位置（JSON Pointer・例 `/clause`。配列の行は `/0`）。
    pub path: String,
}

/// ファイルへのリンクの取り出し方（要素の中の位置）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ReplyFileSpec {
    /// ファイルの ID の位置（省略時は `/file_id`、無ければ `/id`・D-96）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id_path: Option<String>,
    /// ファイルの名前の位置（省略時は `/name`、無ければ `/file_name`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name_path: Option<String>,
}

/// `workflow.reply` — 結果を画面に返す（ir.md §7）。表示されるのはボタン・会話から起動した本人だけ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WorkflowReplyParams {
    /// 結果の形。
    pub format: ReplyFormat,
    /// 見出し（40 字以内のリテラル・省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    /// 色合い（既定 neutral）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tone: Option<ReplyTone>,
    /// 返す値（text・markdown は文字、table は配列、file_link はオブジェクトか配列）。
    pub value: ValueExpr,
    /// 表の列（table のときだけ・20 列まで。省略すると行のキーを使う・並びは名前順になるので、見せたい順があれば指定する）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub columns: Option<Vec<ReplyColumn>>,
    /// ファイルの取り出し方（file_link のときだけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub file: Option<ReplyFileSpec>,
}

/// JSON Pointer の形か（`""` は全体・それ以外は `/` で始める）。
fn is_pointer(s: &str) -> bool {
    s.is_empty() || s.starts_with('/')
}

impl WorkflowReplyParams {
    /// 文字数・件数・形と項目の組み合わせの検査（serde では表せない分・V1 の厳密性）。
    pub fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        let issue = |path: &str, message: String| {
            Err(ParamsIssue {
                expected: Vec::new(),
                path: format!("/params/{path}"),
                message,
            })
        };
        if let Some(label) = &self.label {
            if label.trim().is_empty() || label.chars().count() > MAX_REPLY_LABEL_CHARS {
                return issue(
                    "label",
                    format!("見出しは 1〜{MAX_REPLY_LABEL_CHARS} 字で書いてください（無ければ項目ごと省く）"),
                );
            }
        }
        if let Some(columns) = &self.columns {
            if self.format != ReplyFormat::Table {
                return issue(
                    "columns",
                    "列は形が「表」のときだけ指定できます".to_string(),
                );
            }
            if columns.is_empty() || columns.len() > MAX_REPLY_COLUMNS {
                return issue(
                    "columns",
                    format!("列は 1〜{MAX_REPLY_COLUMNS} 列で指定してください（省略すると行の項目をそのまま使う）"),
                );
            }
            for (i, c) in columns.iter().enumerate() {
                if c.label.trim().is_empty() || c.label.chars().count() > MAX_REPLY_LABEL_CHARS {
                    return issue(
                        &format!("columns/{i}/label"),
                        format!("列の見出しは 1〜{MAX_REPLY_LABEL_CHARS} 字で書いてください"),
                    );
                }
                if !is_pointer(&c.path) {
                    return issue(
                        &format!("columns/{i}/path"),
                        "列の位置は / で始めて書いてください（例: /name）".to_string(),
                    );
                }
            }
        }
        if let Some(file) = &self.file {
            if self.format != ReplyFormat::FileLink {
                return issue(
                    "file",
                    "ファイルの取り出し方は形が「ファイルへのリンク」のときだけ指定できます"
                        .to_string(),
                );
            }
            for (key, path) in [("id_path", &file.id_path), ("name_path", &file.name_path)] {
                if path.as_deref().is_some_and(|p| !is_pointer(p)) {
                    return issue(
                        &format!("file/{key}"),
                        "位置は / で始めて書いてください（例: /file_id）".to_string(),
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn params(v: serde_json::Value) -> WorkflowReplyParams {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn accepts_each_format() {
        for v in [
            json!({ "format": "text", "value": "受け付けました" }),
            json!({ "format": "markdown", "label": "回答", "tone": "success",
                    "value": { "$from": "nodes.answer.output", "path": "/text" } }),
            json!({ "format": "table", "value": { "$from": "nodes.q.output", "path": "/items" },
                    "columns": [{ "label": "条項", "path": "/clause" }] }),
            json!({ "format": "file_link", "value": { "$from": "nodes.s.output", "path": "/results" },
                    "file": { "id_path": "/file_id", "name_path": "/file_name" } }),
        ] {
            assert!(params(v.clone()).check_cross_fields().is_ok(), "{v}");
        }
    }

    #[test]
    fn rejects_bad_combinations_with_paths() {
        let cases = [
            (
                json!({ "format": "text", "value": "x", "label": "" }),
                "/params/label",
            ),
            (
                json!({ "format": "text", "value": "x", "label": "あ".repeat(41) }),
                "/params/label",
            ),
            (
                json!({ "format": "text", "value": "x", "columns": [{ "label": "a", "path": "/a" }] }),
                "/params/columns",
            ),
            (
                json!({ "format": "table", "value": [], "columns": [{ "label": "a", "path": "a" }] }),
                "/params/columns/0/path",
            ),
            (
                json!({ "format": "table", "value": [], "columns": [] }),
                "/params/columns",
            ),
            (
                json!({ "format": "table", "value": [], "file": {} }),
                "/params/file",
            ),
            (
                json!({ "format": "file_link", "value": [], "file": { "id_path": "id" } }),
                "/params/file/id_path",
            ),
        ];
        for (v, path) in cases {
            let e = params(v.clone()).check_cross_fields().unwrap_err();
            assert_eq!(e.path, path, "{v}");
        }
        let many: Vec<_> = (0..21)
            .map(|i| json!({ "label": format!("c{i}"), "path": format!("/c{i}") }))
            .collect();
        let e = params(json!({ "format": "table", "value": [], "columns": many }))
            .check_cross_fields()
            .unwrap_err();
        assert_eq!(e.path, "/params/columns");
    }
}
