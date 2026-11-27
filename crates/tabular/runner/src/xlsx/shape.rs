//! 読んだセルを表の形にする（見出し・列の名前・列の型・D-148・GAP-83）。
//!
//! - 見出し（列の名前）は、表の上の表題の行を飛ばして決める（GAP-83）。値のある先頭の行から順に、値の数が、すぐ下の
//!   [`HEADER_SAMPLE_ROWS`] 行の値の数のなかほど（中央の値）の半分以下なら表題として飛ばし、最初に残った行を見出しに
//!   する。CSV の DuckDB の推定器が、列の数のそろわない先頭の行を飛ばすのと同じ考え方（Excel は空のセルを持たないので、
//!   列の数でなく値の数で比べる）。見出しが 1 つ 2 つ空でも、半分を超えて値があれば見出しのまま。中央の値で比べるので、
//!   右に備考などを書いた幅の広い行が表の中にあっても、見出しを飛ばさない。下に行の無い行（最後の行）は飛ばさない。
//!   2 列の表で見出しの 1 つが空のときは、表題と見分けられず飛ばす（列の案内が見出しにした行の番号を言う）。
//! - 名前の決め方は DuckDB の CSV の読み込みと同じ（前後の空白を外す・空は `column{位置}`・大文字小文字を区別せずに
//!   重なれば `_1` から番号を付ける）。
//! - 列は、見出しと表の本体の値のある左端から右端まで（飛ばした表題の列は入れない）。値の無い行は飛ばす。
//! - 数式は計算しない。Excel が保存したときの値を読む（値の無い数式のセルは空）。外部のブックへの参照はたどらない。
//! - 数は Excel と同じ 15 桁に丸める（0.1+0.2 を 0.3 にする）。日付は `2024-01-31`、日時は `2024-01-31 09:30:00`、
//!   時刻（1 日に満たない値）は `09:30:00`。真偽は `TRUE`／`FALSE`、誤りの値は `#DIV/0!` のような文字。
//! - 集計（Query）は列ごとにセルの種類から型を決める: 数だけなら BIGINT（すべて整数のとき）か DOUBLE、真偽だけなら
//!   BOOLEAN、日付だけなら DATE、時刻だけなら TIME、日付と日時なら TIMESTAMP、ほかは VARCHAR。グリッド（Schema・
//!   Rows）は全列を文字で入れる（CSV と同じ）。

use std::collections::HashSet;

use calamine::DataRef;

/// 見出しを探すときに比べる下の行の数（表題は表の本体のすぐ上にある。遠く離れた行の値の数では決めない）。
pub const HEADER_SAMPLE_ROWS: usize = 20;

/// セルの値（文字の形を決める前）。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Bool(bool),
    /// 日付・時刻（文字の形と種類）。
    Date(String, DateKind),
    Text(String),
}

/// 日付・時刻の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateKind {
    Date,
    Time,
    DateTime,
}

/// 読んだシート（見出しと、値のある行）。
#[derive(Debug, Default)]
pub struct Sheet {
    /// 列の名前（DuckDB の CSV の読み込みと同じ決め方）。
    pub columns: Vec<String>,
    /// 集計で読むときの列の型（DuckDB の型の名前）。
    pub types: Vec<&'static str>,
    /// 行（値のある列だけの、列の位置と文字の形。列の位置の順）。
    pub rows: Vec<Vec<(usize, String)>>,
    /// 見出しにした行の番号（Excel の行の番号・1 から）。表の上の表題の行を飛ばしたときだけ。
    pub header_row: Option<u32>,
}

