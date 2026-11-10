//! ⑦ 検査: 訳文を固定ルールで調べ、外れた理由を返す（設計メモ §4.5）。
//!
//! 理由の文言は**こちらが生成した固定文言＋プレースホルダ名だけ**で組む（ページの文字列を
//! 再試行の指示に混ぜない）。再試行に理由を添えると、同じ入力のやり直しより直りやすい（実測 0/3 → 4/5）。

use std::collections::HashMap;
use std::fmt::Write as _;

use super::chunk::is_heading;
use super::protect::{placeholders, Protected, CLOSE, OPEN};

/// 段落の訳し残しの理由の接頭辞。**弱い不合格**として扱い、最後の試行でこれしか残らなければ
/// その訳を採る（訳せた段落まで原文へ戻さない。引用文献の一覧のように原文のままが妥当な段落もある）。
pub(super) const UNTRANSLATED_PREFIX: &str = "untranslated:";

/// リンクの行き先だけが落ちた理由の接頭辞（弱い不合格・[`UNTRANSLATED_PREFIX`] と同じ扱い）。
pub(super) const LINKS_PREFIX: &str = "links:";

/// 理由が弱い不合格だけか（最後の試行で採ってよいか）。
pub(super) fn only_soft(reasons: &[String]) -> bool {
    !reasons.is_empty()
        && reasons
            .iter()
            .all(|r| r.starts_with(UNTRANSLATED_PREFIX) || r.starts_with(LINKS_PREFIX))
}

/// 段落が訳すべき文のまま残っているか（訳す先の文字の系統と違う文字が、文として十分にある）。
fn left_untranslated(para: &str, target: Script) -> bool {
    /// 文とみなす最小の字数（固有名詞・数値だけの段落・表の短いセルを落とさない）。
    const MIN_LETTERS: usize = 40;
    let plain = strip_placeholders(para);
    let letters: Vec<char> = plain.chars().filter(|c| c.is_alphabetic()).collect();
    let cjk = letters.iter().filter(|c| is_cjk(**c)).count();
    match target {
        Script::Cjk => letters.len() - cjk >= MIN_LETTERS && cjk * 10 < letters.len(),
        Script::Other => cjk >= MIN_LETTERS / 2 && cjk * 2 > letters.len(),
    }
}

/// 理由の列の上限（再試行の指示を長くしすぎない）。
const MAX_REASONS: usize = 6;

/// 文字数比の検査を掛ける最小の長さ（短い片は比がぶれる）。
const MIN_CHARS_FOR_RATIO: usize = 40;

/// ゼロ幅・不可視の文字（実測で訳文に紛れ込み、検査をすり抜けた）。
const INVISIBLE: [char; 5] = ['\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}'];

/// 括弧に化けやすい字（実測で `⟦C7⟧` が `⟦C7⟫` に化けた）。
const LOOKALIKE_BRACKETS: [char; 6] = ['⟫', '⟪', '〚', '〛', '⟬', '⟭'];

/// 対象言語の文字の系統（同一の訳を許すか・文字数比の範囲を決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Script {
    /// 日本語・中国語・韓国語。
    Cjk,
    /// それ以外（英語・欧州語など）。
    Other,
}

impl Script {
    pub(super) fn of_lang(lang: &str) -> Self {
        let l = lang.to_ascii_lowercase();
        if ["ja", "zh", "ko"].iter().any(|p| l.starts_with(p)) {
            Script::Cjk
        } else {
            Script::Other
        }
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF)
}

/// プレースホルダを除いた本文（訳すべき文字が残っているかの判定に使う）。
pub(super) fn strip_placeholders(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for (a, b, _) in placeholders(s) {
        out.push_str(&s[last..a]);
        last = b;
    }
    out.push_str(&s[last..]);
    out
}

/// 訳すべき文字があるか（無ければ LLM に送らずそのまま通す）。
pub(super) fn has_translatable_text(s: &str) -> bool {
    strip_placeholders(s).chars().any(char::is_alphabetic)
}

