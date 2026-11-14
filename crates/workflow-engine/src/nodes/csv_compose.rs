//! 新しく保存する CSV の内容（解決した値）を、CSV の文章にする（`csv.write`・`storage.write` の CSV・BUG-178）。
//!
//! 文字はそのまま使う（作成者が書いた CSV・これまでと同じ）。表（「CSV を集計」の結果 `{columns, rows}`）と、
//! 項目のまとまりのリストは CSV に組み立てる。ほかの値（数・真偽・空の値・表でない項目のまとまり・まとまりで
//! ないものを含むリスト）は、JSON の文字のまま .csv に保存せずに `invalid` で断る（壊れた CSV を黙って作らない）。
//!
//! 組み立ての決まり（RFC 4180）:
//! - 区切りはカンマ、行の終わりは CRLF（最後の行にも付ける）。
//! - カンマ・二重引用符・改行を含むセルは二重引用符で囲み、中の二重引用符は 2 つ重ねる。
//! - 表計算ソフトで式として動きうるセル（先頭が `=` `+` `-` `@` タブ CR の 2 字以上の文字で、数の表記ではないもの）は、
//!   先頭に `'` を付けて文字にする（監査ログの書き出しと同じ打ち消し方。`-5`・`+3.2` のような数と、`-` のような
//!   1 字だけの値はそのまま）。
//! - セルの値: 文字はそのまま、数はその表記、真偽は `true`／`false`、空の値（null）は空。項目のまとまりとリストは
//!   1 つのセルに入れられないので断る。
//! - 表の行は、値のリスト（列の順・数は列の数と同じ）か、項目のまとまり（列の名前で引く・無い列は空）。
//!   「CSV を集計」が打ち切った表（`truncated: true`）は、残りの行が抜けるので断る。
//! - 項目のまとまりのリストの列は、すべての項目の名前を合わせたもの（名前の順）。無い項目は空。空のリストは
//!   空の CSV（見出しの行も無い）。

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::ports::PortError;

/// 行の終わり（RFC 4180）。
const LINE_END: &str = "\r\n";

/// 内容を CSV の文章にする。`label` は欄の見出し（誤りの文で名指しする・「CSV の内容」など）。
///
/// 文字はそのまま返す（`{ "base64": … }` のバイト列は呼び出し側が先に扱う）。
pub(super) fn csv_text(content: &Value, label: &str) -> Result<String, PortError> {
    match content {
        Value::String(s) => Ok(s.clone()),
        Value::Object(map) if map.contains_key("columns") || map.contains_key("rows") => {
            table_text(map, label)
        }
        Value::Array(items) => list_text(items, label),
        other => Err(PortError::invalid(format!(
            "「{label}」に{}が入っているため、CSV にできません。CSV の文字、「CSV を集計」の結果（表）、項目のまとまりの\
             リストのどれかを入れてください。",
            kind_name(other)
        ))),
    }
}

/// 値の種類の呼び名（誤りの文に使う）。
fn kind_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "空の値",
        Value::Bool(_) => "真偽",
        Value::Number(_) => "数",
        Value::String(_) => "文字",
        Value::Array(_) => "リスト",
        Value::Object(_) => "表でない項目のまとまり",
    }
}

