//! 表を読んで応答の行にする（集計の結果・グリッドのページ）。
//!
//! 集計（Query）の結果は、利用者の SQL を包んで列の型を先に読み（`DESCRIBE`）、DuckDB の型のうち JSON の値に
//! そのまま写せない型（日付・時刻・リスト・構造体など）は DuckDB の文字の形にしてから読む。以前は Rust の
//! デバッグ表示（`Date32(19753)` など）を返していた（GAP-40）。
//!
//! 行は 2 つの形で返す。
//! - `rows`: 文字の形（以前からの形・空は None）。数は Rust の表記（`1200`・`0.5`）、真偽は `true`／`false`。
//! - `values`: 値の種類を保った形（Query のときだけ）。数・真偽・空はそのまま、ほかは `rows` と同じ文字。
//!   JavaScript で正確に扱えない大きな整数（±(2^53-1) を超える）と、無限大・非数は文字にする。

use duckdb::types::ValueRef;
use duckdb::{Connection, Row};
use serde_json::{Number, Value};
use tabular::RunnerResponse;

/// JavaScript で正確に扱える最大の整数（`Number.MAX_SAFE_INTEGER`）。
const MAX_SAFE_INTEGER: i128 = (1 << 53) - 1;

/// 値をそのまま読む DuckDB の型か（ほかは文字の形にしてから読む）。
fn reads_as_is(column_type: &str) -> bool {
    matches!(
        column_type,
        "BOOLEAN"
            | "TINYINT"
            | "SMALLINT"
            | "INTEGER"
            | "BIGINT"
            | "HUGEINT"
            | "UTINYINT"
            | "USMALLINT"
            | "UINTEGER"
            | "UBIGINT"
            | "FLOAT"
            | "DOUBLE"
            | "VARCHAR"
    ) || column_type.starts_with("DECIMAL(")
}

/// 末尾のセミコロンと空白を外す（包んだ SQL の中に `;` を残さない）。
fn without_trailing_semicolons(sql: &str) -> &str {
    let mut s = sql.trim_end();
    while let Some(rest) = s.strip_suffix(';') {
        s = rest.trim_end();
    }
    s
}

/// 利用者の SQL（api で検証済みの 1 つの SELECT）を流し、結果を `max_rows` 行まで返す。
///
/// SQL は改行で挟んで包む（末尾の `--` の注釈が包みの閉じ括弧を消さないように）。列は位置で名前を付け直して
/// 読む（同じ名前の列があっても混ざらない）。応答の列の名前と型は、包む前の結果のもの。
pub fn query(conn: &Connection, sql: &str, max_rows: u32) -> Result<RunnerResponse, String> {
    let sql = without_trailing_semicolons(sql);
    let (columns, types) = describe(conn, &format!("DESCRIBE SELECT * FROM (\n{sql}\n) AS q"))?;
    let aliases: Vec<String> = (0..columns.len()).map(|i| format!("c{i}")).collect();
    let selected: Vec<String> = types
        .iter()
        .zip(&aliases)
        .map(|(ty, alias)| {
            if reads_as_is(ty) {
                alias.clone()
            } else {
                format!("CAST({alias} AS VARCHAR)")
            }
        })
        .collect();
    let wrapped = format!(
        "SELECT {} FROM (\n{sql}\n) AS q({}) LIMIT {}",
        selected.join(", "),
        aliases.join(", "),
        u64::from(max_rows).saturating_add(1)
    );
    let mut resp = read_rows(conn, &wrapped, max_rows, true)?;
    resp.columns = columns;
    resp.column_types = types;
    if !resp.truncated {
        resp.total_rows = Some(resp.rows.len() as u64);
    }
    Ok(resp)
}

/// `DESCRIBE …` の列の名前と型。
pub fn describe(conn: &Connection, sql: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut r = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = r.next().map_err(|e| e.to_string())? {
        // DESCRIBE: column_name, column_type, ...
        names.push(row.get::<_, String>(0).map_err(|e| e.to_string())?);
        types.push(row.get::<_, String>(1).unwrap_or_default());
    }
    Ok((names, types))
}

