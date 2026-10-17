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
pub mod formula;
pub mod script;
mod service;
pub mod writer;

pub use service::{
    OfficeEngine, OfficeEngineError, SheetEditReport, SheetReadRequest, DOC_CONTENT_TYPES,
    SHEET_CONTENT_TYPES,
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
