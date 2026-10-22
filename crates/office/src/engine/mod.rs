//! Shiki Office Engine（#725・design §4.8）。
//!
//! AI が Word/Excel を「レイアウト水準で何でも」操作するための層。kit（LibreOffice）の中で
//! 走る**固定の JS インタプリタ**（`crates/office/engine/*.js`）へ、検証・正規化済みの JSON を
//! `executescript` で渡す（PIT-68 ⑤: LLM のコードは kit に入れない）。
//!
//! - `a1` / `formula`: Excel の書き方（A1 表記・数式）の解釈と Calc の API 文法への変換
//! - `calc`: Excel の操作語彙・正規化・読み取りの整形・実行
//! - `script`: 固定ソースの組み立て
//! - `service`: セッション（認可・直列化・保存）に包んだ入口 [`OfficeEngine`]

pub mod a1;
pub mod calc;
pub mod chart;
pub mod chart_insert;
pub mod formula;
pub mod script;
mod service;
pub mod writer;

pub use service::{
    ChartInsertReport, OfficeEngine, OfficeEngineError, SheetEditReport, SheetReadRequest,
    DOC_CONTENT_TYPES, SHEET_CONTENT_TYPES,
};

use serde_json::Value;

use crate::live::{CoolWsClient, LiveError, ScriptResult};

/// エンジン呼び出しの失敗。
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// セッション（WS）の失敗。
    #[error(transparent)]
    Session(#[from] LiveError),
    /// kit 側の例外（構文エラー・未捕捉）。エンジンの不具合なのでログに残す。
    #[error("文書エンジンが異常終了しました: {0}")]
    Script(String),
    /// エンジンが操作を拒否した（変更は取り消し済み）。
    #[error("{message}")]
    Rejected {
        code: String,
        message: String,
        stage: String,
        op_index: Option<usize>,
    },
}

/// エンジンの action を 1 回呼ぶ。`{ok: true, result}` の result を返す。
pub(crate) async fn call(client: &mut CoolWsClient, req: Value) -> Result<Value, EngineError> {
    match client
        .execute_script(&script::ENGINE_SOURCE, &Value::Array(vec![req]))
        .await?
    {
        ScriptResult::Ok(v) if v["ok"] == Value::Bool(true) => Ok(v["result"].clone()),
        ScriptResult::Ok(v) => {
            let e = &v["error"];
            Err(EngineError::Rejected {
                code: e["code"].as_str().unwrap_or("unknown").to_string(),
                message: e["message"]
                    .as_str()
                    .unwrap_or("操作に失敗しました")
                    .to_string(),
                stage: e["stage"].as_str().unwrap_or("").to_string(),
                op_index: e["op_index"].as_u64().and_then(|i| usize::try_from(i).ok()),
            })
        }
        ScriptResult::Err(e) => {
            tracing::error!(error = %e, "office engine: kit 内のスクリプトが例外で終了");
            Err(EngineError::Script(e))
        }
    }
}

/// 真偽の項目（ops の bool / Option<bool>）。モデルが 1・0・"true" と書いても受ける。
const BOOL_KEYS: &[&str] = &[
    "all",
    "allow_blank",
    "allow_row_split",
    "banded",
    "bold",
    "center",
    "center_horizontally",
    "clear_formatting",
    "data_labels",
    "enabled",
    "first_column",
    "first_column_is_header",
    "first_row_is_header",
    "gridlines",
    "has_header",
    "include_tables",
    "italic",
    "keep_with_next",
    "legend",
    "match_case",
    "page_break_before",
    "percent",
    "shrink",
    "stacked",
    "strike",
    "total_row",
    "underline",
    "visible",
    "whole_word",
    "wrap",
];

/// 1 つの操作の、意味が一意な書き方の揺れを直す（Gemma 4 の評価で繰り返し見た形）:
/// - 各操作に書かれた `summary`（一括の要約と取り違えたもの）は捨てる
/// - 操作名と同じ項目に包んだ中身（`{"op":"page_setup","page_setup":{...}}`）は外へ出す
/// - header / footer に書かれた `target` は捨てる（ページ全体に効く操作で対象を持たない）
pub(crate) fn relax_op(v: &mut serde_json::Value) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    // op を書かずに、操作名を項目名にして中身を包んだ形（{"column_width": {...}}）。
    if !obj.contains_key("op") && obj.len() == 1 {
        let key = obj.keys().next().cloned().unwrap_or_default();
        if KNOWN_OPS.contains(&key.as_str()) {
            if let Some(serde_json::Value::Object(inner)) = obj.remove(&key) {
                obj.insert("op".into(), serde_json::Value::String(key));
                for (k, x) in inner {
                    obj.entry(k).or_insert(x);
                }
            }
        }
    }
    let name = obj
        .get("op")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    obj.remove("summary");
    if let Some(serde_json::Value::Object(inner)) = obj.remove(&name) {
        for (k, x) in inner {
            obj.entry(k).or_insert(x);
        }
    }
    if name == "header" || name == "footer" {
        obj.remove("target");
        // 中身（text・align・size）を content の外に書いた形。
        if !obj.contains_key("content") && obj.contains_key("text") {
            let mut content = serde_json::Map::new();
            for k in ["text", "align", "size"] {
                if let Some(x) = obj.remove(k) {
                    content.insert(k.into(), x);
                }
            }
            obj.insert("content".into(), serde_json::Value::Object(content));
        }
    }
    // 改ページの位置を target に書いた形。
    if name == "page_break" && !obj.contains_key("before") {
        if let Some(t) = obj.remove("target") {
            obj.insert("before".into(), t);
        }
    }
    // 切り替えの操作で enabled を書き忘れた形は「有効にする」。
    if (name == "title_page" || name == "track_changes") && !obj.contains_key("enabled") {
        obj.insert("enabled".into(), serde_json::Value::Bool(true));
    }
    // シートを足す操作の名前を sheet に書いた形。
    if name == "add_sheet" && !obj.contains_key("name") {
        if let Some(n) = obj.remove("sheet") {
            obj.insert("name".into(), n);
        }
    }
}

