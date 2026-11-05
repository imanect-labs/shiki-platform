//! Excel（.xlsx）を表として読み、DuckDB の `data` に入れる（CSV を集計の読み取りだけ・D-17）。
//!
//! アップロードされた Excel は敵対的な入力として扱う。上限（`tabular::input` の定数）は次の順に確かめる。
//!
//! 1. ファイルの大きさ・zip の中の数。中のファイルを 1 つずつ実際に展開して数え、1 つずつと合わせた大きさの上限を
//!    超えたら読まない（zip が書く大きさは信じない・zip 爆弾）。展開は deflate だけ（ほかの圧縮・暗号は読まない）。
//! 2. calamine でブックを開き、左端のシート（隠したシートは飛ばす）を 1 セルずつ読む。値の数・列の幅・値の文字の
//!    合計を数え、上限を超えたら止める（シートの「範囲」の宣言は使わない。離れた 2 つのセルで巨大な表を確保しない）。
//!
//! 読み方:
//! - 先頭の値のある行を見出し（列の名前）にする。名前の決め方は DuckDB の CSV の読み込みと同じ（前後の空白を
//!   外す・空は `column{位置}`・大文字小文字を区別せずに重なれば `_1` から番号を付ける）。
//! - 列は値のある左端から右端まで。値の無い行は飛ばす。
//! - 数式は計算しない。Excel が保存したときの値を読む（値の無い数式のセルは空）。外部のブックへの参照はたどらない。
//! - 数は Excel と同じ 15 桁に丸める（0.1+0.2 を 0.3 にする）。日付は `2024-01-31`、日時は `2024-01-31 09:30:00`、
//!   時刻（1 日に満たない値）は `09:30:00`。真偽は `TRUE`／`FALSE`、誤りの値は `#DIV/0!` のような文字。
//! - 集計（Query）は列ごとにセルの種類から型を決める: 数だけなら BIGINT（すべて整数のとき）か DOUBLE、真偽だけなら
//!   BOOLEAN、日付だけなら DATE、時刻だけなら TIME、日付と日時なら TIMESTAMP、ほかは VARCHAR。グリッド（Schema・
//!   Rows）は全列を文字で入れる（CSV と同じ）。

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};

use calamine::{DataRef, Reader, SheetType, SheetVisible, Xlsx, XlsxError};
use duckdb::{appender_params_from_iter, Connection};
use tabular::input::{
    file_too_large, MAX_CELLS, MAX_COLUMNS, MAX_ENTRIES, MAX_ENTRY_UNPACKED_BYTES, MAX_FILE_BYTES,
    MAX_TEXT_BYTES, MAX_UNPACKED_BYTES,
};

/// Excel を読めなかった理由（利用者向けの文言・ランナーは `input_error` で返す）。
#[derive(Debug)]
pub struct Unreadable(pub String);

/// 読めない・壊れた Excel。
const BROKEN: &str =
    "Excel のファイル（.xlsx）として読めませんでした。ファイルが壊れていないか確かめてください。";

/// パスワードを付けた Excel。
const PASSWORD: &str =
    "パスワードを付けた Excel のファイルは読めません。パスワードを外して保存し直してください。";

/// 展開した大きさ・中の数が上限を超えた Excel。
fn unpacked_too_large() -> Unreadable {
    Unreadable(format!(
        "Excel のファイルの中身が大きすぎて読めません（展開すると {} MB を超えます）。使う行と列だけを CSV に保存して選んでください。",
        MAX_UNPACKED_BYTES >> 20
    ))
}

/// セルの値（文字の形を決める前）。
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Number(f64),
    Bool(bool),
    /// 日付・時刻（文字の形と種類）。
    Date(String, DateKind),
    Text(String),
}

/// 日付・時刻の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateKind {
    Date,
    Time,
    DateTime,
}

/// 読んだシート（見出しと、値のある行）。
#[derive(Debug, Default)]
struct Sheet {
    /// 列の名前（DuckDB の CSV の読み込みと同じ決め方）。
    columns: Vec<String>,
    /// 集計で読むときの列の型（DuckDB の型の名前）。
    types: Vec<&'static str>,
    /// 行（列の数だけの文字の形・空は None）。
    rows: Vec<Vec<Option<String>>>,
}

