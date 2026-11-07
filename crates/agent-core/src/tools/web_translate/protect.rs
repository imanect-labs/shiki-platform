//! ③ 保護: 訳してはいけない片をプレースホルダへ置き換える（設計メモ §4.1）。
//!
//! 置き換えた片は復元表に持ち、**LLM には見せない**（URL を書き換えさせない）。
//!
//! | 種類 | 対象 |
//! | --- | --- |
//! | `C` | フェンスコードブロック（行ごと） |
//! | `c` | インラインコード |
//! | `M` | 数式（`$…$`・`$$…$$`） |
//! | `I` | 画像 `![alt](url)`（alt は v1 では訳さない） |
//! | `F` | 脚注の参照 `[\[n\]](#…)` |
//! | `U` | リンクの URL 部分（テキストは訳す） |
//! | `A` | 自動リンク `<https://…>` |
//! | `T` | 境界タグの文字列（`<source>` 等） |
//! | `B` | 本文に元からある `⟦` `⟧`（復元の最後に戻す） |
//!
//! **本文に元からある `⟦` `⟧` は最初に `B` へ逃がす。** 以降の置き換えの元の文字列にも
//! 含まれ得るので、復元は入れ子を外れるまで繰り返し、`B` だけは最後に 1 回戻す（[`Protected::restore`]）。

use std::collections::{HashMap, HashSet};

/// プレースホルダの括弧（本文に現れにくい字）。
pub(super) const OPEN: char = '⟦';
pub(super) const CLOSE: char = '⟧';

/// 訳文の境界タグ（本文に現れたらプレースホルダにする・§4.4）。小文字で照合する。
const BOUNDARY_TAGS: [&str; 6] = [
    "</source",
    "<source",
    "</context",
    "<context",
    "</web_page",
    "<web_page",
];

/// 本文に元からある `⟦` `⟧` を逃がすプレースホルダの種類（復元の最後に戻す）。
const BRACKET: char = 'B';

/// 復元の入れ子の深さの上限（`T` ⊂ `c` ⊂ `I` ⊂ … の段数より十分大きい）。
const MAX_RESTORE_PASSES: usize = 6;

/// プレースホルダ 1 つ（`⟦U12⟧` なら kind = `U`, n = 12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Placeholder {
    pub kind: char,
    pub n: u32,
}

impl Placeholder {
    pub(super) fn render(self) -> String {
        format!("{OPEN}{}{}{CLOSE}", self.kind, self.n)
    }
}

/// `s` の中のプレースホルダを位置つきで列挙する（括弧の字形まで一致したものだけ）。
pub(super) fn placeholders(s: &str) -> Vec<(usize, usize, Placeholder)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = s[from..].find(OPEN) {
        let start = from + rel;
        let body = &s[start + OPEN.len_utf8()..];
        if let Some((ph, len)) = parse_body(body) {
            let end = start + OPEN.len_utf8() + len;
            out.push((start, end, ph));
            from = end;
        } else {
            from = start + OPEN.len_utf8();
        }
    }
    out
}

/// `U12⟧…` の先頭からプレースホルダを読む（読めたら消費したバイト数も返す）。
fn parse_body(body: &str) -> Option<(Placeholder, usize)> {
    let mut chars = body.char_indices();
    let (_, kind) = chars.next()?;
    if !kind.is_ascii_alphabetic() {
        return None;
    }
    let digits: String = body[1..].chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 6 {
        return None;
    }
    let rest = &body[1 + digits.len()..];
    if !rest.starts_with(CLOSE) {
        return None;
    }
    Some((
        Placeholder {
            kind,
            n: digits.parse().ok()?,
        },
        1 + digits.len() + CLOSE.len_utf8(),
    ))
}

/// 保護済みの本文と復元表。
pub(super) struct Protected {
    /// プレースホルダに置き換えた本文（分割・翻訳の入力）。
    pub text: String,
    table: HashMap<String, String>,
    /// 単独の行に置かれたプレースホルダ（ブロックの片）。
    blocks: HashSet<String>,
    /// 原文の画像 URL（出所の判定に使う。LLM を通らない値）。
    pub image_urls: Vec<String>,
    counters: HashMap<char, u32>,
}

impl Protected {
    fn new() -> Self {
        Protected {
            text: String::new(),
            table: HashMap::new(),
            blocks: HashSet::new(),
            image_urls: Vec::new(),
            counters: HashMap::new(),
        }
    }

