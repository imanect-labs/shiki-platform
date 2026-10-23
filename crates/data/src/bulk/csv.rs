//! CSV → 取込行（HTTP の一括取込の入口・監査 B2-6）。
//!
//! CSV は**形の変換だけ**を行い、型・必須・選択肢・unique・参照の検証は通常の書込と同じ検証器
//! （ジョブの ② 検証段）に任せる。変換できないセル（数値の列の非数値など）は文字列のまま渡し、
//! 検証器がその行を「不合格」にする（行エラーとして観測できる・取込全体は止めない）。
//!
//! | 規則 | 内容 |
//! |---|---|
//! | 文字コード | UTF-8（先頭の BOM は読み飛ばす）。それ以外は 400 |
//! | 見出し行 | 必須。列はフィールドの参照名（`key`）で、無ければ表示名（`display_name`・一意なもの）で照合 |
//! | 行キー | 見出し [`ROW_KEY_COLUMN`]（`_key`）の列があれば行キー（取込内で一意・エラー報告で行を特定する） |
//! | 空のセル | そのフィールドを持たない（値なし） |
//! | number | 前後の空白を除いて JSON の数値として読む（読めなければ文字列のまま → 検証で不合格） |
//! | multi_select / record_ref | `[` で始まれば JSON 配列、それ以外は `;` 区切り（各要素の前後の空白を除く） |
//! | その他 | セルの文字列そのまま |
//!
//! 未知の列・派生列（書込不可）・同じフィールドへの重複した列は取込全体を 400 にする（入力の取り違えを
//! 黙って捨てない）。行数は [`super::MAX_BULK_ROWS`] まで。

use std::collections::HashMap;

use serde_json::{Map, Number, Value};

use super::{BulkImportRow, MAX_BULK_ROWS};
use crate::model::{FieldDef, FieldType, TableSchema};
use crate::DataError;

/// 行キーの列の見出し。
pub const ROW_KEY_COLUMN: &str = "_key";
/// HTTP で受け付ける CSV の最大バイト数。
pub const MAX_BULK_CSV_BYTES: usize = 32 * 1024 * 1024;

/// 見出しの列 1 つの行き先。
enum Column<'a> {
    Key,
    Field(&'a FieldDef),
}

fn invalid(msg: impl Into<String>) -> DataError {
    DataError::Invalid(msg.into())
}

/// 見出しをフィールドへ照合する（参照名 → 一意な表示名の順）。
fn columns<'a>(schema: &'a TableSchema, headers: &[String]) -> Result<Vec<Column<'a>>, DataError> {
    let mut by_display: HashMap<&str, Vec<&FieldDef>> = HashMap::new();
    for f in &schema.fields {
        by_display
            .entry(f.display_name.as_str())
            .or_default()
            .push(f);
    }
    let mut out = Vec::with_capacity(headers.len());
    let mut seen_key = false;
    let mut seen: Vec<crate::model::FieldId> = Vec::new();
    for h in headers {
        let h = h.trim();
        if h == ROW_KEY_COLUMN {
            if seen_key {
                return Err(invalid(format!("列 '{ROW_KEY_COLUMN}' が重複しています")));
            }
            seen_key = true;
            out.push(Column::Key);
            continue;
        }
        let f = match schema.field(h) {
            Some(f) => f,
            None => match by_display.get(h).map(Vec::as_slice) {
                Some([f]) => *f,
                Some(_) => {
                    return Err(invalid(format!(
                        "列 '{h}' は表示名が重複しています（参照名で指定してください）"
                    )))
                }
                None => return Err(invalid(format!("列 '{h}' はテーブルにありません"))),
            },
        };
        if f.field_type.is_derived() || f.is_reverse_link() {
            return Err(invalid(format!("列 '{h}' は書き込めません（派生列）")));
        }
        if seen.contains(&f.id) {
            return Err(invalid(format!(
                "フィールド '{}' の列が重複しています",
                f.key
            )));
        }
        seen.push(f.id);
        out.push(Column::Field(f));
    }
    if seen.is_empty() {
        return Err(invalid("取り込むフィールドの列がありません"));
    }
    Ok(out)
}