/// Excel の左端のシートを読み、`data` を作る（`typed` なら列に型を付ける）。返り値は集計で読むときの列の型。
pub fn load(conn: &Connection, path: &str, typed: bool) -> Result<Vec<String>, LoadError> {
    check_archive(path)?;
    let sheet = read_first_sheet(path)?;
    create_table(conn, &sheet, typed).map_err(LoadError::Internal)?;
    Ok(sheet.types.iter().map(|t| (*t).to_string()).collect())
}

/// 読み込みの失敗（利用者が直す誤りと、ランナーの異常）。
#[derive(Debug)]
pub enum LoadError {
    Input(Unreadable),
    Internal(String),
}

impl From<Unreadable> for LoadError {
    fn from(e: Unreadable) -> Self {
        LoadError::Input(e)
    }
}

/// zip の中の数と、実際に展開した大きさを確かめる（zip 爆弾）。
fn check_archive(path: &str) -> Result<(), Unreadable> {
    let file = File::open(path).map_err(|_| Unreadable(BROKEN.into()))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
    if len > MAX_FILE_BYTES {
        return Err(Unreadable(file_too_large()));
    }
    let mut archive =
        zip::ZipArchive::new(BufReader::new(file)).map_err(|_| Unreadable(BROKEN.into()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(unpacked_too_large());
    }
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|_| Unreadable(BROKEN.into()))?;
        if entry.is_dir() {
            continue;
        }
        // 1 つの上限か、合わせた上限の残りの小さいほうまで展開して数える（上限 + 1 バイト読めたら超えている）。
        let budget = MAX_ENTRY_UNPACKED_BYTES.min(MAX_UNPACKED_BYTES - total);
        let read = std::io::copy(&mut (&mut entry).take(budget + 1), &mut std::io::sink())
            .map_err(|_| Unreadable(BROKEN.into()))?;
        if read > budget {
            return Err(unpacked_too_large());
        }
        total += read;
    }
    Ok(())
}

/// 左端のシート（隠したシートは飛ばす）を読む。
fn read_first_sheet(path: &str) -> Result<Sheet, Unreadable> {
    let file = File::open(path).map_err(|_| Unreadable(BROKEN.into()))?;
    let mut book: Xlsx<_> = Xlsx::new(BufReader::new(file)).map_err(|e| match e {
        XlsxError::Password => Unreadable(PASSWORD.into()),
        _ => Unreadable(BROKEN.into()),
    })?;
    let sheets = book.sheets_metadata();
    let worksheets: Vec<_> = sheets
        .iter()
        .filter(|s| s.typ == SheetType::WorkSheet)
        .collect();
    let Some(sheet) = worksheets
        .iter()
        .find(|s| s.visible == SheetVisible::Visible)
        .or(worksheets.first())
    else {
        return Err(Unreadable(
            "Excel のファイルに表のシートがありません。".into(),
        ));
    };
    let name = sheet.name.clone();
    let mut reader = book
        .worksheet_cells_reader(&name)
        .map_err(|_| Unreadable(BROKEN.into()))?;
    let mut cells: Vec<(u32, u32, Value)> = Vec::new();
    let mut text_bytes = 0usize;
    let (mut min_col, mut max_col) = (u32::MAX, 0u32);
    while let Some(cell) = reader.next_cell().map_err(|_| Unreadable(BROKEN.into()))? {
        let Some(value) = value_of(cell.get_value()) else {
            continue;
        };
        if cells.len() >= MAX_CELLS {
            return Err(Unreadable(format!(
                "Excel のシート「{name}」の値が多すぎます（{} 個まで）。使う行と列だけを CSV に保存して選んでください。",
                group_digits(MAX_CELLS)
            )));
        }
        text_bytes += match &value {
            Value::Text(s) | Value::Date(s, _) => s.len(),
            _ => 0,
        };
        if text_bytes > MAX_TEXT_BYTES {
            return Err(Unreadable(format!(
                "Excel のシート「{name}」の文字が多すぎて読めません（合わせて {} MB まで）。使う行と列だけを CSV に保存して選んでください。",
                MAX_TEXT_BYTES >> 20
            )));
        }
        let (row, col) = cell.get_position();
        min_col = min_col.min(col);
        max_col = max_col.max(col);
        if max_col - min_col >= MAX_COLUMNS {
            return Err(Unreadable(format!(
                "Excel のシート「{name}」の列が多すぎます（{} 列まで）。表から離れた右の列に値が無いか確かめてください。",
                group_digits(MAX_COLUMNS as usize)
            )));
        }
        cells.push((row, col, value));
    }
    if cells.is_empty() {
        return Err(Unreadable(format!(
            "Excel の左端のシート「{name}」に値がありません。表を左端のシートに置いてください。"
        )));
    }
    Ok(sheet_of(cells, min_col, max_col))
}

