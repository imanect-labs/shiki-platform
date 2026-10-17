//! 数式の書き方の変換（Excel ⇔ Calc の API 文法・#725）。
//!
//! LLM には Excel と同じ書き方（区切り `,`・シート参照 `Sheet1!A1`・配列定数 `{1,2;3,4}`）で
//! 書かせ、読むときも Excel の書き方で見せる。kit の `XCell.setFormula` / `getFormula` は
//! Calc の API 文法（区切り `;`・シート参照 `$Sheet1.A1`・配列定数 `{1;2|3;4}`）なので、
//! ここで相互に変換する（関数名は英語名のまま両者で共通）。
//!
//! 文字列リテラル（`"…"`）と引用符つきシート名（`'…'`）の中身には触れない。

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FormulaError {
    #[error("数式の文字列リテラル（\"…\"）が閉じていません")]
    UnclosedString,
    #[error("数式のシート名の引用符（'…'）が閉じていません")]
    UnclosedSheet,
    #[error("数式の括弧の対応がとれていません")]
    Unbalanced,
    #[error("! の前にシート名がありません")]
    MissingSheet,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// `"…"`（引用符ごと保持）。
    Str(String),
    /// `'…'`（引用符ごと保持）。
    Quoted(String),
    /// 識別子・参照・数値（演算子・区切り・空白以外の連なり）。
    Word(String),
    /// 1 文字の記号。
    Punct(char),
    /// 空白（そのまま保持）。
    Space(String),
}

const PUNCT: &[char] = &[
    '+', '-', '*', '/', '^', '&', '=', '<', '>', '(', ')', ',', ';', '{', '}', '!', ':', '%', '|',
];

fn tokenize(src: &str) -> Result<Vec<Tok>, FormulaError> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            let mut j = i + 1;
            loop {
                if j >= chars.len() {
                    return Err(if c == '"' {
                        FormulaError::UnclosedString
                    } else {
                        FormulaError::UnclosedSheet
                    });
                }
                if chars[j] == c {
                    // 二重にした引用符はリテラル内の引用符。
                    if chars.get(j + 1) == Some(&c) {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            let s: String = chars[i..=j].iter().collect();
            out.push(if c == '"' {
                Tok::Str(s)
            } else {
                Tok::Quoted(s)
            });
            i = j + 1;
        } else if c.is_whitespace() {
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            out.push(Tok::Space(chars[i..j].iter().collect()));
            i = j;
        } else if PUNCT.contains(&c) {
            out.push(Tok::Punct(c));
            i += 1;
        } else {
            let mut j = i;
            while j < chars.len()
                && !chars[j].is_whitespace()
                && !PUNCT.contains(&chars[j])
                && chars[j] != '"'
                && chars[j] != '\''
            {
                j += 1;
            }
            out.push(Tok::Word(chars[i..j].iter().collect()));
            i = j;
        }
    }
    let mut depth: i32 = 0;
    for t in &out {
        match t {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return Err(FormulaError::Unbalanced);
        }
    }
    if depth != 0 {
        return Err(FormulaError::Unbalanced);
    }
    Ok(out)
}

fn join(toks: &[Tok]) -> String {
    toks.iter()
        .map(|t| match t {
            Tok::Str(s) | Tok::Quoted(s) | Tok::Word(s) | Tok::Space(s) => s.clone(),
            Tok::Punct(c) => c.to_string(),
        })
        .collect()
}

/// Excel の書き方 → Calc の API 文法。先頭の `=` は無ければ補う。
pub fn excel_to_calc(formula: &str) -> Result<String, FormulaError> {
    let body = formula.trim().strip_prefix('=').unwrap_or(formula.trim());
    let toks = tokenize(body)?;
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut brace = 0;
    for t in toks {
        match t {
            Tok::Punct('{') => {
                brace += 1;
                out.push(t);
            }
            Tok::Punct('}') => {
                brace -= 1;
                out.push(t);
            }
            Tok::Punct(',') => out.push(Tok::Punct(';')),
            Tok::Punct(';') if brace > 0 => out.push(Tok::Punct('|')),
            Tok::Punct('!') => {
                // 直前の単語（または引用符つき名）がシート名。$name. へ書き換える。
                let sheet = match out.pop() {
                    Some(Tok::Word(w)) => w,
                    Some(Tok::Quoted(q)) => q,
                    _ => return Err(FormulaError::MissingSheet),
                };
                out.push(Tok::Word(format!("${sheet}.")));
            }
            other => out.push(other),
        }
    }
    // `$Sheet.` と後続の参照をつなぐ（トークンを結合するだけで、間に何も入らない）。
    Ok(format!("={}", join(&out)))
}

