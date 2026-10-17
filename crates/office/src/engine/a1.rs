//! A1 表記の解釈（Excel と同じ書き方・#725）。
//!
//! LLM には Excel の書き方（`A1`・`B2:D10`・`'集計 2'!A1:C3`・`A:C`・`3:5`）で書かせ、
//! ここで 0 始まりの数値座標へ直す。エンジン（kit 内の JS）は数値しか受け取らない。

use std::fmt;

/// Calc / Excel の上限（XFD 列・1048576 行）。
pub const MAX_COL: u32 = 16_383;
pub const MAX_ROW: u32 = 1_048_575;

/// 0 始まりの矩形（列始・行始・列終・行終）。シート名は省略可（省略時は呼び出し側の既定シート）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Area {
    pub sheet: Option<String>,
    pub sc: u32,
    pub sr: u32,
    pub ec: u32,
    pub er: u32,
}

impl Area {
    pub fn cells(&self) -> u64 {
        u64::from(self.ec - self.sc + 1) * u64::from(self.er - self.sr + 1)
    }

    pub fn width(&self) -> u32 {
        self.ec - self.sc + 1
    }

    pub fn height(&self) -> u32 {
        self.er - self.sr + 1
    }

    /// エンジンへ渡す形（`[sc, sr, ec, er]`）。
    pub fn coords(&self) -> [u32; 4] {
        [self.sc, self.sr, self.ec, self.er]
    }

    /// シート名を除いた A1 表記（例 `B2:D10`・単一セルは `B2`）。
    pub fn a1(&self) -> String {
        let start = cell_name(self.sc, self.sr);
        if self.sc == self.ec && self.sr == self.er {
            start
        } else {
            format!("{start}:{}", cell_name(self.ec, self.er))
        }
    }
}

impl fmt::Display for Area {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.sheet {
            Some(sheet) => write!(f, "{}!{}", quote_sheet(sheet), self.a1()),
            None => f.write_str(&self.a1()),
        }
    }
}

/// A1 表記の誤り（LLM へそのまま見せる文言）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("範囲「{input}」を解釈できません: {reason}")]
pub struct A1Error {
    pub input: String,
    pub reason: &'static str,
}

fn err(input: &str, reason: &'static str) -> A1Error {
    A1Error {
        input: input.to_string(),
        reason,
    }
}

/// 列番号（0 始まり）→ 列名（`A`・`AB`）。
pub fn col_name(col: u32) -> String {
    let mut n = col + 1;
    let mut out = Vec::new();
    while n > 0 {
        let m = (n - 1) % 26;
        out.push(char::from(b'A' + u8::try_from(m).unwrap_or(0)));
        n = (n - 1) / 26;
    }
    out.iter().rev().collect()
}

/// セル名（`B3`）。
pub fn cell_name(col: u32, row: u32) -> String {
    format!("{}{}", col_name(col), row + 1)
}

/// シート名を必要なら引用符で囲む（英数・`_`・非 ASCII 文字だけなら裸のまま）。
pub fn quote_sheet(name: &str) -> String {
    let bare = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || (!c.is_ascii() && !c.is_whitespace()));
    if bare {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

/// 列名（`A`〜`XFD`・大文字小文字は問わない）→ 0 始まり。
pub fn parse_col(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 3 || !s.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut n: u32 = 0;
    for c in s.chars() {
        n = n * 26 + (u32::from(c.to_ascii_uppercase()) - u32::from('A') + 1);
    }
    let col = n - 1;
    (col <= MAX_COL).then_some(col)
}

fn parse_row(s: &str) -> Option<u32> {
    if s.is_empty() || s.starts_with('0') || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: u32 = s.parse().ok()?;
    (1..=MAX_ROW + 1).contains(&n).then(|| n - 1)
}

/// 1 つの端点（`B3`・`$B$3`・`B`・`3`）。
enum End {
    Cell(u32, u32),
    Col(u32),
    Row(u32),
}

fn parse_end(s: &str) -> Option<End> {
    let s = s.replace('$', "");
    let letters = s.chars().take_while(char::is_ascii_alphabetic).count();
    let (col, row) = s.split_at(letters);
    match (col.is_empty(), row.is_empty()) {
        (false, false) => Some(End::Cell(parse_col(col)?, parse_row(row)?)),
        (false, true) => Some(End::Col(parse_col(col)?)),
        (true, false) => Some(End::Row(parse_row(row)?)),
        (true, true) => None,
    }
}

/// シート接頭辞（`Sheet1!`・`'集計 2'!`）を切り出す。
fn split_sheet(input: &str) -> Result<(Option<String>, &str), A1Error> {
    let s = input.trim();
    if let Some(rest) = s.strip_prefix('\'') {
        // 引用符つき。'' は引用符そのもの。
        let mut name = String::new();
        let mut chars = rest.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if chars.peek().map(|(_, n)| *n) == Some('\'') {
                    name.push('\'');
                    chars.next();
                    continue;
                }
                let after = &rest[i + 1..];
                let Some(range) = after.strip_prefix('!') else {
                    return Err(err(input, "引用符で囲んだシート名の後には ! が必要です"));
                };
                return Ok((Some(name), range));
            }
            name.push(c);
        }
        return Err(err(input, "シート名の引用符が閉じていません"));
    }
    match s.rsplit_once('!') {
        Some((sheet, range)) if !sheet.is_empty() => Ok((Some(sheet.to_string()), range)),
        Some(_) => Err(err(input, "! の前にシート名がありません")),
        None => Ok((None, s)),
    }
}