/// セルの値（空・空の文字は None）。
fn value_of(data: &DataRef<'_>) -> Option<Value> {
    Some(match data {
        DataRef::Empty => return None,
        DataRef::Int(n) => Value::Number(*n as f64),
        DataRef::Float(f) => Value::Number(*f),
        DataRef::Bool(b) => Value::Bool(*b),
        DataRef::String(s) if s.is_empty() => return None,
        DataRef::String(s) => Value::Text(s.clone()),
        DataRef::SharedString("") => return None,
        DataRef::SharedString(s) => Value::Text((*s).to_string()),
        DataRef::DateTime(dt) if dt.is_datetime() => date_of(dt),
        // 期間（[h]:mm など）は日数の数のまま。
        DataRef::DateTime(dt) => Value::Number(dt.as_f64()),
        DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => Value::Text(s.clone()),
        DataRef::Error(e) => Value::Text(e.to_string()),
    })
}

/// Excel の日時（日数）を文字にする。1 日に満たない値は時刻、時刻が 0 なら日付。
fn date_of(dt: &calamine::ExcelDateTime) -> Value {
    let serial = dt.as_f64();
    let (y, mo, d, h, mi, s, ms) = dt.to_ymd_hms_milli();
    if (y, mo, d) == (1900, 2, 29) {
        // Excel が閏年と扱う 1900 年 2 月 29 日は実在しない（日付の型に入れられないので文字にする）。
        return Value::Text("1900-02-29".into());
    }
    let time = if ms == 0 {
        format!("{h:02}:{mi:02}:{s:02}")
    } else {
        format!("{h:02}:{mi:02}:{s:02}.{ms:03}")
    };
    if (0.0..1.0).contains(&serial) {
        Value::Date(time, DateKind::Time)
    } else if (h, mi, s, ms) == (0, 0, 0, 0) {
        Value::Date(format!("{y:04}-{mo:02}-{d:02}"), DateKind::Date)
    } else {
        Value::Date(format!("{y:04}-{mo:02}-{d:02} {time}"), DateKind::DateTime)
    }
}

/// Excel の数の精度（15 桁）に丸める。
fn excel_number(f: f64) -> f64 {
    if f == 0.0 || !f.is_finite() {
        return f;
    }
    format!("{f:.14e}").parse().unwrap_or(f)
}

/// 値の文字の形。
fn text_of(value: &Value) -> String {
    match value {
        Value::Number(f) => excel_number(*f).to_string(),
        Value::Bool(true) => "TRUE".into(),
        Value::Bool(false) => "FALSE".into(),
        Value::Date(s, _) | Value::Text(s) => s.clone(),
    }
}

/// 読んだセル（行と列の位置つき）を、見出しと行にする。
fn sheet_of(mut cells: Vec<(u32, u32, Value)>, min_col: u32, max_col: u32) -> Sheet {
    // 行と列の順にそろえる（壊れた順のファイルでも同じ表になる）。同じ位置は後のセルを使う。
    cells.sort_by_key(|(r, c, _)| (*r, *c));
    let width = (max_col - min_col + 1) as usize;
    let mut by_row: BTreeMap<u32, Vec<Option<Value>>> = BTreeMap::new();
    for (r, c, v) in cells {
        by_row.entry(r).or_insert_with(|| vec![None; width])[(c - min_col) as usize] = Some(v);
    }
    let mut rows = by_row.into_values();
    let header = rows.next().unwrap_or_default();
    let columns = column_names(
        &header
            .iter()
            .map(|v| v.as_ref().map(text_of).unwrap_or_default())
            .collect::<Vec<_>>(),
    );
    let data: Vec<Vec<Option<Value>>> = rows.collect();
    let types = (0..width).map(|i| column_type(&data, i)).collect();
    let rows = data
        .iter()
        .map(|row| row.iter().map(|v| v.as_ref().map(text_of)).collect())
        .collect();
    Sheet {
        columns,
        types,
        rows,
    }
}