/// 表（`{columns, rows}`）を CSV にする。
fn table_text(map: &Map<String, Value>, label: &str) -> Result<String, PortError> {
    let invalid = |m: String| PortError::invalid(m);
    let columns: Vec<&str> = match map.get("columns") {
        Some(Value::Array(cols)) => cols.iter().map(Value::as_str).collect::<Option<_>>().ok_or_else(|| {
            invalid(format!("「{label}」の表の列の名前（columns）は、文字のリストで書いてください。"))
        })?,
        _ => {
            return Err(invalid(format!(
                "「{label}」の表に列の名前（columns）がありません。列の名前を文字のリストで書いてください。"
            )))
        }
    };
    let Some(Value::Array(rows)) = map.get("rows") else {
        return Err(invalid(format!(
            "「{label}」の表に行（rows）がありません。行をリストで書いてください。"
        )));
    };
    if map.get("truncated") == Some(&Value::Bool(true)) {
        let total = map
            .get("total_rows")
            .and_then(Value::as_u64)
            .map_or_else(String::new, |t| format!("{} 行のうち ", group_digits(t)));
        return Err(invalid(format!(
            "「{label}」の表は「CSV を集計」が {total}{} 行で打ち切ったもので、そのまま保存すると残りの行が抜けます。\
             SQL の条件で行を絞るか、先頭の行だけでよければ SQL に LIMIT を付けてください。",
            group_digits(rows.len() as u64)
        )));
    }
    if columns.is_empty() && !rows.is_empty() {
        return Err(invalid(format!(
            "「{label}」の表に列がありません。列の名前（columns）を書いてください。"
        )));
    }
    let mut out = String::new();
    if !columns.is_empty() {
        push_line(&mut out, columns.iter().map(|c| escape(c)));
    }
    for (i, row) in rows.iter().enumerate() {
        let n = i + 1;
        let at = |col: &str, v: &Value| {
            cell(v).ok_or_else(|| {
                invalid(format!(
                    "「{label}」の表の {n} 行目の「{col}」は{}のため、1 つのセルに入れられません。セルには文字・数・真偽を\
                     入れてください。",
                    cell_kind(v)
                ))
            })
        };
        let cells: Vec<String> = match row {
            Value::Array(values) => {
                if values.len() != columns.len() {
                    return Err(invalid(format!(
                        "「{label}」の表の {n} 行目は値が {} 個で、列の数（{}）と合いません。",
                        values.len(),
                        columns.len()
                    )));
                }
                columns
                    .iter()
                    .zip(values)
                    .map(|(col, v)| at(col, v))
                    .collect::<Result<_, _>>()?
            }
            Value::Object(fields) => columns
                .iter()
                .map(|col| {
                    fields
                        .get(*col)
                        .map_or_else(|| Ok(String::new()), |v| at(col, v))
                })
                .collect::<Result<_, _>>()?,
            _ => {
                return Err(invalid(format!(
                    "「{label}」の表の {n} 行目は、値のリストか項目のまとまりで書いてください。"
                )))
            }
        };
        push_line(&mut out, cells.into_iter());
    }
    Ok(out)
}