    fn put(&mut self, kind: char, original: &str) -> String {
        let n = self.counters.entry(kind).or_insert(0);
        *n += 1;
        let ph = Placeholder { kind, n: *n }.render();
        self.table.insert(ph.clone(), original.to_string());
        ph
    }

    /// プレースホルダが単独の行に置かれていた（ブロックの片）か。
    pub(super) fn is_block(&self, ph: &str) -> bool {
        self.blocks.contains(ph)
    }

    /// 復元表にあるプレースホルダか（LLM が捏造した番号を見分ける）。
    pub(super) fn knows(&self, ph: &str) -> bool {
        self.table.contains_key(ph)
    }

    /// プレースホルダを原文へ戻す（入れ子を外れるまで繰り返す）。
    ///
    /// 表に無いプレースホルダ（捏造）はそのまま残る。検査（⑦）で弾くので通常は現れない。
    ///
    /// 本文に元からあった括弧（`B`）は**最後に 1 回だけ**戻す。入れ子の展開と同じ周回で戻すと、
    /// 原文の `⟦c1⟧` という文字列が次の周回で本物のプレースホルダとして置き換わる。
    pub(super) fn restore(&self, s: &str) -> String {
        let mut cur = s.to_string();
        for _ in 0..MAX_RESTORE_PASSES {
            let (next, changed) = self.replace_once(&cur, |kind| kind != BRACKET);
            cur = next;
            if !changed {
                break;
            }
        }
        self.replace_once(&cur, |kind| kind == BRACKET).0
    }

    /// `pick` に当たる種類のプレースホルダを 1 周だけ置き換える（置き換えたかも返す）。
    fn replace_once(&self, s: &str, pick: impl Fn(char) -> bool) -> (String, bool) {
        let mut out = String::with_capacity(s.len());
        let (mut last, mut changed) = (0, false);
        for (a, b, ph) in placeholders(s) {
            if !pick(ph.kind) {
                continue;
            }
            if let Some(original) = self.table.get(&s[a..b]) {
                out.push_str(&s[last..a]);
                out.push_str(original);
                last = b;
                changed = true;
            }
        }
        out.push_str(&s[last..]);
        (out, changed)
    }
}

/// 本文を保護する。
pub(super) fn protect(markdown: &str) -> Protected {
    let mut p = Protected::new();
    // 0. 本文に元からある括弧を逃がす（以降の元の文字列にも残り得るので最初に行う）。
    let text = escape_brackets(&mut p, markdown);
    // 1. フェンスコードブロック（行単位）。
    let text = protect_fences(&mut p, &text);
    // 1b. 別行立ての数式（`$$` だけの行で開いて閉じる複数行）。
    let text = protect_display_math_blocks(&mut p, &text);
    // 2. インラインコード（同じ行の中で、同じ長さのバッククォートの連なりで閉じる）。
    let text = map_lines(&text, |line| protect_code_spans(&mut p, line));
    // 3. 数式。
    let text = map_lines(&text, |line| protect_math(&mut p, line));
    // 3b. 行き先を落とされたリンクの名残 `[text]` を外す（コード・数式は保護済みなので触れない）。
    let text = map_lines(&text, unwrap_bare_labels);
    // 4. 画像 → 5. 脚注の参照とリンクの URL → 6. 自動リンク。
    let text = map_lines(&text, |line| protect_images(&mut p, line));
    let text = map_lines(&text, |line| protect_links(&mut p, line));
    let text = map_lines(&text, |line| protect_autolinks(&mut p, line));
    // 7. 境界タグ。
    let text = protect_boundary_tags(&mut p, &text);
    // 単独の行にあるプレースホルダをブロックとして記録する。
    for line in text.lines() {
        let t = line.trim();
        if let [(0, end, _)] = placeholders(t).as_slice() {
            if *end == t.len() {
                p.blocks.insert(t.to_string());
            }
        }
    }
    p.text = text;
    p
}

fn escape_brackets(p: &mut Protected, s: &str) -> String {
    if !s.contains([OPEN, CLOSE]) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == OPEN || c == CLOSE {
            let ph = p.put(BRACKET, &c.to_string());
            out.push_str(&ph);
        } else {
            out.push(c);
        }
    }
    out
}