/// 列の名前（DuckDB の CSV の読み込みと同じ決め方）。
fn column_names(header: &[String]) -> Vec<String> {
    let digits = (header.len().max(1) - 1).to_string().len();
    let mut used: HashSet<String> = HashSet::new();
    header
        .iter()
        .enumerate()
        .map(|(i, raw)| {
            let trimmed = raw.trim();
            let base = if trimmed.is_empty() {
                format!("column{i:0digits$}")
            } else {
                trimmed.to_string()
            };
            let mut name = base.clone();
            let mut n = 1;
            while used.contains(&name.to_lowercase()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            used.insert(name.to_lowercase());
            name
        })
        .collect()
}

/// 列のセルの種類から、集計で読むときの型を決める。
fn column_type(rows: &[Vec<Option<Value>>], col: usize) -> &'static str {
    let (mut numbers, mut integers, mut bools, mut texts) = (0usize, 0usize, 0usize, 0usize);
    let (mut dates, mut times, mut datetimes) = (0usize, 0usize, 0usize);
    for value in rows.iter().filter_map(|r| r[col].as_ref()) {
        match value {
            Value::Number(f) => {
                numbers += 1;
                let f = excel_number(*f);
                if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
                    integers += 1;
                }
            }
            Value::Bool(_) => bools += 1,
            Value::Date(_, DateKind::Date) => dates += 1,
            Value::Date(_, DateKind::Time) => times += 1,
            Value::Date(_, DateKind::DateTime) => datetimes += 1,
            Value::Text(_) => texts += 1,
        }
    }
    let total = numbers + bools + texts + dates + times + datetimes;
    match () {
        _ if total == 0 => "VARCHAR",
        _ if numbers == total && integers == total => "BIGINT",
        _ if numbers == total => "DOUBLE",
        _ if bools == total => "BOOLEAN",
        _ if dates == total => "DATE",
        _ if times == total => "TIME",
        _ if dates + datetimes == total => "TIMESTAMP",
        _ => "VARCHAR",
    }
}

/// 識別子の引用（`"` は重ねる）。
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// シートを `data` に入れる。まず全列を文字で入れ、集計なら列の型に直す（文字の形はこちらで作ったので直せる）。
fn create_table(conn: &Connection, sheet: &Sheet, typed: bool) -> Result<(), String> {
    let raw: Vec<String> = (0..sheet.columns.len())
        .map(|i| format!("c{i} VARCHAR"))
        .collect();
    conn.execute_batch(&format!("CREATE TABLE raw ({});", raw.join(", ")))
        .map_err(|e| format!("Excel の表の準備に失敗: {e}"))?;
    {
        let mut app = conn
            .appender("raw")
            .map_err(|e| format!("Excel の表の準備に失敗: {e}"))?;
        for row in &sheet.rows {
            app.append_row(appender_params_from_iter(row.iter().map(Option::as_deref)))
                .map_err(|e| format!("Excel の表の書き込みに失敗: {e}"))?;
        }
        app.flush()
            .map_err(|e| format!("Excel の表の書き込みに失敗: {e}"))?;
    }
    let selected: Vec<String> = sheet
        .columns
        .iter()
        .zip(&sheet.types)
        .enumerate()
        .map(|(i, (name, ty))| {
            if typed && *ty != "VARCHAR" {
                format!("CAST(c{i} AS {ty}) AS {}", quote(name))
            } else {
                format!("c{i} AS {}", quote(name))
            }
        })
        .collect();
    conn.execute_batch(&format!(
        "CREATE TABLE data AS SELECT {} FROM raw; DROP TABLE raw;",
        selected.join(", ")
    ))
    .map_err(|e| format!("Excel の表の作成に失敗: {e}"))
}

/// 3 桁ごとにカンマを入れる（1,000,000）。
fn group_digits(n: usize) -> String {
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