/// 単語が「シート接頭辞つき参照」なら (シート名, 参照) に分ける（例 `$Sheet1.B2` → (`Sheet1`, `B2`)）。
///
/// 列だけ・行だけの参照（`B`・`2`）は範囲の端点（`:` の隣）にあるときだけ参照とみなす
/// （`STDEV.S` を「シート STDEV のセル S」と読まないため）。
fn split_calc_sheet_ref(word: &str, near_colon: bool) -> Option<(String, String)> {
    let (prefix, rest) = word.rsplit_once('.')?;
    let name = prefix.strip_prefix('$').unwrap_or(prefix);
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    is_ref_part(rest, near_colon).then(|| (name.to_string(), rest.to_string()))
}

/// `B2`・`$B$2`、または範囲の端点としての `B`（列だけ）・`2`（行だけ）。
fn is_ref_part(s: &str, near_colon: bool) -> bool {
    let t = s.replace('$', "");
    let letters = t.chars().take_while(char::is_ascii_alphabetic).count();
    let (col, row) = t.split_at(letters);
    let shape_ok = col.len() <= 3
        && row.chars().all(|c| c.is_ascii_digit())
        && col.chars().all(|c| c.is_ascii_uppercase());
    let full = !col.is_empty() && !row.is_empty();
    let partial = col.is_empty() != row.is_empty();
    shape_ok && (full || (partial && near_colon))
}

/// Calc の API 文法 → Excel の書き方（先頭の `=` つき）。変換できない形はそのまま残す。
pub fn calc_to_excel(formula: &str) -> String {
    let body = formula.strip_prefix('=').unwrap_or(formula);
    let Ok(toks) = tokenize(body) else {
        return formula.to_string();
    };
    let mut out: Vec<String> = Vec::with_capacity(toks.len());
    let mut brace = 0;
    let mut last_sheet: Option<String> = None;
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Punct('{') => {
                brace += 1;
                out.push("{".into());
            }
            Tok::Punct('}') => {
                brace -= 1;
                out.push("}".into());
            }
            Tok::Punct(';') => out.push(",".into()),
            Tok::Punct('|') if brace > 0 => out.push(";".into()),
            // `$'集計 2'.A1` は [Word("$")][Quoted][Word(".A1")] に分かれる。
            Tok::Quoted(q) if matches!(toks.get(i + 1), Some(Tok::Word(w)) if w.starts_with('.')) =>
            {
                if out.last().is_some_and(|s| s == "$") {
                    out.pop();
                }
                let Some(Tok::Word(w)) = toks.get(i + 1) else {
                    unreachable!()
                };
                let rest = &w[1..];
                if last_sheet.as_deref() == Some(q.as_str()) && out.last().is_some_and(|s| s == ":")
                {
                    out.push(rest.to_string());
                } else {
                    out.push(format!("{q}!{rest}"));
                }
                last_sheet = Some(q.clone());
                i += 2;
                continue;
            }
            Tok::Word(w) => match (!matches!(toks.get(i + 1), Some(Tok::Punct('('))))
                .then(|| {
                    let near_colon = matches!(toks.get(i + 1), Some(Tok::Punct(':')))
                        || (i > 0 && matches!(toks.get(i - 1), Some(Tok::Punct(':'))));
                    split_calc_sheet_ref(w, near_colon)
                })
                .flatten()
            {
                Some((sheet, rest)) => {
                    let quoted = super::a1::quote_sheet(&sheet);
                    if last_sheet.as_deref() == Some(quoted.as_str())
                        && out.last().is_some_and(|s| s == ":")
                    {
                        out.push(rest);
                    } else {
                        out.push(format!("{quoted}!{rest}"));
                    }
                    last_sheet = Some(quoted);
                }
                None => out.push(w.clone()),
            },
            Tok::Str(s) | Tok::Quoted(s) | Tok::Space(s) => out.push(s.clone()),
            Tok::Punct(c) => out.push(c.to_string()),
        }
        i += 1;
    }
    format!("={}", out.concat())
}

/// セル参照の相対部分を (dc, dr) だけずらす（Excel の書き方の数式・フィル用）。
///
/// `$` で固定した側・関数名・文字列・シート名には触れない。ずらした結果が範囲外なら `#REF!`。
pub fn shift_relative(formula: &str, dc: i64, dr: i64) -> Result<String, FormulaError> {
    let body = formula.trim().strip_prefix('=').unwrap_or(formula.trim());
    let toks = tokenize(body)?;
    let mut out = String::with_capacity(body.len() + 8);
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Word(w) => {
                let is_function = matches!(toks.get(i + 1), Some(Tok::Punct('(')));
                let near_colon = matches!(toks.get(i + 1), Some(Tok::Punct(':')))
                    || (i > 0 && matches!(toks.get(i - 1), Some(Tok::Punct(':'))));
                match (!is_function)
                    .then(|| shift_ref(w, dc, dr, near_colon))
                    .flatten()
                {
                    Some(s) => out.push_str(&s),
                    None => out.push_str(w),
                }
            }
            Tok::Str(s) | Tok::Quoted(s) | Tok::Space(s) => out.push_str(s),
            Tok::Punct(c) => out.push(*c),
        }
    }
    Ok(format!("={out}"))
}

