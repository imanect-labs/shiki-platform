//! `data_query` の SQL 入力（#970）: 入力の形の検査・SQL の結果の整形・拒否理由の整形。
//!
//! SQL は data の SQL 前段面（[`data::DataStore::run_sql`]・data-platform §16.1）が DataQuery へ変換して
//! 実行する。ここはツールの入力と観測の形だけを扱う（認可・名前解決は data が本人の権限で行う）。

use data::{DataQuery, QueryRejection, SqlQueryResult, SqlRequest};
use serde_json::json;

use crate::data_tool::{DataQueryToolInput, MAX_OUTPUT_CHARS, MAX_ROWS_SHOWN};

/// 拒否理由に入れる候補の上限（観測を有界に保つ）。
const MAX_EXPECTED_SHOWN: usize = 20;

/// 422 の拒否を、モデルが書き直せる形に（種類・位置・手がかり・候補）。
///
/// 候補（`expected`）は data が閲覧者に見えるものだけを入れて返す（存在しない名前と見えない名前は
/// 同一の応答・data-platform §16.1）ので、そのまま載せてよい。
pub(crate) fn rejection_message(r: &QueryRejection) -> String {
    use std::fmt::Write as _;
    let code = serde_json::to_value(r.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut out = format!("クエリを受け付けられません（{code}）: {}", r.message);
    if let Some(loc) = &r.location {
        let _ = write!(out, "（{} 行 {} 列目）", loc.line, loc.column);
    }
    if let Some(hint) = r.hint.as_deref().filter(|h| !h.trim().is_empty()) {
        let _ = write!(out, "\n手がかり: {hint}");
    }
    if !r.expected.is_empty() {
        let shown: Vec<&str> = r
            .expected
            .iter()
            .take(MAX_EXPECTED_SHOWN)
            .map(String::as_str)
            .collect();
        let more = if r.expected.len() > shown.len() {
            " …"
        } else {
            ""
        };
        let _ = write!(out, "\n使える候補: {}{more}", shown.join(", "));
    }
    out
}

/// SQL の結果をモデル向けに整形する（列名の見出し＋1 行 1 配列・行数と長さは有界）。
pub(crate) fn format_sql_result(table: &str, res: &SqlQueryResult) -> String {
    use std::fmt::Write as _;
    let mut out = format!("テーブル「{table}」への SQL の結果: {} 行", res.rows.len());
    if let Some(count) = res.count {
        let _ = match count {
            data::CountResult::Exact(n) => write!(out, "（全 {n} 件）"),
            data::CountResult::AtLeast(n) => write!(out, "（全 {n} 件以上）"),
        };
    }
    let _ = writeln!(out, "\n列: {}", json!(res.columns));
    let mut omitted = res.rows.len() > MAX_ROWS_SHOWN;
    for row in res.rows.iter().take(MAX_ROWS_SHOWN) {
        let line = json!(row).to_string();
        if out.len() + line.len() > MAX_OUTPUT_CHARS {
            omitted = true;
            break;
        }
        out.push_str(&line);
        out.push('\n');
    }
    if omitted {
        out.push_str("…（一部のみ表示。SELECT の列を絞るか LIMIT を小さくする）\n");
    }
    if res.suppressed {
        out.push_str(
            "※ 件数の少ないグループは値を伏せています（null）。数える・足す行のテーブルを FROM に置いているか確かめる。\n",
        );
    }
    if let Some(cursor) = &res.next_cursor {
        let _ = writeln!(
            out,
            "続きがあります。次のページは同じ sql に cursor \"{cursor}\" を付けて呼ぶ。"
        );
    }
    out
}

/// 入力の組み合わせの検査（どちらの形かを決める）。誤りはモデルが直せる文言で返す。
pub(crate) enum QueryForm {
    Sql(SqlRequest),
    Ir {
        table: String,
        query: Box<DataQuery>,
    },
}

impl DataQueryToolInput {
    pub(crate) fn form(self) -> Result<QueryForm, String> {
        match (self.sql, self.table) {
            (Some(_), _) if self.query.is_some() => Err(
                "sql と query は同時に指定できません。どちらか一方にしてください。".into(),
            ),
            (Some(_), Some(_)) => Err(
                "sql を使うときは table を指定しません（テーブルは FROM に書く）。".into(),
            ),
            (Some(sql), None) => {
                if sql.trim().is_empty() {
                    return Err("sql が空です。SELECT 文を 1 つ指定してください。".into());
                }
                Ok(QueryForm::Sql(SqlRequest {
                    sql,
                    cursor: self.cursor,
                    with_count: self.with_count,
                }))
            }
            (None, _) if self.cursor.is_some() || self.with_count => Err(
                "cursor と with_count（最上位）は sql と組で使います。table ＋ query のときは query の中に書いてください。"
                    .into(),
            ),
            (None, Some(table)) => Ok(QueryForm::Ir {
                table,
                query: Box::new(self.query.unwrap_or_default()),
            }),
            (None, None) => Err(
                "sql（SELECT 文）か table（＋ query）のどちらかを指定してください。".into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_tool::denied;
    use data::DataError;
    use serde_json::Value;
    use uuid::Uuid;

    fn form(v: Value) -> Result<QueryForm, String> {
        serde_json::from_value::<DataQueryToolInput>(v)
            .unwrap()
            .form()
    }

    #[test]
    fn input_takes_either_sql_or_table_with_query() {
        let Ok(QueryForm::Sql(req)) = form(json!({
            "sql": "SELECT title FROM 経費", "cursor": "c1", "with_count": true
        })) else {
            panic!("sql だけなら SQL の形");
        };
        assert_eq!(req.sql, "SELECT title FROM 経費");
        assert_eq!(req.cursor.as_deref(), Some("c1"));
        assert!(req.with_count);
        let Ok(QueryForm::Ir { table, query }) = form(json!({ "table": "経費" })) else {
            panic!("table だけなら既定のクエリ");
        };
        assert_eq!(table, "経費");
        assert_eq!(*query, DataQuery::default());
        // 組み合わせの誤りは、直し方の分かる文言で返す。
        for (input, needle) in [
            (
                json!({ "sql": "SELECT a FROM t", "table": "経費" }),
                "table を指定しません",
            ),
            (
                json!({ "sql": "SELECT a FROM t", "query": {} }),
                "同時に指定できません",
            ),
            (json!({ "sql": "  " }), "sql が空です"),
            (
                json!({ "table": "経費", "cursor": "c1" }),
                "sql と組で使います",
            ),
            (json!({}), "どちらかを指定してください"),
        ] {
            let err = form(input.clone())
                .err()
                .unwrap_or_else(|| panic!("{input}"));
            assert!(err.contains(needle), "{input} → {err}");
        }
    }

    #[test]
    fn sql_result_is_a_bounded_table_with_paging_note() {
        let result = |n: usize, cursor: Option<&str>| SqlQueryResult {
            columns: vec!["title".into(), "amount".into()],
            rows: (0..n)
                .map(|i| vec![json!(format!("申請{i}")), json!(i)])
                .collect(),
            next_cursor: cursor.map(str::to_string),
            count: Some(data::CountResult::Exact(120)),
            suppressed: false,
            shares_truncated: false,
            sort_precision: data::SortPrecision::Exact,
            sort_degraded: false,
            table_id: Uuid::nil(),
            query: DataQuery::default(),
        };
        let out = format_sql_result("経費", &result(2, None));
        assert!(
            out.starts_with("テーブル「経費」への SQL の結果: 2 行（全 120 件）"),
            "{out}"
        );
        assert!(out.contains(r#"列: ["title","amount"]"#), "{out}");
        assert!(out.contains(r#"["申請1",1]"#), "{out}");
        assert!(
            !out.contains("一部のみ表示") && !out.contains("続きがあります"),
            "{out}"
        );
        let over = format_sql_result("経費", &result(MAX_ROWS_SHOWN + 1, Some("next")));
        assert_eq!(
            over.lines().filter(|l| l.starts_with("[\"申請")).count(),
            MAX_ROWS_SHOWN
        );
        assert!(over.contains("一部のみ表示"), "{over}");
        assert!(over.contains(r#"cursor "next""#), "{over}");
    }

    #[test]
    fn rejection_tells_the_model_how_to_rewrite() {
        let r = QueryRejection {
            code: data::RejectCode::UnknownName,
            message: "列 'amout' は見つからないか、参照できません".into(),
            hint: Some("列名は data_schema の参照名".into()),
            location: Some(data::SqlLocation { line: 1, column: 8 }),
            limit: None,
            expected: (0..25).map(|i| format!("col{i}")).collect(),
        };
        let msg = denied(&DataError::Rejected(Box::new(r))).content;
        assert!(
            msg.contains("unknown_name") && msg.contains("amout"),
            "{msg}"
        );
        assert!(msg.contains("1 行 8 列目"), "{msg}");
        assert!(
            msg.contains("手がかり: 列名は data_schema の参照名"),
            "{msg}"
        );
        assert!(
            msg.contains("col19") && !msg.contains("col20") && msg.contains(" …"),
            "{msg}"
        );
    }
}