/// 原文が既に対象言語の文字で書かれていそうか（同一の訳を許す）。
fn already_in_target(src: &str, target: Script) -> bool {
    let letters: Vec<char> = src.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return true;
    }
    let cjk = letters.iter().filter(|c| is_cjk(**c)).count();
    match target {
        Script::Cjk => cjk * 2 >= letters.len(),
        // 欧州語どうしは文字だけでは見分けられないので、同一は「訳さなかった」とみなさない
        // （固有名詞だけ・既に対象言語のチャンクを落とさない）。CJK が主なら訳し忘れ。
        Script::Other => cjk * 2 < letters.len(),
    }
}

/// 文字数比の許容範囲（en→ja の実測は 0.62〜0.64）。
fn ratio_range(src: &str, target: Script) -> (f64, f64) {
    let src_cjk = already_in_target(src, Script::Cjk);
    match (src_cjk, target) {
        (false, Script::Cjk) => (0.25, 1.5),
        (true, Script::Other) => (0.8, 6.0),
        _ => (0.4, 2.5),
    }
}

fn counts(s: &str) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for (a, b, _) in placeholders(s) {
        *m.entry(s[a..b].to_string()).or_insert(0) += 1;
    }
    m
}

fn list_items(s: &str) -> usize {
    s.lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("- ")
                || t.starts_with("* ")
                || t.starts_with("+ ")
                || t.split_once(". ").is_some_and(|(n, _)| {
                    !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit())
                })
        })
        .count()
}

fn headings(s: &str) -> Vec<usize> {
    s.lines().filter_map(is_heading).collect()
}

/// 表の各行の列数（`|` の数。インラインコードは保護済みなのでセル内の `|` は数えない）。
fn table_shape(s: &str) -> Vec<usize> {
    s.lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .map(|l| l.matches('|').count() - l.matches("\\|").count())
        .collect()
}

/// 空行で区切った段落ごとの、インラインのプレースホルダの多重集合。
fn inline_by_paragraph(s: &str, p: &Protected) -> Vec<Vec<String>> {
    paragraphs(s)
        .into_iter()
        .map(|para| {
            let mut v: Vec<String> = placeholders(para)
                .into_iter()
                .map(|(a, b, _)| para[a..b].to_string())
                .filter(|ph| !p.is_block(ph))
                .collect();
            v.sort();
            v
        })
        .collect()
}

/// 段落ごとの脚注の参照（`F`）の並び。
fn footnotes_by_paragraph(s: &str) -> Vec<Vec<String>> {
    paragraphs(s)
        .into_iter()
        .map(|para| {
            placeholders(para)
                .into_iter()
                .filter(|(_, _, ph)| ph.kind == 'F')
                .map(|(a, b, _)| para[a..b].to_string())
                .collect()
        })
        .collect()
}

fn paragraphs(s: &str) -> Vec<&str> {
    s.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// 単独の行に置かれたブロックのプレースホルダの並び。
fn block_order(s: &str, p: &Protected) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|t| p.is_block(t))
        .map(str::to_string)
        .collect()
}

/// `http(s)://` で始まる語（プレースホルダの外に書かれた URL）。
fn bare_urls(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = s[from..].find("http") {
        let at = from + rel;
        let rest = &s[at..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest
                .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '<'))
                .unwrap_or(rest.len());
            out.push(&rest[..end]);
            from = at + end.max(1);
        } else {
            from = at + 4;
        }
    }
    out
}

/// リンクの `](⟦Un⟧)` が画像（`![…]`）に書き換えられていないか。`U` の直前の `](` から
/// 対応する `[` を遡り、その前が `!` なら画像。
fn link_became_image(s: &str, ph_start: usize) -> bool {
    let head = &s[..ph_start.saturating_sub(2)];
    let mut depth = 0usize;
    for (i, c) in head.char_indices().rev() {
        match c {
            ']' => depth += 1,
            '[' if depth == 0 => return head[..i].ends_with('!'),
            '[' => depth -= 1,
            '\n' => return false,
            _ => {}
        }
    }
    false
}