/// 数式の「形」（相対参照を R[行差]C[列差] に置き換えた文字列）。`(col, row)` は数式のあるセル。
///
/// 縦に並んだ同じ形の数式をまとめる比較に使う。`shift_relative` と違い範囲外へ切り詰めないので、
/// 違う形の数式が `#REF!` に潰れて一致することがない。
pub fn relative_signature(formula: &str, col: u32, row: u32) -> String {
    let body = formula.trim().strip_prefix('=').unwrap_or(formula.trim());
    let Ok(toks) = tokenize(body) else {
        return formula.to_string();
    };
    let mut out = String::with_capacity(body.len() + 16);
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Word(w) => {
                let is_function = matches!(toks.get(i + 1), Some(Tok::Punct('(')));
                match (!is_function).then(|| ref_signature(w, col, row)).flatten() {
                    Some(sig) => out.push_str(&sig),
                    None => out.push_str(w),
                }
            }
            Tok::Str(s) | Tok::Quoted(s) | Tok::Space(s) => out.push_str(s),
            Tok::Punct(c) => out.push(*c),
        }
    }
    out
}

fn ref_signature(word: &str, col: u32, row: u32) -> Option<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut i = 0;
    let col_abs = chars.first() == Some(&'$');
    if col_abs {
        i += 1;
    }
    let cs = i;
    while i < chars.len() && chars[i].is_ascii_uppercase() {
        i += 1;
    }
    let c: String = chars[cs..i].iter().collect();
    let row_abs = chars.get(i) == Some(&'$');
    if row_abs {
        i += 1;
    }
    let rs = i;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    let r: String = chars[rs..i].iter().collect();
    if i != chars.len() || c.is_empty() || r.is_empty() || c.len() > 3 {
        return None;
    }
    let cn = i64::from(super::a1::parse_col(&c)?);
    let rn: i64 = r.parse::<i64>().ok()? - 1;
    let cpart = if col_abs {
        format!("C{}", cn + 1)
    } else {
        format!("C[{}]", cn - i64::from(col))
    };
    let rpart = if row_abs {
        format!("R{}", rn + 1)
    } else {
        format!("R[{}]", rn - i64::from(row))
    };
    Some(format!("{rpart}{cpart}"))
}