/// 行ごとに変換する（改行は保つ）。
fn map_lines(s: &str, mut f: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&f(line));
    }
    out
}

/// フェンスの開き（`` ``` `` か `~~~` が 3 つ以上）なら（字, 長さ）を返す。
fn fence_open(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    // 記号は ASCII なので、文字数がそのままバイト数になる。
    let n = t.chars().take_while(|&x| x == c).count();
    // バッククォートのフェンスの情報文字列にバッククォートは書けない（```x``` は行内のコード）。
    (n >= 3 && !(c == '`' && t[n..].contains('`'))).then_some((c, n))
}

fn protect_fences(table: &mut Protected, text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        let Some((mark, open_len)) = fence_open(line) else {
            out.push(line.to_string());
            at += 1;
            continue;
        };
        // 閉じ: 同じ字で開きの長さ以上・その字だけの行。閉じが無ければ末尾まで。
        let close = lines[at + 1..]
            .iter()
            .position(|l| {
                let t = l.trim();
                t.len() >= open_len && t.chars().all(|x| x == mark)
            })
            .map_or(lines.len() - 1, |k| at + 1 + k);
        let indent_len = line.len() - line.trim_start().len();
        let mut block = line[indent_len..].to_string();
        for l in &lines[at + 1..=close] {
            block.push('\n');
            block.push_str(l);
        }
        let ph = table.put('C', &block);
        out.push(format!("{}{ph}", &line[..indent_len]));
        at = close + 1;
    }
    out.join("\n")
}

/// `$$` だけの行で開き、`$$` だけの行で閉じる数式を 1 つのブロックにする（閉じが無ければ触らない）。
fn protect_display_math_blocks(table: &mut Protected, text: &str) -> String {
    /// 数式 1 本の行数の上限（閉じ忘れで本文を丸ごと飲み込まない）。
    const MAX_MATH_LINES: usize = 200;
    if !text.contains("$$") {
        return text.to_string();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        let close = (line.trim() == "$$")
            .then(|| {
                lines[at + 1..]
                    .iter()
                    .take(MAX_MATH_LINES)
                    .position(|l| l.trim() == "$$")
                    .map(|k| at + 1 + k)
            })
            .flatten();
        let Some(close) = close else {
            out.push(line.to_string());
            at += 1;
            continue;
        };
        let indent_len = line.len() - line.trim_start().len();
        let block = lines[at..=close].join("\n");
        let ph = table.put('M', &block[indent_len..]);
        out.push(format!("{}{ph}", &line[..indent_len]));
        at = close + 1;
    }
    out.join("\n")
}

fn protect_code_spans(p: &mut Protected, line: &str) -> String {
    if !line.contains('`') {
        return line.to_string();
    }
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut last = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
        // 同じ長さの連なりを探す（長さの違う連なりは中身の一部）。
        let mut k = i + run;
        let mut close = None;
        while k < bytes.len() {
            if bytes[k] == b'`' {
                let r = bytes[k..].iter().take_while(|&&b| b == b'`').count();
                if r == run {
                    close = Some(k);
                    break;
                }
                k += r;
            } else {
                k += 1;
            }
        }
        match close {
            Some(k) => {
                out.push_str(&line[last..i]);
                let ph = p.put('c', &line[i..k + run]);
                out.push_str(&ph);
                i = k + run;
                last = i;
            }
            None => i += run,
        }
    }
    out.push_str(&line[last..]);
    out
}

/// `$$…$$` と `$…$`（開きの直後・閉じの直前が空白でなく、閉じの直後が数字でない＝Pandoc と同じ規則）。
///
/// 規則を絞るのは、本文の金額（`$5 and $10`）を数式と取り違えないため。
fn protect_math(p: &mut Protected, line: &str) -> String {
    if !line.contains('$') {
        return line.to_string();
    }
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let (mut i, mut last) = (0, 0);
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let display = bytes.get(i + 1) == Some(&b'$');
        let open_len = if display { 2 } else { 1 };
        let start = i + open_len;
        let close = find_math_close(bytes, start, display);
        match close {
            Some(k) => {
                out.push_str(&line[last..i]);
                // 行内の `$…$` は表示で `\(…\)` にする。チャットの Markdown は金額を守るため単独の `$` を
                // 数式にしないので、`$` のままだと TeX が素の文字で出て `_` が強調に化ける。ここでは
                // 上の規則で数式と判定済みなので、表示側が確実に式と読む形に直してよい。
                let ph = if display {
                    p.put('M', &line[i..k + open_len])
                } else {
                    p.put('M', &format!("\\({}\\)", &line[start..k]))
                };
                out.push_str(&ph);
                i = k + open_len;
                last = i;
            }
            None => i += open_len,
        }
    }
    out.push_str(&line[last..]);
    out
}