/// 訳文を調べる。空なら合格。
#[allow(clippy::too_many_lines)] // 検査項目を設計メモ §4.5 の順に 1 か所へ並べる（分けると漏れが見えない）。
pub(super) fn check(src: &str, out: &str, p: &Protected, target: Script) -> Vec<String> {
    let mut reasons = Vec::new();
    if out.trim().is_empty() {
        return vec!["the translation is empty".into()];
    }

    // プレースホルダ: 同じ種類・同じ回数（集合でなく出現回数）。
    let (a, b) = (counts(src), counts(out));
    let mut missing: Vec<&String> = a
        .iter()
        .filter(|(k, n)| b.get(*k).copied().unwrap_or(0) < **n)
        .map(|(k, _)| k)
        .collect();
    missing.sort();
    let mut extra: Vec<&String> = b
        .iter()
        .filter(|(k, n)| a.get(*k).copied().unwrap_or(0) < **n)
        .map(|(k, _)| k)
        .collect();
    extra.sort();
    // 落ちたのがリンクの行き先（U・A）だけなら弱い不合格（リンクが地の文になるだけで、内容は欠けない）。
    // リンクの密集した箇所（Wikipedia の情報ボックス）で、再試行しても 1〜2 本落ち続ける実測がある。
    let links_only = |ph: &&String| {
        placeholders(ph)
            .first()
            .is_some_and(|(_, _, x)| matches!(x.kind, 'U' | 'A'))
    };
    let soft_drop = !missing.is_empty() && extra.is_empty() && missing.iter().all(links_only);
    let dropped: Vec<String> = if soft_drop {
        missing.iter().map(|s| (*s).clone()).collect()
    } else {
        Vec::new()
    };
    if soft_drop {
        reasons.push(format!(
            "{LINKS_PREFIX} link targets dropped: {}; keep every link as [text](placeholder)",
            join_capped(&missing)
        ));
    } else if !missing.is_empty() {
        reasons.push(format!(
            "placeholders missing or dropped: {}",
            join_capped(&missing)
        ));
    }
    if !extra.is_empty() {
        let invented = extra.iter().any(|ph| !p.knows(ph));
        reasons.push(format!(
            "placeholders duplicated{}: {}",
            if invented { " or invented" } else { "" },
            join_capped(&extra)
        ));
    }
    // 括弧の化け・括弧だけの残骸。
    let real = placeholders(out).len();
    if out.matches(OPEN).count() != real || out.matches(CLOSE).count() != real {
        reasons.push(format!(
            "a placeholder bracket was altered; write placeholders exactly like {OPEN}C1{CLOSE}"
        ));
    } else if LOOKALIKE_BRACKETS
        .iter()
        .any(|c| out.matches(*c).count() > src.matches(*c).count())
    {
        reasons.push(format!(
            "a placeholder bracket was altered; use exactly {OPEN} and {CLOSE}"
        ));
    }

    // ブロックの片は単独の行のまま・順序も原文どおり。
    if block_order(src, p) != block_order(out, p) {
        reasons.push(
            "placeholders that stand alone on a line must stay alone on their own line, in the same order"
                .into(),
        );
    }
    // リンクの URL はリンクの中に・画像へ書き換えない。
    for (start, end, ph) in placeholders(out) {
        if ph.kind != 'U' {
            continue;
        }
        let in_link = out[..start].ends_with("](") && out[end..].starts_with(')');
        if !in_link {
            reasons.push(format!(
                "{} must stay as a link target: [text]({})",
                ph.render(),
                ph.render()
            ));
            break;
        }
        if link_became_image(out, start) {
            reasons.push(format!(
                "the link to {} was turned into an image; keep it as [text]({})",
                ph.render(),
                ph.render()
            ));
            break;
        }
    }
    // インラインの片は元の段落の中に（段落の中の順序は自由・脚注の参照だけ順序も一致）。
    // 弱い不合格として落ちたリンクは、段落の照合から外す（同じ欠落を二重に数えない）。
    let pi: Vec<Vec<String>> = inline_by_paragraph(src, p)
        .into_iter()
        .map(|v| v.into_iter().filter(|ph| !dropped.contains(ph)).collect())
        .collect();
    let po = inline_by_paragraph(out, p);
    if pi.len() != po.len() {
        reasons.push(format!(
            "paragraph count changed ({} -> {}); keep the same paragraph breaks",
            pi.len(),
            po.len()
        ));
    } else if pi != po {
        reasons.push("an inline placeholder moved to a different paragraph".into());
    } else if footnotes_by_paragraph(src) != footnotes_by_paragraph(out) {
        reasons.push("footnote markers (F) must keep their original order".into());
    }
    // 段落ごとの訳し残し（チャンクの一部の段落だけ原文のまま返す落ち方・Wikipedia の実測）。
    if pi.len() == po.len() {
        if let Some(n) = paragraphs(src)
            .iter()
            .zip(paragraphs(out))
            .position(|(a, b)| *a == b && left_untranslated(a, target))
        {
            reasons.push(format!(
                "{UNTRANSLATED_PREFIX} paragraph {} was left in the source language; translate every paragraph",
                n + 1
            ));
        }
    }

    // 構造: 見出しの数とレベル・リスト項目数・表の行数と列数。
    let (hs, ho) = (headings(src), headings(out));
    if hs != ho {
        reasons.push(format!(
            "headings changed (levels {hs:?} -> {ho:?}); keep the same headings"
        ));
    }
    let (ls, lo) = (list_items(src), list_items(out));
    if ls != lo {
        reasons.push(format!("list items changed ({ls} -> {lo})"));
    }
    let (ts, to) = (table_shape(src), table_shape(out));
    if ts.len() != to.len() {
        reasons.push(format!(
            "table rows changed ({} -> {}); keep every row",
            ts.len(),
            to.len()
        ));
    } else if ts != to {
        reasons.push("table columns changed; keep every cell and `|`".into());
    }

    // 原文に無いものを足さない（フェンス・リンク・画像・URL・不可視の文字）。
    if out.contains("```") || out.contains("~~~") {
        reasons.push("do not add code fences; output plain Markdown".into());
    }
    if strip_placeholders(out).contains("![") {
        reasons.push("do not add images".into());
    }
    if out
        .match_indices("](")
        .any(|(i, _)| !out[i + 2..].starts_with(OPEN))
    {
        reasons.push("do not add links; every link target must be a U placeholder".into());
    }
    let src_urls = bare_urls(src);
    if bare_urls(out).iter().any(|u| !src_urls.contains(u)) {
        reasons.push("do not add or change URLs".into());
    }
    if INVISIBLE
        .iter()
        .any(|c| out.matches(*c).count() > src.matches(*c).count())
    {
        reasons.push("do not insert zero-width or invisible characters".into());
    }

    // 訳したか（訳すべき文字があるのに原文と同一）・文字数比。
    let src_plain = strip_placeholders(src);
    let out_plain = strip_placeholders(out);
    if out.trim() == src.trim() && !already_in_target(&src_plain, target) {
        reasons.push("the text was not translated".into());
    }
    let (ls, lo) = (
        src_plain.trim().chars().count(),
        out_plain.trim().chars().count(),
    );
    if ls >= MIN_CHARS_FOR_RATIO {
        #[allow(clippy::cast_precision_loss)]
        let ratio = lo as f64 / ls as f64;
        let (min, max) = ratio_range(&src_plain, target);
        if ratio < min {
            reasons.push("the translation is much shorter than the source; translate everything, do not summarize".into());
        } else if ratio > max {
            reasons.push(
                "the translation is much longer than the source; do not add explanations".into(),
            );
        }
    }
    reasons.truncate(MAX_REASONS);
    reasons
}

