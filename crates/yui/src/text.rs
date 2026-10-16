//! 行単位の共通処理（`@imanect/yui` の lines.ts の移植）。
//!
//! 文字数は JavaScript と同じく UTF-16 の単位で数える（TS 版と同じ所で切り詰めるため）。

use regex::{Captures, Regex};
use std::collections::HashSet;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

/// 正規表現を一度だけ組む。パターンは固定でテスト（`patterns_compile`）が組めることを確かめる。
/// 組めなければ `None`（一致しない扱い）にして、本番で panic しない。
pub(crate) type Re = LazyLock<Option<Regex>>;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        pub(crate) static $name: $crate::text::Re =
            ::std::sync::LazyLock::new(|| ::regex::Regex::new($pat).ok());
    };
}
pub(crate) use re;

pub(crate) fn is_match(r: &Re, s: &str) -> bool {
    r.as_ref().is_some_and(|r| r.is_match(s))
}

pub(crate) fn caps<'a>(r: &Re, s: &'a str) -> Option<Captures<'a>> {
    r.as_ref()?.captures(s)
}

/// 最初の一致を、閉じた関数の結果で置き換える（JS の非 global な `replace`）。
pub(crate) fn replace_first(r: &Re, s: &str, f: impl Fn(&Captures<'_>) -> String) -> String {
    match r.as_ref() {
        Some(r) => r.replacen(s, 1, |c: &Captures<'_>| f(c)).into_owned(),
        None => s.to_string(),
    }
}

/// キャプチャの文字列（無ければ空）。
pub(crate) fn cap(c: &Captures<'_>, i: usize) -> String {
    c.get(i)
        .map_or_else(String::new, |m| m.as_str().to_string())
}

/// 上限（`@imanect/yui` の LIMITS のうち、操作を持つ種類で使うもの）。
pub(crate) const TEXT_LENGTH: usize = 200;
pub(crate) const STEPS: usize = 15;
pub(crate) const APP_SECTIONS: usize = 6;
pub(crate) const APP_SECTION_FIELDS: usize = 20;
pub(crate) const APP_FIELDS: usize = 40;
pub(crate) const APP_SECTION_ACTIONS: usize = 3;
pub(crate) const APP_LABEL: usize = 40;
pub(crate) const ASK_QUESTIONS: usize = 8;
pub(crate) const ASK_OPTIONS: usize = 12;
pub(crate) const PLAN_STEPS: usize = 12;

/// JavaScript の `String.prototype.length`（UTF-16 の単位）。
pub(crate) fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// 先頭から UTF-16 の単位で n まで（サロゲートの途中では切らない）。
pub(crate) fn js_slice(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut len = 0;
    for ch in s.chars() {
        let l = ch.len_utf16();
        if len + l > n {
            break;
        }
        len += l;
        out.push(ch);
    }
    out
}

pub(crate) fn nfkc(s: &str) -> String {
    s.nfkc().collect()
}

/// 診断（行番号と code）。文言は TS 版と同じ意味で、一字一句は揃えない。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Diagnostic {
    pub line: usize,
    pub severity: &'static str,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Default)]
pub(crate) struct Diags {
    pub(crate) list: Vec<Diagnostic>,
    once: HashSet<&'static str>,
}

impl Diags {
    pub(crate) fn add(
        &mut self,
        line: usize,
        severity: &'static str,
        code: &'static str,
        message: impl Into<String>,
    ) {
        self.list.push(Diagnostic {
            line,
            severity,
            code,
            message: message.into(),
        });
    }

    /// 同じ code を 1 度だけ報告する（上限超過など）。
    pub(crate) fn add_once(
        &mut self,
        line: usize,
        severity: &'static str,
        code: &'static str,
        message: impl Into<String>,
    ) {
        if self.once.insert(code) {
            self.add(line, severity, code, message);
        }
    }
}

/// ブロック内の 1 行（行番号は 1 始まり）。
#[derive(Debug, Clone)]
pub(crate) struct Line {
    pub(crate) no: usize,
    pub(crate) text: String,
}

re!(REFS_TAIL, r"\s*((?:\[[0-9]+\])+)\s*$");
re!(REF_ONE, r"\[([0-9]+)\]");

/// 末尾の引用番号 `[1][2]` を取り外す。
pub(crate) fn take_refs(text: &str) -> (String, Vec<u32>) {
    let Some(c) = caps(&REFS_TAIL, text) else {
        return (text.trim().to_string(), Vec::new());
    };
    let whole = c.get(0).map_or(text.len(), |m| m.start());
    let refs = REF_ONE.as_ref().map_or_else(Vec::new, |r| {
        r.captures_iter(&cap(&c, 1))
            .filter_map(|x| x.get(1)?.as_str().parse().ok())
            .collect()
    });
    (text[..whole].trim().to_string(), refs)
}

/// 長すぎるテキストを切り詰める。
pub(crate) fn clip(text: &str, line: usize, diags: &mut Diags) -> String {
    if js_len(text) <= TEXT_LENGTH {
        return text.to_string();
    }
    diags.add(
        line,
        "warning",
        "text_too_long",
        format!("{TEXT_LENGTH} 文字を超える部分を省略しました"),
    );
    format!("{}…", js_slice(text, TEXT_LENGTH - 1))
}

re!(LIST_MARKER, r"^(?:[-*+・•]|[0-9]{1,3}[.)])\s+");

/// 行頭の箇条書き記号を取り外す。
pub(crate) fn strip_list_marker(text: &str) -> (String, bool) {
    match LIST_MARKER.as_ref().and_then(|r| r.find(text)) {
        Some(m) => (text[m.end()..].to_string(), true),
        None => (text.to_string(), false),
    }
}

/// `|` で区切られているか（エスケープ `\|` は除く）。
pub(crate) fn has_pipe(text: &str) -> bool {
    let mut prev = None;
    for ch in text.chars() {
        if ch == '|' && prev != Some('\\') {
            return true;
        }
        prev = Some(ch);
    }
    false
}

/// セルに分割する。`|` 区切り（GFM の表の前後 `|` も可）、`|` が無ければタブ区切り。
pub(crate) fn split_cells(text: &str) -> Vec<String> {
    let mut t = text.trim().to_string();
    if has_pipe(&t) {
        if let Some(rest) = t.strip_prefix('|') {
            t = rest.to_string();
        }
        if t.ends_with('|') && !t.ends_with("\\|") {
            t.pop();
        }
        let mut cells = Vec::new();
        let mut cur = String::new();
        let chars: Vec<char> = t.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            if ch == '\\' && chars.get(i + 1) == Some(&'|') {
                cur.push('|');
                i += 1;
            } else if ch == '|' {
                cells.push(cur.trim().to_string());
                cur.clear();
            } else {
                cur.push(ch);
            }
            i += 1;
        }
        cells.push(cur.trim().to_string());
        return cells;
    }
    if t.contains('\t') {
        return t
            .split('\t')
            .filter(|c| !c.is_empty())
            .map(|c| c.trim().to_string())
            .collect();
    }
    vec![t]
}

re!(SEP_CELL, r"^:?-{2,}:?$");

/// GFM の区切り行 `|---|:--:|` か。
pub(crate) fn is_separator_row(cells: &[String]) -> bool {
    !cells.is_empty() && cells.iter().all(|c| c.is_empty() || is_match(&SEP_CELL, c))
}

re!(
    OPTION_LINE,
    r"^([A-Za-z_]+|注記|注|単位|出典|方向|向き)\s*[:：]\s*(.*)$"
);

/// `note:` `src:` のオプション行なら、正規化したキーと値を返す（操作を持つ種類で使うのはこの 2 つだけ）。
pub(crate) fn parse_option(text: &str) -> Option<(&'static str, String)> {
    let c = caps(&OPTION_LINE, text.trim())?;
    let key = cap(&c, 1);
    let k = match key.to_lowercase().as_str() {
        "note" => "note",
        "src" | "source" => "src",
        _ => match key.as_str() {
            "注記" | "注" => "note",
            "出典" => "src",
            _ => return None,
        },
    };
    Some((k, cap(&c, 2).trim().to_string()))
}

/// 行頭の字下げの幅（空白 1、タブ 4、全角空白 2）。
pub(crate) fn indent_of(raw: &str) -> usize {
    let mut n = 0;
    for ch in raw.chars() {
        match ch {
            ' ' => n += 1,
            '\t' => n += 4,
            '\u{3000}' => n += 2,
            _ => break,
        }
    }
    n
}

re!(SPLIT_TITLE, r"^(.+?)(?:：|:\s+)(.+)$");

/// `名前: 説明` を見出しと説明に分ける（`10:00` のような時刻は分けない）。
pub(crate) fn split_title(text: &str) -> (String, Option<String>) {
    let t = text.trim();
    match caps(&SPLIT_TITLE, t) {
        Some(c) => (
            cap(&c, 1).trim().to_string(),
            Some(cap(&c, 2).trim().to_string()),
        ),
        None => (t.to_string(), None),
    }
}

/// 重複を除いて順を保つ（JS の `[...new Set(a)]`）。
pub(crate) fn uniq<T: PartialEq + Clone>(items: &[T]) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for x in items {
        if !out.contains(x) {
            out.push(x.clone());
        }
    }
    out
}

/// JavaScript の `Number(s)`（文字列）。空は 0、読めなければ NaN。
pub(crate) fn js_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    // JS と違う読み方（`inf`・16 進）は、どちらも有限でない・読めない扱いになり検証で弾かれる
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// 数値を JavaScript の `String(n)` と同じ形に（整数は小数点なし）。
pub(crate) fn js_num_str(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

/// 数値を JSON に（整数は整数として）。
pub(crate) fn json_num(n: f64) -> serde_json::Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        serde_json::Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map_or(serde_json::Value::Null, serde_json::Value::Number)
    }
}

#[cfg(test)]
mod pattern_tests {
    use super::*;

    /// 組めないパターンは「一致しない」扱いになり黙って壊れるので、すべて組めることを確かめる。
    #[test]
    fn patterns_compile() {
        for r in [
            &REFS_TAIL,
            &REF_ONE,
            &LIST_MARKER,
            &SEP_CELL,
            &OPTION_LINE,
            &SPLIT_TITLE,
        ] {
            assert!(r.is_some());
        }
    }
}