/// SELECT を流し、`max_rows` 行まで読む（超えたら `truncated`）。`typed` なら `values` も作る。
pub fn read_rows(
    conn: &Connection,
    sql: &str,
    max_rows: u32,
    typed: bool,
) -> Result<RunnerResponse, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let mut r = stmt.query([]).map_err(|e| e.to_string())?;
    // 列名は**実行後**に取れる（query() 前だと DuckDB が None を返す）。Rows 経由で取得。
    let columns: Vec<String> = r
        .as_ref()
        .map(|s| s.column_names().iter().map(ToString::to_string).collect())
        .unwrap_or_default();
    let ncols = columns.len();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut values: Vec<Vec<Value>> = Vec::new();
    let mut truncated = false;
    while let Some(row) = r.next().map_err(|e| e.to_string())? {
        if rows.len() as u32 >= max_rows {
            truncated = true;
            break;
        }
        let mut texts = Vec::with_capacity(ncols);
        let mut typed_cells = Vec::with_capacity(if typed { ncols } else { 0 });
        for i in 0..ncols {
            let (text, value) = cell(row, i);
            texts.push(text);
            if typed {
                typed_cells.push(value);
            }
        }
        rows.push(texts);
        if typed {
            values.push(typed_cells);
        }
    }
    Ok(RunnerResponse {
        ok: true,
        columns,
        rows,
        values,
        truncated,
        ..RunnerResponse::default()
    })
}

/// セルを、文字の形と値の種類を保った形にする（NULL は None と null）。
fn cell(row: &Row<'_>, i: usize) -> (Option<String>, Value) {
    match row.get_ref(i) {
        Ok(ValueRef::Null) | Err(_) => (None, Value::Null),
        Ok(ValueRef::Boolean(b)) => (Some(b.to_string()), Value::Bool(b)),
        Ok(ValueRef::TinyInt(n)) => integer(i128::from(n)),
        Ok(ValueRef::SmallInt(n)) => integer(i128::from(n)),
        Ok(ValueRef::Int(n)) => integer(i128::from(n)),
        Ok(ValueRef::BigInt(n)) => integer(i128::from(n)),
        Ok(ValueRef::HugeInt(n)) => integer(n),
        Ok(ValueRef::UTinyInt(n)) => integer(i128::from(n)),
        Ok(ValueRef::USmallInt(n)) => integer(i128::from(n)),
        Ok(ValueRef::UInt(n)) => integer(i128::from(n)),
        Ok(ValueRef::UBigInt(n)) => integer(i128::from(n)),
        Ok(ValueRef::Float(f)) => {
            let text = f.to_string();
            // f32 をそのまま f64 にすると 0.1 が 0.10000000149011612 になるので、表記から読み直す。
            let value = number(text.parse().unwrap_or(f64::from(f)));
            (Some(text), value)
        }
        Ok(ValueRef::Double(f)) => (Some(f.to_string()), number(f)),
        Ok(ValueRef::Decimal(d)) => {
            // 文字は桁を保つ（1.50）。値は数（1.5）。
            let text = d.to_string();
            let value = text
                .parse()
                .map_or_else(|_| Value::String(text.clone()), number);
            (Some(text), value)
        }
        Ok(ValueRef::Text(bytes)) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            (Some(text.clone()), Value::String(text))
        }
        // 文字の形にしてから読むので来ない（来たら以前と同じ表示の文字にする）。
        Ok(other) => {
            let text = format!("{other:?}");
            (Some(text.clone()), Value::String(text))
        }
    }
}

/// 整数（JavaScript で正確に扱えない大きさは文字）。
fn integer(n: i128) -> (Option<String>, Value) {
    let text = n.to_string();
    let value = match i64::try_from(n) {
        Ok(v) if n.abs() <= MAX_SAFE_INTEGER => Value::Number(Number::from(v)),
        _ => Value::String(text.clone()),
    };
    (Some(text), value)
}

/// 小数（整数の値は整数の数にする・無限大と非数は文字）。
fn number(f: f64) -> Value {
    if !f.is_finite() {
        return Value::String(f.to_string());
    }
    if f.fract() == 0.0 && f.abs() <= MAX_SAFE_INTEGER as f64 {
        // 1200.0 を 1200 にする（差し込んだ文に 1200.0 と出さない）。範囲は上で確かめた。
        return Value::Number(Number::from(f as i64));
    }
    Number::from_f64(f).map_or_else(|| Value::String(f.to_string()), Value::Number)
}