fn join_capped(items: &[&String]) -> String {
    let shown: Vec<&str> = items.iter().take(8).map(|s| s.as_str()).collect();
    let mut s = shown.join(", ");
    if items.len() > shown.len() {
        let _ = write!(s, " (+{} more)", items.len() - shown.len());
    }
    s
}

/// 訳文の出力から、LLM がよく付ける包み（全体を囲むフェンス・境界タグ）を外す。
///
/// 原文はフェンスをすべて保護しているので、訳文の先頭と末尾がフェンスなら包みと判断できる。
pub(super) fn unwrap_output(raw: &str) -> String {
    let mut s = raw.trim();
    for (open, close) in [
        ("<source>", "</source>"),
        ("<translation>", "</translation>"),
    ] {
        if let Some(inner) = s.strip_prefix(open).and_then(|x| x.strip_suffix(close)) {
            s = inner.trim();
        }
    }
    // 改行の無い「```訳文```」は包みと判断できないので外さない（検査が不合格にする）。
    if s.starts_with("```") && s.ends_with("```") {
        if let Some(inner) = s
            .find('\n')
            .and_then(|nl| s.get(nl..s.len().saturating_sub(3)))
            .filter(|inner| !inner.contains("```"))
        {
            s = inner.trim_matches('\n');
        }
    }
    s.to_string()
}