fn find_math_close(bytes: &[u8], start: usize, display: bool) -> Option<usize> {
    let first = *bytes.get(start)?;
    if !display && (first == b' ' || first == b'$') {
        return None;
    }
    let mut k = start;
    while k < bytes.len() {
        match bytes[k] {
            b'\\' => k += 2,
            b'$' if display => {
                return (bytes.get(k + 1) == Some(&b'$') && k > start).then_some(k);
            }
            b'$' => {
                let prev = bytes[k - 1];
                let next = bytes.get(k + 1);
                if k > start && prev != b' ' && !next.is_some_and(u8::is_ascii_digit) {
                    return Some(k);
                }
                return None;
            }
            _ => k += 1,
        }
    }
    None
}

/// `(` の直後 `start` から、対応する `)` の位置を返す（`<…>` 形・入れ子・エスケープを考慮）。
fn dest_end(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(start) == Some(&b'<') {
        let gt = s[start..].find('>')? + start;
        return (bytes.get(gt + 1) == Some(&b')')).then_some(gt + 1);
    }
    let mut depth = 0usize;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' if depth == 0 => return Some(i),
            b')' => {
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// `[` の位置 `open` に対応する `]` の位置（入れ子・エスケープを考慮）。
fn label_end(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' if depth == 0 => return Some(i),
            b']' => {
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// 画像の URL 部分（`<…>` やタイトル付きを外した素の URL）。
fn image_url(dest: &str) -> &str {
    let d = dest.trim();
    if let Some(inner) = d.strip_prefix('<') {
        return inner.split('>').next().unwrap_or("");
    }
    d.split_whitespace().next().unwrap_or("")
}

fn protect_images(p: &mut Protected, line: &str) -> String {
    if !line.contains("![") {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(rel) = line[from..].find("![") {
        let bang = from + rel;
        let Some(rb) = label_end(line, bang + 1) else {
            break;
        };
        if line.as_bytes().get(rb + 1) != Some(&b'(') {
            from = rb + 1;
            continue;
        }
        let Some(rp) = dest_end(line, rb + 2) else {
            from = rb + 1;
            continue;
        };
        out.push_str(&line[last..bang]);
        let url = image_url(&line[rb + 2..rp]);
        if !url.is_empty() {
            p.image_urls.push(url.to_string());
        }
        let ph = p.put('I', &line[bang..=rp]);
        out.push_str(&ph);
        last = rp + 1;
        from = last;
    }
    out.push_str(&line[last..]);
    out
}

/// 取得時に行き先を落とされたリンク（断片のみ `#bib.bib10` など）は `[text]` の形で残る。
/// 表示ではこれが文字どおりの角括弧になり、arXiv の引用 `[<a>10</a>]` が `[[10]]` に、
/// `<a>Smith et al. [3]</a>` が `[Smith et al. [3]]` に化けるので、外枠の `[` `]` を外す。
/// 原文の角括弧は `\[` `\]` にエスケープされて届くため、エスケープの無い対はリンクの名残に限られる。
/// 次が `(`（リンク・画像）・`[`（参照リンク）・`:`（参照定義）の対と、チェックボックス（`[ ]` `[x]`）は残す。
fn unwrap_bare_labels(line: &str) -> String {
    if !line.contains('[') {
        return line.to_string();
    }
    let bytes = line.as_bytes();
    // 外す `[` `]` の位置（バイト単位の印。位置の検索を文字ごとに線形にしない）。
    let mut drop = vec![false; bytes.len()];
    let mut dropped = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let Some(rb) = label_end(line, i) else {
            i += 1;
            continue;
        };
        let after = bytes.get(rb + 1).copied();
        let label = &line[i + 1..rb];
        let keep = (i > 0 && bytes[i - 1] == b'!')
            || matches!(after, Some(b'(' | b'[' | b':'))
            || matches!(label, "" | " " | "x" | "X");
        if keep {
            // リンク・画像のテキストの中と、参照リンクの参照名（`[text][ref]` の `[ref]`）は触らない。
            i = match after {
                Some(b'[') => label_end(line, rb + 1).map_or(rb + 1, |e| e + 1),
                _ => rb + 1,
            };
            continue;
        }
        drop[i] = true;
        drop[rb] = true;
        dropped = true;
        // 名残の中にさらに名残があり得るので、中身も続けて見る。
        i += 1;
    }
    if !dropped {
        return line.to_string();
    }
    line.char_indices()
        .filter(|&(at, _)| !drop[at])
        .map(|(_, c)| c)
        .collect()
}

/// 脚注の参照の見た目（`\[12\]`・`\[a\]`・`\[note 3\]`）か。
fn is_footnote_label(label: &str) -> bool {
    let Some(inner) = label
        .strip_prefix("\\[")
        .and_then(|s| s.strip_suffix("\\]"))
    else {
        return false;
    };
    !inner.is_empty()
        && inner.chars().count() <= 12
        && !inner.contains(['[', ']', '\n'])
        && inner.chars().any(char::is_alphanumeric)
}

fn protect_links(p: &mut Protected, line: &str) -> String {
    if !line.contains("](") {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    let mut i = 0;
    let bytes = line.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let Some(rb) = label_end(line, i) else {
            i += 1;
            continue;
        };
        if bytes.get(rb + 1) != Some(&b'(') {
            i += 1;
            continue;
        }
        let Some(rp) = dest_end(line, rb + 2) else {
            i += 1;
            continue;
        };
        out.push_str(&line[last..i]);
        if is_footnote_label(&line[i + 1..rb]) {
            let ph = p.put('F', &line[i..=rp]);
            out.push_str(&ph);
        } else {
            out.push_str(&line[i..rb + 2]);
            let ph = p.put('U', &line[rb + 2..rp]);
            out.push_str(&ph);
            out.push(')');
        }
        last = rp + 1;
        i = last;
    }
    out.push_str(&line[last..]);
    out
}

fn protect_autolinks(p: &mut Protected, line: &str) -> String {
    if !line.contains("<http") {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(rel) = line[from..].find("<http") {
        let lt = from + rel;
        let Some(gt) = line[lt..].find('>').map(|g| g + lt) else {
            break;
        };
        let inner = &line[lt + 1..gt];
        if inner.contains(char::is_whitespace)
            || !(inner.starts_with("http://") || inner.starts_with("https://"))
        {
            from = lt + 1;
            continue;
        }
        out.push_str(&line[last..lt]);
        let ph = p.put('A', &line[lt..=gt]);
        out.push_str(&ph);
        last = gt + 1;
        from = last;
    }
    out.push_str(&line[last..]);
    out
}

fn protect_boundary_tags(p: &mut Protected, s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    if !BOUNDARY_TAGS.iter().any(|t| lower.contains(t)) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut last = 0;
    while i < s.len() {
        if s.as_bytes()[i] == b'<' {
            if let Some(tag) = BOUNDARY_TAGS.iter().find(|t| lower[i..].starts_with(*t)) {
                out.push_str(&s[last..i]);
                let ph = p.put('T', &s[i..i + tag.len()]);
                out.push_str(&ph);
                i += tag.len();
                last = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_restores_the_original() {
        let md = "# Title ⟦x⟧\n\nSee [the docs](https://e.x/a_(b)) and `code` ![fig](https://e.x/i.png \"t\").\n\n\
                  ```rust\nlet a = `b`; // [x](y)\n```\n\nMath $x^2$ and $$\\sum_i a_i$$ cost $5 and $10.\n\
                  Ref[\\[12\\]](#cite-12) <https://e.x/auto> </source> <Context>";
        let p = protect(md);
        // 行内の `$…$` だけは表示で式と読まれる `\(…\)` に直る（それ以外は原文のまま）。
        assert_eq!(p.restore(&p.text), md.replace("$x^2$", "\\(x^2\\)"));
        // LLM に見える本文には URL・コード・数式・括弧・境界タグが残らない。
        for leaked in ["https://", "`", "$x", "\\sum", "</source", "<Context", "⟦x"] {
            assert!(!p.text.contains(leaked), "{leaked} が残った: {}", p.text);
        }
        // 金額は数式にしない。
        assert!(p.text.contains("cost $5 and $10."), "{}", p.text);
        // リンクのテキストは訳す対象として残る。
        assert!(p.text.contains("[the docs](⟦U1⟧)"), "{}", p.text);
        assert!(p.text.contains("Ref⟦F1⟧"), "{}", p.text);
        assert_eq!(p.image_urls, vec!["https://e.x/i.png".to_string()]);
    }

    #[test]
    fn fences_become_block_placeholders_with_indent() {
        let md = "- item\n\n  ```\n  code\n  ```\n\nafter";
        let p = protect(md);
        assert_eq!(p.text, "- item\n\n  ⟦C1⟧\n\nafter");
        assert!(p.is_block("⟦C1⟧"));
        assert_eq!(p.restore(&p.text), md);
    }

    /// 情報文字列にバッククォートを含む行はフェンスの開きではない（後ろの本文を丸ごと飲み込まない）。
    #[test]
    fn backticks_in_the_info_string_do_not_open_a_fence() {
        let p = protect("```x``` is code\n\nlater text");
        assert_eq!(p.text, "⟦c1⟧ is code\n\nlater text");
    }

    #[test]
    fn longer_fence_is_not_closed_by_a_shorter_run() {
        let md = "````\n```\ninner\n```\n````\ntext";
        let p = protect(md);
        assert_eq!(p.text, "⟦C1⟧\ntext");
        assert_eq!(p.restore(&p.text), md);
    }

    #[test]
    fn image_inside_link_keeps_both_parts_protected() {
        let md = "[![logo](https://e.x/l.png)](https://e.x/home)";
        let p = protect(md);
        assert_eq!(p.text, "[⟦I1⟧](⟦U1⟧)");
        assert_eq!(p.restore(&p.text), md);
    }

    /// 原文に元からあるプレースホルダの形の文字列は、復元で本物と取り違えない。
    #[test]
    fn literal_placeholder_text_in_the_source_is_kept() {
        let md = "Literal ⟦c1⟧ and `actual code`";
        let p = protect(md);
        assert_eq!(p.restore(&p.text), md);
    }

    #[test]
    fn multi_line_display_math_is_one_block() {
        let md = "Before\n\n$$\nx = \\text{distance}\n$$\n\nAfter";
        let p = protect(md);
        assert_eq!(p.text, "Before\n\n⟦M1⟧\n\nAfter");
        assert!(p.is_block("⟦M1⟧"));
        assert_eq!(p.restore(&p.text), md);
        // 閉じの無い `$$` は触らない。
        let p = protect("$$\nunclosed");
        assert_eq!(p.text, "$$\nunclosed");
    }

    /// 取得で行き先を落とされたリンクの名残は角括弧ごと外す（arXiv の引用が `[[10]]` に化けない）。
    #[test]
    fn dropped_link_leftovers_lose_their_brackets() {
        let md = "shown \\[[10]\\] and [BehnamGhader et al. \\[37\\]]; note [\\[1\\]] and [節へ].";
        let p = protect(md);
        assert_eq!(
            p.text,
            "shown \\[10\\] and BehnamGhader et al. \\[37\\]; note \\[1\\] and 節へ."
        );
        // リンク・画像・参照リンク・チェックボックス・コード・数式の中は残す。
        let md = "- [ ] todo [a](https://e.x) ![b](https://e.x/i.png) [c][r] `[d]` $$x[1]$$\n[r]: https://e.x";
        let p = protect(md);
        assert_eq!(p.restore(&p.text), md);
    }

    /// 行内の `$…$` は `\(…\)` で戻す（チャットの Markdown は単独の `$` を式にしない）。金額は触らない。
    #[test]
    fn inline_dollar_math_is_restored_as_paren_math() {
        let md = "given $\\mathbf{T}_{\\text{in}}$ of length $L$, pay $5 and $10.";
        let p = protect(md);
        assert_eq!(
            p.restore(&p.text),
            "given \\(\\mathbf{T}_{\\text{in}}\\) of length \\(L\\), pay $5 and $10."
        );
    }

    #[test]
    fn placeholders_require_the_exact_brackets() {
        assert_eq!(placeholders("a ⟦C7⟧ b ⟦C7⟫ ⟦x⟧ ⟦U12⟧").len(), 2);
    }
}