/// 単語が参照なら相対部分をずらした文字列を返す（参照でなければ None）。
fn shift_ref(word: &str, dc: i64, dr: i64, near_colon: bool) -> Option<String> {
    let bytes: Vec<char> = word.chars().collect();
    let mut i = 0;
    let col_abs = bytes.first() == Some(&'$');
    if col_abs {
        i += 1;
    }
    let col_start = i;
    while i < bytes.len() && bytes[i].is_ascii_uppercase() {
        i += 1;
    }
    let col: String = bytes[col_start..i].iter().collect();
    let row_abs = bytes.get(i) == Some(&'$');
    if row_abs {
        i += 1;
    }
    let row_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let row: String = bytes[row_start..i].iter().collect();
    if i != bytes.len() || col.len() > 3 {
        return None;
    }
    // 列だけ・行だけの参照は範囲（A:A・1:3）の端点としてだけ認める（名前や数値と取り違えない）。
    if (col.is_empty() || row.is_empty()) && !near_colon {
        return None;
    }
    if col.is_empty() && row.is_empty() {
        return None;
    }
    if col_abs && col.is_empty() {
        return None;
    }
    let mut s = String::new();
    if !col.is_empty() {
        let c = i64::from(super::a1::parse_col(&col)?);
        let c = if col_abs { c } else { c + dc };
        if !(0..=i64::from(super::a1::MAX_COL)).contains(&c) {
            return Some("#REF!".into());
        }
        if col_abs {
            s.push('$');
        }
        s.push_str(&super::a1::col_name(u32::try_from(c).ok()?));
    }
    if !row.is_empty() {
        let r: i64 = row.parse().ok()?;
        let r = if row_abs { r } else { r + dr };
        if !(1..=i64::from(super::a1::MAX_ROW) + 1).contains(&r) {
            return Some("#REF!".into());
        }
        if row_abs {
            s.push('$');
        }
        s.push_str(&r.to_string());
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn converts_excel_syntax_to_calc_api() {
        let cases = [
            ("=SUM(B2:B4,1)", "=SUM(B2:B4;1)"),
            ("SUM(A1)", "=SUM(A1)"),
            ("=IF(Sheet1!D2>0.3,1,0)", "=IF($Sheet1.D2>0.3;1;0)"),
            ("=SUM(売上!B2:B4)", "=SUM($売上.B2:B4)"),
            ("='集計 2'!A1*2", "=$'集計 2'.A1*2"),
            ("=SUM({1,2;3,4})", "=SUM({1;2|3;4})"),
            ("=COUNTIF(A2:A4,\"a,b;c\")", "=COUNTIF(A2:A4;\"a,b;c\")"),
            (
                "=TEXT(DATE(2026,1,15),\"yyyy/mm/dd\")",
                "=TEXT(DATE(2026;1;15);\"yyyy/mm/dd\")",
            ),
            ("=STDEV.S(B2:B9)", "=STDEV.S(B2:B9)"),
            ("=XLOOKUP(\"x\",A:A,B:B)", "=XLOOKUP(\"x\";A:A;B:B)"),
        ];
        for (excel, calc) in cases {
            assert_eq!(excel_to_calc(excel).unwrap(), calc, "{excel}");
        }
    }

    #[test]
    fn converts_calc_api_back_to_excel_syntax() {
        let cases = [
            ("=SUM(B2:B4;1)", "=SUM(B2:B4,1)"),
            ("=IF($Sheet1.D2>0.3;1;0)", "=IF(Sheet1!D2>0.3,1,0)"),
            ("=SUM($売上.B2:B4)", "=SUM(売上!B2:B4)"),
            ("=$'集計 2'.A1*2", "='集計 2'!A1*2"),
            ("=SUM({1;2|3;4})", "=SUM({1,2;3,4})"),
            ("=SUM($Sheet1.B2:$Sheet1.B4)", "=SUM(Sheet1!B2:B4)"),
            ("=STDEV.S(B2:B9)", "=STDEV.S(B2:B9)"),
            ("=ROUND(1.5;0)", "=ROUND(1.5,0)"),
            ("=SUM($Sheet1.B:B)", "=SUM(Sheet1!B:B)"),
        ];
        for (calc, excel) in cases {
            assert_eq!(calc_to_excel(calc), excel, "{calc}");
        }
    }

    #[test]
    fn round_trips() {
        for f in [
            "=SUMPRODUCT((Sheet1!B2:B4>10000000)*Sheet1!B2:B4)",
            "=IFERROR(VLOOKUP(A2,'マスタ 一覧'!A:C,3,FALSE),\"-\")",
            "=LET(x,2,x*3)",
        ] {
            assert_eq!(calc_to_excel(&excel_to_calc(f).unwrap()), f);
        }
    }

    #[test]
    fn rejects_broken_formulas() {
        assert_eq!(excel_to_calc("=SUM(A1"), Err(FormulaError::Unbalanced));
        assert_eq!(excel_to_calc("=\"abc"), Err(FormulaError::UnclosedString));
        assert_eq!(excel_to_calc("='abc!A1"), Err(FormulaError::UnclosedSheet));
        assert_eq!(excel_to_calc("=!A1"), Err(FormulaError::MissingSheet));
    }

    #[test]
    fn signatures_distinguish_shapes_that_shift_would_collapse() {
        assert_eq!(
            relative_signature("=SUM(B2:D2)", 4, 1),
            relative_signature("=SUM(B3:D3)", 4, 2)
        );
        assert_ne!(
            relative_signature("=SUM(B2:D2)", 4, 1),
            relative_signature("=SUM(E2:E4)", 4, 4)
        );
        assert_eq!(relative_signature("=$A$1*B2", 2, 1), "R1C1*R[0]C[-1]");
    }

    #[test]
    fn shifts_relative_references_only() {
        assert_eq!(shift_relative("=B2-C2", 0, 3).unwrap(), "=B5-C5");
        assert_eq!(
            shift_relative("=$B2*C$1+$D$4", 1, 2).unwrap(),
            "=$B4*D$1+$D$4"
        );
        assert_eq!(shift_relative("=SUM(B2:D2)", 0, 1).unwrap(), "=SUM(B3:D3)");
        assert_eq!(
            shift_relative("=LOG10(A1)", 1, 0).unwrap(),
            "=LOG10(B1)",
            "関数名はずらさない"
        );
        assert_eq!(
            shift_relative("=\"A1\"&A1", 0, 1).unwrap(),
            "=\"A1\"&A2",
            "文字列はずらさない"
        );
        assert_eq!(shift_relative("=Sheet1!A1", 0, 1).unwrap(), "=Sheet1!A2");
        assert_eq!(shift_relative("=SUM(A:A)", 1, 0).unwrap(), "=SUM(B:B)");
        assert_eq!(shift_relative("=A1", 0, -1).unwrap(), "=#REF!");
        assert_eq!(
            shift_relative("=ROUND(A1,2)", 0, 1).unwrap(),
            "=ROUND(A2,2)",
            "数値はずらさない"
        );
    }
}