/// セルの値（空・空の文字は None）。
pub fn value_of(data: &DataRef<'_>) -> Option<Value> {
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

/// 見出しの行の位置（値のある行の並びの中の位置）。値の数が、すぐ下の [`HEADER_SAMPLE_ROWS`] 行の値の数の中央の値
/// （偶数のときは大きいほう）の半分以下の行は、表の上の表題として飛ばす。
fn header_index(counts: &[usize]) -> usize {
    (0..counts.len())
        .find(|&h| {
            let mut below: Vec<usize> = counts[h + 1..]
                .iter()
                .take(HEADER_SAMPLE_ROWS)
                .copied()
                .collect();
            below.sort_unstable();
            below
                .get(below.len() / 2)
                .is_none_or(|&body| counts[h] * 2 > body)
        })
        .unwrap_or(0)
}

/// 値のある 1 行（行の位置と、列の位置つきの値・列の位置の順）。
type SheetRow = (u32, Vec<(u32, Value)>);

/// 読んだセル（行と列の位置つき）を、見出しと行にする。行は疎のまま（値のある列だけ）持つ。
pub fn sheet_of(mut cells: Vec<(u32, u32, Value)>) -> Sheet {
    // 行と列の順にそろえる（壊れた順のファイルでも同じ表になる）。同じ位置は後のセルを使う（安定な並べ替え）。
    cells.sort_by_key(|(r, c, _)| (*r, *c));
    let mut by_row: Vec<SheetRow> = Vec::new();
    for (r, c, v) in cells {
        if by_row.last().is_none_or(|(row, _)| *row != r) {
            by_row.push((r, Vec::new()));
        }
        let Some((_, row)) = by_row.last_mut() else {
            continue;
        };
        match row.last_mut() {
            Some((last, value)) if *last == c => *value = v,
            _ => row.push((c, v)),
        }
    }
    let counts: Vec<usize> = by_row.iter().map(|(_, row)| row.len()).collect();
    let h = header_index(&counts);
    let header_row = by_row.get(h).filter(|_| h > 0).map(|(r, _)| r + 1);
    let body = by_row.split_off(h);
    // 列は見出しと本体の値のある左端から右端まで（飛ばした表題の列は入れない）。
    let cols = body.iter().flat_map(|(_, row)| row.iter().map(|(c, _)| *c));
    let (min_col, max_col) = cols.fold((u32::MAX, 0), |(lo, hi), c| (lo.min(c), hi.max(c)));
    if min_col > max_col {
        return Sheet::default();
    }
    let width = (max_col - min_col + 1) as usize;
    let mut rows = body.into_iter();
    let mut header = vec![String::new(); width];
    for (col, value) in rows.next().map(|(_, row)| row).unwrap_or_default() {
        header[(col - min_col) as usize] = text_of(&value);
    }
    let columns = column_names(&header);
    let mut kinds = vec![ColumnKinds::default(); width];
    let rows = rows
        .map(|(_, row)| {
            row.into_iter()
                .map(|(col, value)| {
                    let col = (col - min_col) as usize;
                    kinds[col].count(&value);
                    (col, text_of(&value))
                })
                .collect()
        })
        .collect();
    Sheet {
        columns,
        types: kinds.iter().map(ColumnKinds::column_type).collect(),
        rows,
        header_row,
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

/// 列のセルの種類の数（集計で読むときの型を決める）。
#[derive(Debug, Clone, Copy, Default)]
struct ColumnKinds {
    numbers: usize,
    integers: usize,
    bools: usize,
    texts: usize,
    dates: usize,
    times: usize,
    datetimes: usize,
}

impl ColumnKinds {
    fn count(&mut self, value: &Value) {
        match value {
            Value::Number(f) => {
                self.numbers += 1;
                let f = excel_number(*f);
                if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
                    self.integers += 1;
                }
            }
            Value::Bool(_) => self.bools += 1,
            Value::Date(_, DateKind::Date) => self.dates += 1,
            Value::Date(_, DateKind::Time) => self.times += 1,
            Value::Date(_, DateKind::DateTime) => self.datetimes += 1,
            Value::Text(_) => self.texts += 1,
        }
    }

    /// 列のセルの種類から、集計で読むときの型を決める。
    fn column_type(&self) -> &'static str {
        let total =
            self.numbers + self.bools + self.texts + self.dates + self.times + self.datetimes;
        match () {
            _ if total == 0 => "VARCHAR",
            _ if self.numbers == total && self.integers == total => "BIGINT",
            _ if self.numbers == total => "DOUBLE",
            _ if self.bools == total => "BOOLEAN",
            _ if self.dates == total => "DATE",
            _ if self.times == total => "TIME",
            _ if self.dates + self.datetimes == total => "TIMESTAMP",
            _ => "VARCHAR",
        }
    }
}