/// 範囲を解釈する。単一セル・矩形・列全体（`A:C`）・行全体（`3:5`）を受ける。
pub fn parse_area(input: &str) -> Result<Area, A1Error> {
    let (sheet, range) = split_sheet(input)?;
    let (a, b) = match range.split_once(':') {
        Some((a, b)) => (a, Some(b)),
        None => (range, None),
    };
    let first = parse_end(a).ok_or_else(|| err(input, "A1 形式（例 B2:D10）で書いてください"))?;
    let second = match b {
        Some(b) => {
            Some(parse_end(b).ok_or_else(|| err(input, "A1 形式（例 B2:D10）で書いてください"))?)
        }
        None => None,
    };
    let (sc, sr, ec, er) = match (first, second) {
        (End::Cell(c, r), None) => (c, r, c, r),
        (End::Cell(c1, r1), Some(End::Cell(c2, r2))) => {
            (c1.min(c2), r1.min(r2), c1.max(c2), r1.max(r2))
        }
        (End::Col(c), None) => (c, 0, c, MAX_ROW),
        (End::Col(c1), Some(End::Col(c2))) => (c1.min(c2), 0, c1.max(c2), MAX_ROW),
        (End::Row(r), None) => (0, r, MAX_COL, r),
        (End::Row(r1), Some(End::Row(r2))) => (0, r1.min(r2), MAX_COL, r1.max(r2)),
        _ => {
            return Err(err(
                input,
                "端点の種類がそろっていません（例 A1:C3・A:C・3:5）",
            ))
        }
    };
    Ok(Area {
        sheet,
        sc,
        sr,
        ec,
        er,
    })
}

/// 単一セルを解釈する（範囲を渡されたら誤り）。
pub fn parse_cell(input: &str) -> Result<(Option<String>, u32, u32), A1Error> {
    let area = parse_area(input)?;
    if area.sc != area.ec || area.sr != area.er {
        return Err(err(input, "セルを 1 つだけ指定してください（例 B2）"));
    }
    Ok((area.sheet, area.sc, area.sr))
}

/// 列の範囲（`C`・`B:D`）→ (始, 終)。
pub fn parse_columns(input: &str) -> Result<(Option<String>, u32, u32), A1Error> {
    let area = parse_area(input)?;
    if area.sr != 0 || area.er != MAX_ROW {
        return Err(err(input, "列を指定してください（例 B または B:D）"));
    }
    Ok((area.sheet, area.sc, area.ec))
}

/// 行の範囲（`3`・`3:5`）→ (始, 終)。
pub fn parse_rows(input: &str) -> Result<(Option<String>, u32, u32), A1Error> {
    let area = parse_area(input)?;
    if area.sc != 0 || area.ec != MAX_COL {
        return Err(err(input, "行番号を指定してください（例 3 または 3:5）"));
    }
    Ok((area.sheet, area.sr, area.er))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn column_names_round_trip() {
        for (n, s) in [
            (0, "A"),
            (25, "Z"),
            (26, "AA"),
            (51, "AZ"),
            (701, "ZZ"),
            (702, "AAA"),
            (16_383, "XFD"),
        ] {
            assert_eq!(col_name(n), s);
            assert_eq!(parse_col(s), Some(n));
        }
        assert_eq!(parse_col("XFE"), None);
        assert_eq!(parse_col("a"), Some(0));
    }

    #[test]
    fn parses_cells_ranges_columns_and_rows() {
        let a = parse_area("B2:D10").unwrap();
        assert_eq!(a.coords(), [1, 1, 3, 9]);
        assert_eq!(a.sheet, None);
        assert_eq!(parse_area("$B$2").unwrap().coords(), [1, 1, 1, 1]);
        assert_eq!(
            parse_area("D10:B2").unwrap().coords(),
            [1, 1, 3, 9],
            "逆順も正規化"
        );
        assert_eq!(parse_area("A:C").unwrap().coords(), [0, 0, 2, MAX_ROW]);
        assert_eq!(parse_area("3:5").unwrap().coords(), [0, 2, MAX_COL, 4]);
        assert_eq!(parse_columns("C").unwrap(), (None, 2, 2));
        assert_eq!(parse_rows("4").unwrap(), (None, 3, 3));
    }

    #[test]
    fn parses_sheet_prefixes() {
        let a = parse_area("売上!A1:B2").unwrap();
        assert_eq!(a.sheet.as_deref(), Some("売上"));
        let a = parse_area("'集計 2'!C3").unwrap();
        assert_eq!(a.sheet.as_deref(), Some("集計 2"));
        assert_eq!(a.coords(), [2, 2, 2, 2]);
        let a = parse_area("'It''s'!A1").unwrap();
        assert_eq!(a.sheet.as_deref(), Some("It's"));
        assert_eq!(a.to_string(), "'It''s'!A1");
        assert_eq!(parse_area("売上!A1").unwrap().to_string(), "売上!A1");
        assert_eq!(
            parse_area("'集計 2'!A1:B2").unwrap().to_string(),
            "'集計 2'!A1:B2"
        );
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in [
            "",
            "A0",
            "A01",
            "1A",
            "A1:",
            "'abc!A1",
            "!A1",
            "A1:3",
            "B2:C",
            "ZZZZ1",
            "A1048577",
            "Sheet1.A1",
        ] {
            assert!(parse_area(bad).is_err(), "{bad:?} は誤り");
        }
        assert!(parse_cell("A1:B2").is_err());
        assert!(parse_columns("A1").is_err());
        assert!(parse_rows("B").is_err());
    }

    #[test]
    fn quotes_sheet_names_only_when_needed() {
        assert_eq!(quote_sheet("Sheet1"), "Sheet1");
        assert_eq!(quote_sheet("売上"), "売上");
        assert_eq!(quote_sheet("集計 2"), "'集計 2'");
        assert_eq!(quote_sheet("2026"), "'2026'");
        assert_eq!(quote_sheet("a-b"), "'a-b'");
    }
}