/// セル 1 つを外部形の値へ（空は `None`＝値なし）。
fn cell(f: &FieldDef, s: &str) -> Option<Value> {
    if s.is_empty() {
        return None;
    }
    Some(match f.field_type {
        FieldType::Number => serde_json::from_str::<Number>(s.trim())
            .map_or_else(|_| Value::String(s.to_string()), Value::Number),
        FieldType::MultiSelect | FieldType::RecordRef => {
            let t = s.trim();
            if t.starts_with('[') {
                serde_json::from_str::<Value>(t)
                    .ok()
                    .filter(Value::is_array)
                    .unwrap_or_else(|| Value::String(s.to_string()))
            } else {
                Value::Array(
                    t.split(';')
                        .map(str::trim)
                        .filter(|x| !x.is_empty())
                        .map(|x| Value::String(x.to_string()))
                        .collect(),
                )
            }
        }
        _ => Value::String(s.to_string()),
    })
}

/// CSV を取込行にする（形の変換だけ・検証はジョブ）。
pub(crate) fn csv_rows(
    schema: &TableSchema,
    bytes: &[u8],
) -> Result<Vec<BulkImportRow>, DataError> {
    if bytes.len() > MAX_BULK_CSV_BYTES {
        return Err(invalid(format!(
            "CSV が大きすぎます（最大 {MAX_BULK_CSV_BYTES} バイト）"
        )));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if std::str::from_utf8(bytes).is_err() {
        return Err(invalid("CSV は UTF-8 で保存してください"));
    }
    let mut rd = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(bytes);
    let headers: Vec<String> = rd
        .headers()
        .map_err(|e| invalid(format!("CSV の見出し行を読めません: {e}")))?
        .iter()
        .map(str::to_string)
        .collect();
    let cols = columns(schema, &headers)?;
    let mut out = Vec::new();
    for (i, rec) in rd.records().enumerate() {
        // 見出しが 1 行目なので、データの i 行目はファイルの i + 2 行目。
        let line = i + 2;
        let rec = rec.map_err(|e| invalid(format!("CSV の {line} 行目を読めません: {e}")))?;
        if out.len() >= MAX_BULK_ROWS {
            return Err(invalid(format!(
                "1 回に取り込める行は {MAX_BULK_ROWS} 行までです"
            )));
        }
        let mut data = Map::new();
        let mut key = None;
        for (c, s) in cols.iter().zip(rec.iter()) {
            match c {
                Column::Key => key = (!s.is_empty()).then(|| s.to_string()),
                Column::Field(f) => {
                    if let Some(v) = cell(f, s) {
                        data.insert(f.key.clone(), v);
                    }
                }
            }
        }
        out.push(BulkImportRow {
            key,
            data: Value::Object(data),
        });
    }
    if out.is_empty() {
        return Err(invalid("取り込む行がありません"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use serde_json::json;

    fn schema() -> TableSchema {
        let mut title = FieldDef::new("title", FieldType::Text);
        title.display_name = "件名".into();
        let amount = FieldDef::new("amount", FieldType::Number);
        let mut tags = FieldDef::new("tags", FieldType::MultiSelect);
        tags.options = vec!["a".into(), "b".into()];
        let mut f = FieldDef::new("total", FieldType::Formula);
        f.display_name = "合計".into();
        let mut s = TableSchema {
            fields: vec![title, amount, tags, f],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        s
    }

    #[test]
    fn converts_cells_by_field_type() {
        let csv = "\u{feff}_key,件名,amount,tags\nk1,出張,\"1,000\",a;b\nk2,,12.50,\"[\"\"b\"\"]\"\n,宿泊,,\n";
        let rows = csv_rows(&schema(), csv.as_bytes()).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].key.as_deref(), Some("k1"));
        assert_eq!(
            rows[0].data,
            json!({"title": "出張", "amount": "1,000", "tags": ["a", "b"]}),
            "読めない数値は文字列のまま（検証で不合格になる）"
        );
        assert_eq!(rows[1].data, json!({"amount": 12.50, "tags": ["b"]}));
        assert_eq!(rows[2].key, None);
        assert_eq!(rows[2].data, json!({"title": "宿泊"}), "空のセルは値なし");
    }

    #[test]
    fn rejects_unknown_derived_duplicate_and_malformed_input() {
        let s = schema();
        let err = |c: &str| match csv_rows(&s, c.as_bytes()) {
            Err(DataError::Invalid(m)) => m,
            other => panic!("{other:?}"),
        };
        assert!(err("title,nope\nx,y\n").contains("nope"));
        assert!(err("title,合計\nx,1\n").contains("派生列"));
        assert!(err("title,件名\nx,y\n").contains("重複"));
        assert!(err("_key\nk\n").contains("フィールドの列がありません"));
        assert!(err("title\n").contains("行がありません"));
        assert!(err("title,amount\nx\n").contains("2 行目"));
        assert!(matches!(
            csv_rows(&s, b"title\n\xff\xfe\n"),
            Err(DataError::Invalid(m)) if m.contains("UTF-8")
        ));
    }
}
