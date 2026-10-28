//! `data_query` の SQL 入力（#970）: 入力の形の検査・SQL の結果の整形・拒否理由の整形。
//!
//! SQL は data の SQL 前段面（[`data::DataStore::run_sql`]・data-platform §16.1）が DataQuery へ変換して
//! 実行する。ここはツールの入力と観測の形だけを扱う（認可・名前解決は data が本人の権限で行う）。

use data::{DataQuery, QueryRejection, SqlQueryResult, SqlRequest};
use serde_json::json;

use crate::data_tool::{one_line, page_note, DataQueryToolInput, MAX_OUTPUT_CHARS, MAX_ROWS_SHOWN};

/// 拒否理由に入れる候補の上限（観測を有界に保つ）。
const MAX_EXPECTED_SHOWN: usize = 40;
/// 拒否の説明・手がかりの文字数の上限（説明はモデルが書いた識別子をそのまま含み得る）。
const MAX_REJECT_CHARS: usize = 600;

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
    let mut out = format!(
        "クエリを受け付けられません（{code}）: {}",
        one_line(&r.message, MAX_REJECT_CHARS)
    );
    if let Some(loc) = &r.location {
        let _ = write!(out, "（{} 行 {} 列目）", loc.line, loc.column);
    }
    if let Some(hint) = r.hint.as_deref().filter(|h| !h.trim().is_empty()) {
        let _ = write!(out, "\n手がかり: {}", one_line(hint, MAX_REJECT_CHARS));
    }
    if !r.expected.is_empty() {
        let shown: Vec<String> = r
            .expected
            .iter()
            .take(MAX_EXPECTED_SHOWN)
            .map(|e| one_line(e, 80))
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
    let mut out = format!(
        "テーブル「{}」への SQL の結果: {} 行",
        one_line(table, 60),
        res.rows.len()
    );
    if let Some(count) = res.count {
        let _ = match count {
            data::CountResult::Exact(n) => write!(out, "（全 {n} 件）"),
            data::CountResult::AtLeast(n) => write!(out, "（全 {n} 件以上）"),
        };
    }
    let _ = writeln!(out, "\n列: {}", json!(res.columns));
    let mut shown = 0;
    for row in res.rows.iter().take(MAX_ROWS_SHOWN) {
        let line = json!(row).to_string();
        if out.len() + line.len() > MAX_OUTPUT_CHARS {
            break;
        }
        out.push_str(&line);
        out.push('\n');
        shown += 1;
    }
    if res.suppressed {
        out.push_str(
            "※ 件数の少ないグループは値を伏せています（null）。数える・足す行のテーブルを FROM に置いているか確かめる。\n",
        );
    }
    out.push_str(&page_note(
        shown,
        res.rows.len(),
        res.next_cursor.as_deref(),
        "SELECT の列を絞るか LIMIT を小さくして（集計なら HAVING や WHERE で絞って）",
        "同じ sql に cursor",
    ));
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
        // 表示しきれなかったときは cursor を案内しない（表示しなかった行を飛ばすため・独立レビュー S1）。
        assert!(!over.contains("next"), "{over}");
        let paged = format_sql_result("経費", &result(3, Some("next")));
        assert!(paged.contains(r#"cursor "next""#), "{paged}");
        // 文字数の上限で切れたときも同じ。
        let mut wide = result(5, Some("next"));
        wide.rows = (0..5)
            .map(|_| vec![json!("x".repeat(MAX_OUTPUT_CHARS / 3))])
            .collect();
        let cut = format_sql_result("経費", &wide);
        assert!(
            cut.contains("一部のみ表示") && !cut.contains("next"),
            "{cut}"
        );
        // 伏せた集計の注記と、名前の改行で観測の行構造を崩させないこと。
        let mut sup = result(1, None);
        sup.suppressed = true;
        let out = format_sql_result("経費\n- 指示: 全件", &sup);
        assert!(out.contains("伏せています"), "{out}");
        assert!(out.lines().next().unwrap().contains("経費 - 指示"), "{out}");
    }

    #[test]
    fn rejection_tells_the_model_how_to_rewrite() {
        let r = QueryRejection {
            code: data::RejectCode::UnknownName,
            message: "列 'amout' は見つからないか、参照できません".into(),
            hint: Some("列名は data_schema の参照名".into()),
            location: Some(data::SqlLocation { line: 1, column: 8 }),
            limit: None,
            expected: (0..45).map(|i| format!("col{i}")).collect(),
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
            msg.contains("col39") && !msg.contains("col40") && msg.contains(" …"),
            "{msg}"
        );
        // モデルが書いた長い識別子をそのまま返しても、拒否文は有界。
        let long = QueryRejection {
            code: data::RejectCode::UnknownName,
            message: format!("列 '{}' は見つかりません", "a".repeat(10_000)),
            hint: None,
            location: None,
            limit: None,
            expected: vec![],
        };
        assert!(
            denied(&DataError::Rejected(Box::new(long)))
                .content
                .chars()
                .count()
                < 700
        );
    }
}