/// 項目のまとまりのリストを CSV にする（列はすべての項目の名前を合わせたもの・名前の順）。
fn list_text(items: &[Value], label: &str) -> Result<String, PortError> {
    let mut records = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let Value::Object(fields) = item else {
            return Err(PortError::invalid(format!(
                "「{label}」のリストの {} 件目が項目のまとまりではありません。CSV にするリストには、1 行ごとに項目の\
                 まとまりを並べてください。",
                i + 1
            )));
        };
        records.push(fields);
    }
    let columns: BTreeSet<&str> = records
        .iter()
        .flat_map(|r| r.keys().map(String::as_str))
        .collect();
    let mut out = String::new();
    if columns.is_empty() {
        return Ok(out);
    }
    push_line(&mut out, columns.iter().map(|c| escape(c)));
    for (i, fields) in records.iter().enumerate() {
        let cells = columns
            .iter()
            .map(|col| match fields.get(*col) {
                None => Ok(String::new()),
                Some(v) => cell(v).ok_or_else(|| {
                    PortError::invalid(format!(
                        "「{label}」のリストの {} 件目の「{col}」は{}のため、1 つのセルに入れられません。セルには文字・数・\
                         真偽を入れてください。",
                        i + 1,
                        cell_kind(v)
                    ))
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        push_line(&mut out, cells.into_iter());
    }
    Ok(out)
}

/// 1 行を書く（セルはエスケープ済み）。
fn push_line(out: &mut String, cells: impl Iterator<Item = String>) {
    for (j, c) in cells.enumerate() {
        if j > 0 {
            out.push(',');
        }
        out.push_str(&c);
    }
    out.push_str(LINE_END);
}

/// セルに入れられない値の呼び名（誤りの文に使う）。
fn cell_kind(v: &Value) -> &'static str {
    if v.is_array() {
        "リスト"
    } else {
        "項目のまとまり"
    }
}

/// セルの値をエスケープ済みの文字にする（項目のまとまりとリストは `None`）。
fn cell(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some(String::new()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(escape(s)),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// 文字のセルを、式の打ち消しと RFC 4180 の引用符を付けた形にする。
fn escape(s: &str) -> String {
    let text = if looks_like_formula(s) {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

/// 表計算ソフトが式として読みうる文字か（先頭が = + - @ タブ CR の 2 字以上で、数の表記ではない）。
fn looks_like_formula(s: &str) -> bool {
    s.chars().nth(1).is_some()
        && s.starts_with(['=', '+', '-', '@', '\t', '\r'])
        && !is_plain_number(s)
}

/// 数の表記か（符号・数字・小数点・指数だけ。例: -5、+3.2、1e3）。
fn is_plain_number(s: &str) -> bool {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = (!int.is_empty() || !frac.is_empty()) && digits(int) && digits(frac);
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    mantissa_ok && exponent_ok
}

/// 3 桁ごとにカンマを入れる（1,234）。
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    const L: &str = "CSV の内容";

    fn err(v: &Value) -> String {
        let e = csv_text(v, L).unwrap_err();
        assert_eq!(e.code, "invalid");
        assert!(!e.retryable);
        e.message
    }

    #[test]
    fn keeps_text_as_it_is() {
        // 文字は作成者が書いた CSV のまま（改行も式の打ち消しも変えない・保存済みの定義の挙動）。
        let text = "品目,金額\n=1+1,1200\n";
        assert_eq!(csv_text(&json!(text), L).unwrap(), text);
    }

    #[test]
    fn builds_csv_from_a_table() {
        // 「CSV を集計」の結果の形（値はすべて文字・空は null・ほかの項目は使わない）。
        let table = json!({
            "columns": ["品目", "金額", "備考"],
            "column_types": ["VARCHAR", "BIGINT", "VARCHAR"],
            "rows": [["交通費", "1200", null], ["会議費, 軽食", "-300", "\"至急\"\n2 行目"]],
            "total_rows": 2, "truncated": false, "version": 3
        });
        assert_eq!(
            csv_text(&table, L).unwrap(),
            "品目,金額,備考\r\n交通費,1200,\r\n\"会議費, 軽食\",-300,\"\"\"至急\"\"\n2 行目\"\r\n"
        );
        // 行は項目のまとまりでも書ける（列の名前で引く・無い列は空・列に無い項目は使わない）。
        let by_name = json!({
            "columns": ["金額", "品目"],
            "rows": [{ "品目": "交通費", "金額": 1200, "メモ": "x" }, { "品目": "備品" }]
        });
        assert_eq!(
            csv_text(&by_name, L).unwrap(),
            "金額,品目\r\n1200,交通費\r\n,備品\r\n"
        );
        // 行の無い表は見出しの行だけ。列も行も無い表は空。
        assert_eq!(
            csv_text(&json!({ "columns": ["a"], "rows": [] }), L).unwrap(),
            "a\r\n"
        );
        assert_eq!(
            csv_text(&json!({ "columns": [], "rows": [] }), L).unwrap(),
            ""
        );
    }

    #[test]
    fn builds_csv_from_a_list_of_records() {
        // 列はすべての項目の名前を合わせたもの（名前の順）。数・真偽・空の値もセルにする。
        let list = json!([
            { "name": "佐藤", "amount": 1200, "paid": true },
            { "name": "鈴木", "note": "a,b", "paid": null }
        ]);
        assert_eq!(
            csv_text(&list, L).unwrap(),
            "amount,name,note,paid\r\n1200,佐藤,,true\r\n,鈴木,\"a,b\",\r\n"
        );
        assert_eq!(csv_text(&json!([]), L).unwrap(), "");
    }

    #[test]
    fn neutralizes_cells_that_look_like_formulas() {
        let table = json!({
            "columns": ["v"],
            "rows": [["=SUM(A1:A9)"], ["+81-3-1234"], ["@cmd"], ["\tx"], ["-5"], ["+3.2"], ["-1e3"], ["-"], ["ok"], ["-1,2"]]
        });
        assert_eq!(
            csv_text(&table, L).unwrap(),
            "v\r\n'=SUM(A1:A9)\r\n'+81-3-1234\r\n'@cmd\r\n'\tx\r\n-5\r\n+3.2\r\n-1e3\r\n-\r\nok\r\n\"'-1,2\"\r\n"
        );
        // 見出しも同じ決まりで書く。
        assert_eq!(csv_text(&json!([{ "=x": 1 }]), L).unwrap(), "'=x\r\n1\r\n");
    }

    #[test]
    fn refuses_values_that_cannot_be_csv() {
        for (v, kind) in [
            (json!(12), "数"),
            (json!(true), "真偽"),
            (json!(null), "空の値"),
            (json!({ "a": 1 }), "表でない項目のまとまり"),
        ] {
            assert_eq!(
                err(&v),
                format!(
                    "「CSV の内容」に{kind}が入っているため、CSV にできません。CSV の文字、「CSV を集計」の結果（表）、\
                     項目のまとまりのリストのどれかを入れてください。"
                )
            );
        }
        assert!(err(&json!(["a", "b"]))
            .starts_with("「CSV の内容」のリストの 1 件目が項目のまとまりではありません。"));
        assert!(err(&json!([{ "a": 1 }, { "a": { "b": 2 } }]))
            .starts_with("「CSV の内容」のリストの 2 件目の「a」は項目のまとまりのため、1 つのセルに入れられません。"));
        assert!(err(&json!({ "rows": [] }))
            .starts_with("「CSV の内容」の表に列の名前（columns）がありません。"));
        assert!(
            err(&json!({ "columns": [1], "rows": [] })).contains("文字のリストで書いてください")
        );
        assert!(err(&json!({ "columns": ["a"] }))
            .starts_with("「CSV の内容」の表に行（rows）がありません。"));
        assert_eq!(
            err(&json!({ "columns": ["a", "b"], "rows": [["1", "2"], ["3"]] })),
            "「CSV の内容」の表の 2 行目は値が 1 個で、列の数（2）と合いません。"
        );
        assert!(
            err(&json!({ "columns": ["a"], "rows": [["1"], "x"] })).starts_with(
                "「CSV の内容」の表の 2 行目は、値のリストか項目のまとまりで書いてください。"
            )
        );
        assert!(err(&json!({ "columns": ["a"], "rows": [[["x"]]] }))
            .starts_with("「CSV の内容」の表の 1 行目の「a」はリストのため"));
        assert!(err(&json!({ "columns": [], "rows": [[]] }))
            .starts_with("「CSV の内容」の表に列がありません。"));
    }

    #[test]
    fn refuses_tables_cut_by_csv_query() {
        // 「CSV を集計」が打ち切った表は、残りの行が抜けるので保存しない。
        let cut = json!({ "columns": ["a"], "rows": [["1"], ["2"]], "total_rows": 12_345, "truncated": true });
        assert_eq!(
            err(&cut),
            "「CSV の内容」の表は「CSV を集計」が 12,345 行のうち 2 行で打ち切ったもので、そのまま保存すると残りの行が\
             抜けます。SQL の条件で行を絞るか、先頭の行だけでよければ SQL に LIMIT を付けてください。"
        );
        let cut = json!({ "columns": ["a"], "rows": [["1"]], "truncated": true });
        assert!(
            err(&cut).starts_with("「CSV の内容」の表は「CSV を集計」が 1 行で打ち切ったもので")
        );
    }

    #[test]
    fn plain_numbers_and_digit_groups() {
        for s in ["-5", "+3.2", "1e3", "-1.5E-3", ".5", "5."] {
            assert!(is_plain_number(s), "{s}");
        }
        for s in ["-", "+", "-e3", "1e", "1-2", "-1,2", "--5", "-.", "-5円"] {
            assert!(!is_plain_number(s), "{s}");
        }
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }
}