/// Excel・Word の操作名（op を書き忘れて操作名で包んだ形を見分けるため）。
const KNOWN_OPS: &[&str] = &[
    "set_values",
    "fill_formula",
    "format",
    "style_table",
    "merge",
    "unmerge",
    "column_width",
    "row_height",
    "insert_rows",
    "delete_rows",
    "insert_columns",
    "delete_columns",
    "hide_rows",
    "show_rows",
    "hide_columns",
    "show_columns",
    "clear",
    "sort",
    "autofilter",
    "freeze",
    "add_sheet",
    "rename_sheet",
    "delete_sheet",
    "move_sheet",
    "sheet_props",
    "conditional_format",
    "clear_conditional_formats",
    "data_validation",
    "define_name",
    "add_chart",
    "delete_chart",
    "page_setup",
    "insert",
    "replace",
    "delete",
    "replace_text",
    "format_text",
    "format_paragraph",
    "set_style",
    "header",
    "footer",
    "insert_toc",
    "update_toc",
    "page_break",
    "title_page",
    "set_cells",
    "format_cells",
    "table_props",
    "track_changes",
    "accept_all_changes",
    "reject_all_changes",
];

/// 真偽の項目に書かれた 1 / 0 / "true" / "false" を真偽値に直す（入れ子の中も）。
///
/// 弱いモデル（Gemma 4 の評価）は `banded: 1` のように書き、型の誤りで一括ごと弾かれていた。
/// 意味が一意なものだけを直し、それ以外（2 や "maybe"）は型の誤りとして返す。
pub(crate) fn coerce_bools(v: &mut serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            for (k, x) in map.iter_mut() {
                if BOOL_KEYS.contains(&k.as_str()) {
                    let b = match &*x {
                        Value::Number(n) if n.as_f64() == Some(1.0) => Some(true),
                        Value::Number(n) if n.as_f64() == Some(0.0) => Some(false),
                        Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
                        Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
                        _ => None,
                    };
                    if let Some(b) = b {
                        *x = Value::Bool(b);
                        continue;
                    }
                }
                coerce_bools(x);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(coerce_bools),
        _ => {}
    }
}

#[cfg(test)]
mod coerce_tests {
    use super::coerce_bools;
    use serde_json::json;

    #[test]
    fn relaxes_summary_wrapped_contents_and_header_targets() {
        let mut setup = json!({"op": "page_setup", "summary": "x", "page_setup": {"paper": "A4"}});
        super::relax_op(&mut setup);
        assert_eq!(setup, json!({"op": "page_setup", "paper": "A4"}));
        let mut header =
            json!({"op": "header", "target": "b0-0000", "content": {"text": "社外秘"}});
        super::relax_op(&mut header);
        assert!(header.get("target").is_none());
        let mut footer = json!({"op": "footer", "text": "&[Page]", "align": "center"});
        super::relax_op(&mut footer);
        assert_eq!(
            footer["content"],
            json!({"text": "&[Page]", "align": "center"})
        );
        let mut page_break = json!({"op": "page_break", "target": "b3-0000"});
        super::relax_op(&mut page_break);
        assert_eq!(page_break["before"], json!("b3-0000"));
        let mut track = json!({"op": "track_changes"});
        super::relax_op(&mut track);
        assert_eq!(track["enabled"], json!(true));
    }

    #[test]
    fn turns_unambiguous_numbers_and_strings_into_booleans() {
        let mut v = json!({"op": "style_table", "banded": 1, "total_row": "false",
                            "font": {"bold": 0}, "width": 1, "header_rows": 1, "wrap": 2});
        coerce_bools(&mut v);
        assert_eq!(v["banded"], json!(true));
        assert_eq!(v["total_row"], json!(false));
        assert_eq!(v["font"]["bold"], json!(false));
        // 真偽の項目でないもの・意味が一意でないものは触らない。
        assert_eq!(v["width"], json!(1));
        assert_eq!(v["header_rows"], json!(1));
        assert_eq!(v["wrap"], json!(2));
    }
}
