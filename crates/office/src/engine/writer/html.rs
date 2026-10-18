//! LLM が書いた HTML＋CSS を、LibreOffice の HTML 取り込みが確実に読める形へ整える（#725）。
//!
//! 実機で確かめた取り込みの性質に合わせる:
//! - `<style>` のクラス・子孫セレクタは効かない（インラインの style とセルの bgcolor・width 属性は効く）
//!   → CSS をセレクタで要素へ当て、インラインへ展開する。親の文字色・書体などは子へ配る
//! - 既存の文書へ差し込むと class から段落スタイルは作られない
//!   → `data-style` は段落の先頭に置くブックマーク `<a name="shiki-s-…">` で運ぶ（エンジンが当てて消す）
//! - 最初の段落は差し込み先の段落の書式を引き継ぐ → 先頭に番兵の段落を置く（エンジンが消す）
//!
//! 安全: 取り込みは外部の画像・スタイルシートを取りに行く（kit からの外向き通信）。許可リストの
//! タグ・属性だけを残し、画像は `data:` のみ、CSS の `url(…)` と `@import` は捨てる（PIT-68・PIT-23）。

use std::collections::HashMap;
use std::fmt::Write as _;

use dom_query::{Document, NodeId, NodeRef};

/// 取り込みの先頭と末尾に置く番兵の段落の文字列（writer_apply.js の IMPORT_SENTINEL と同じ）。
/// 取り込みの最初と最後の段落は差し込み先の段落と融合し、その書式を引き継ぐ。番兵に吸わせて捨てる。
pub const IMPORT_SENTINEL: &str = "\u{2063}SHIKI-IMPORT\u{2063}";

/// 1 回に差し込める HTML の大きさの上限（画像を含む）。
pub const MAX_HTML_BYTES: usize = 8 << 20;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HtmlError {
    #[error("HTML が大きすぎます（画像を含めて 8MB まで）")]
    TooLarge,
    #[error("HTML に本文がありません")]
    Empty,
    #[error("HTML の入れ子が深すぎます（{MAX_HTML_DEPTH} 段まで）")]
    TooDeep,
}

/// 要素の入れ子の上限。正規化は要素ごとに再帰するので、深い入れ子でスタックを溢れさせない。
pub const MAX_HTML_DEPTH: usize = 64;

/// HTML が段落 1 つだけ（`<p>` / `<h1>`〜`<h6>`・中に段落や表を持たない）で、data-style を書いていないか。
///
/// 1 段落を 1 段落で置き換えるとき、スタイルを書かなければ元の段落のスタイル（Title など）を引き継ぐ
/// （題を `<h1>` で書き直して表題が見出し 1 に変わる、を防ぐ・Gemma 4 の評価）。
pub fn single_plain_block(input: &str) -> bool {
    plain_block_tag(input).is_some()
}

/// [`single_plain_block`] の段落のタグ（`h1`・`p` など）。
pub fn plain_block_tag(input: &str) -> Option<String> {
    if !single_plain_block_inner(input) {
        return None;
    }
    let doc = Document::from(input);
    let body = doc.body()?;
    let only = body.children().into_iter().find(NodeRef::is_element)?;
    only.node_name().map(|n| n.to_ascii_lowercase())
}

fn single_plain_block_inner(input: &str) -> bool {
    if input.contains("data-style") {
        return false;
    }
    let doc = Document::from(input);
    let Some(body) = doc.body() else {
        return false;
    };
    let elements: Vec<NodeRef<'_>> = body
        .children()
        .into_iter()
        .filter(NodeRef::is_element)
        .collect();
    let [only] = elements.as_slice() else {
        return false;
    };
    let tag = only
        .node_name()
        .map(|n| n.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(tag.as_str(), "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") && !has_block_child(only)
}

/// body から最も深い要素までの段数が上限を超えるか（再帰せず、明示のスタックで数える）。
fn too_deep(root: &NodeRef<'_>) -> bool {
    let mut stack = vec![(*root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_HTML_DEPTH {
            return true;
        }
        for child in node.children() {
            if child.is_element() {
                stack.push((child, depth + 1));
            }
        }
    }
    false
}

/// 段落になる要素（data-style を付けられる・継承を受け取る）。
const BLOCK_TAGS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "td",
    "th",
    "blockquote",
    "pre",
    "div",
    "caption",
];

/// そのまま通す要素。これ以外は中身だけを残す（タグを捨てる）。script 等は中身ごと捨てる。
const ALLOWED_TAGS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "th",
    "td",
    "colgroup",
    "col",
    "caption",
    "br",
    "hr",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "s",
    "strike",
    "del",
    "sup",
    "sub",
    "span",
    "a",
    "blockquote",
    "pre",
    "code",
    "div",
    "img",
    "font",
];
const DROPPED_WITH_CONTENT: &[&str] = &[
    "script", "style", "iframe", "object", "embed", "link", "meta", "title", "head", "noscript",
    "template", "svg", "math", "form", "input", "button", "select", "textarea",
];

/// 親から子へ配る CSS プロパティ。
const INHERITED: &[&str] = &[
    "color",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "text-align",
    "line-height",
];

/// 文字の書式。段落に付けると段落スタイルを当て直したときに消えるため、段落の中の span へ移す。
const CHAR_PROPS: &[&str] = &[
    "color",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "text-decoration",
];

/// 段落の中に段落（ブロック・表・箇条書き）を持つか。知らないタグは中身を見る。
fn has_block_child(node: &NodeRef<'_>) -> bool {
    node.children().iter().any(|c| {
        if !c.is_element() {
            return false;
        }
        let t = c
            .node_name()
            .map(|t| t.to_ascii_lowercase())
            .unwrap_or_default();
        if DROPPED_WITH_CONTENT.contains(&t.as_str()) {
            return false;
        }
        BLOCK_TAGS.contains(&t.as_str())
            || matches!(t.as_str(), "ul" | "ol" | "table" | "hr")
            || (!ALLOWED_TAGS.contains(&t.as_str()) && has_block_child(c))
    })
}

/// 残す CSS プロパティ（LibreOffice の HTML 取り込みが解釈するもの）。
const ALLOWED_CSS: &[&str] = &[
    "color",
    "background",
    "background-color",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "text-decoration",
    "text-align",
    "text-indent",
    "line-height",
    "margin",
    "margin-left",
    "margin-right",
    "margin-top",
    "margin-bottom",
    "padding",
    "padding-left",
    "padding-right",
    "padding-top",
    "padding-bottom",
    "border",
    "border-top",
    "border-bottom",
    "border-left",
    "border-right",
    "border-collapse",
    "width",
    "height",
    "vertical-align",
    "page-break-before",
    "page-break-after",
    "break-after",
    "white-space",
];

type Decls = Vec<(String, String)>;

/// CSS の宣言の並び（`a: b; c: d`）を読む。危険な値（url・expression・@import）は捨てる。
fn parse_decls(css: &str) -> Decls {
    let mut out = Vec::new();
    for decl in css.split(';') {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let mut v = v.trim().trim_end_matches("!important").trim().to_string();
        let lower = v.to_ascii_lowercase();
        if lower.contains("url(")
            || lower.contains("expression(")
            || lower.contains("javascript:")
            || lower.contains('\\')
        {
            continue;
        }
        // break-before: page は page-break-before: always に写す（取り込みが読むのは後者）。
        let k = match k.as_str() {
            "break-before" if lower == "page" => {
                v = "always".into();
                "page-break-before".to_string()
            }
            "break-after" if lower == "page" => {
                v = "always".into();
                "page-break-after".to_string()
            }
            _ => k,
        };
        if ALLOWED_CSS.contains(&k.as_str()) && !v.is_empty() {
            out.push((k, v));
        }
    }
    out
}

/// `<style>` の本文を (セレクタの並び, 宣言) の規則にする（@ 規則とコメントは捨てる）。
fn parse_rules(css: &str) -> Vec<(String, Decls)> {
    let mut text = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        text.push_str(&rest[..i]);
        rest = rest[i + 2..].split_once("*/").map_or("", |(_, r)| r);
    }
    text.push_str(rest);
    // `@import …;` などの文（ブロックを持たない @ 規則）を取り除く。外部の取得を許さない。
    let mut cleaned = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find('@') {
        cleaned.push_str(&rest[..i]);
        let after = &rest[i..];
        let semi = after.find(';');
        let brace = after.find('{');
        rest = match (semi, brace) {
            (Some(s), Some(b)) if s < b => &after[s + 1..],
            (Some(s), None) => &after[s + 1..],
            // ブロックを持つ @ 規則（@media など）は、対応する } まで捨てる。
            (_, Some(b)) => {
                let mut depth = 0;
                let mut end = after.len();
                for (j, c) in after[b..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = b + j + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                &after[end.min(after.len())..]
            }
            (None, None) => "",
        };
    }
    cleaned.push_str(rest);
    let text = cleaned;
    let mut rules = Vec::new();
    for block in text.split('}') {
        let Some((sel, body)) = block.split_once('{') else {
            continue;
        };
        let sel = sel.trim();
        if sel.is_empty() || sel.starts_with('@') || sel.contains('@') {
            continue;
        }
        rules.push((sel.to_string(), parse_decls(body)));
    }
    rules
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

/// 列幅の % を px にするときの本文の幅（A4 の本文幅 160mm 相当）。取り込みは列の % を読まないが、
/// px の比は表の幅に合わせて伸縮されるので、比が保たれれば十分。
const PERCENT_BASE_PX: f64 = 605.0;

/// 列・セルの幅を HTML の width 属性の形（px の数値）にする。% は本文の幅に対する比として px にする。
fn column_width(v: &str) -> String {
    match v
        .trim()
        .strip_suffix('%')
        .and_then(|n| n.trim().parse::<f64>().ok())
    {
        Some(pct) => format!("{}", (pct / 100.0 * PERCENT_BASE_PX).round()),
        None => html_width(v),
    }
}

/// 幅の指定を HTML の width 属性の形（"40%" か px の数値）にする。mm・cm・pt・in は px へ換算する。
fn html_width(v: &str) -> String {
    let v = v.trim();
    if v.ends_with('%') {
        return v.to_string();
    }
    let units = [
        ("px", 1.0),
        ("mm", 96.0 / 25.4),
        ("cm", 96.0 / 2.54),
        ("pt", 96.0 / 72.0),
        ("in", 96.0),
    ];
    for (unit, scale) in units {
        if let Some(n) = v
            .strip_suffix(unit)
            .and_then(|n| n.trim().parse::<f64>().ok())
        {
            return format!("{}", (n * scale).round());
        }
    }
    v.to_string()
}

/// 文字の無い段落（空行のつもりの <p></p>）か。画像だけの段落は空ではない。
fn empty_block(tag: &str, node: &NodeRef<'_>) -> bool {
    matches!(tag, "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
        && node.text().trim().is_empty()
        && !node.children().iter().any(|c| {
            c.is_element()
                && c.node_name().is_some_and(|t| {
                    matches!(
                        t.to_ascii_lowercase().as_str(),
                        "img" | "table" | "ul" | "ol" | "p" | "div"
                    )
                })
        })
}

/// 書体か大きさの指定を含むか。
fn has_font(decls: &Decls) -> bool {
    ["font-size", "font-family", "font"]
        .iter()
        .any(|k| get(decls, k).is_some())
}

/// 段落の中の要素が書体か大きさを指定しているか（style・クラスの規則・<font>・等幅の <code> など）。
fn authored_font(cx: &Ctx, node: &NodeRef<'_>) -> bool {
    node.children().iter().any(|c| {
        if !c.is_element() {
            return false;
        }
        let tag = c
            .node_name()
            .map(|t| t.to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(
            tag.as_str(),
            "font" | "code" | "pre" | "tt" | "kbd" | "samp"
        ) {
            return true;
        }
        let inline = c.attr("style").map(|s| parse_decls(&s)).unwrap_or_default();
        has_font(&inline) || cx.rules.get(&c.id).is_some_and(has_font) || authored_font(cx, c)
    })
}

/// 取り込みの後にエンジンが段落へ当てる指定の印（t: 上の間隔・b: 下の間隔を HTML で指定した、
/// n: 次の段落と同じページに置く＝page-break-after: avoid）。
fn spacing_flags(decls: &Decls) -> String {
    let both = get(decls, "margin").is_some();
    let mut flags = String::new();
    if both || get(decls, "margin-top").is_some() {
        flags.push('t');
    }
    if both || get(decls, "margin-bottom").is_some() {
        flags.push('b');
    }
    let avoid = |k: &str| get(decls, k).is_some_and(|v| v.trim().eq_ignore_ascii_case("avoid"));
    if avoid("page-break-after") || avoid("break-after") {
        flags.push('n');
    }
    flags
}

/// スタイル名 → ブックマークの印の名前（`shiki-s-<16 進の UTF-8>-<連番>`）。
fn style_marker(name: &str, seq: usize) -> String {
    let hex = name.bytes().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    });
    format!("shiki-s-{hex}-{seq}")
}

struct Ctx {
    rules: HashMap<NodeId, Decls>,
    out: String,
    markers: usize,
}

fn set(decls: &mut Decls, k: &str, v: &str) {
    match decls.iter_mut().find(|(dk, _)| dk == k) {
        Some(e) => e.1 = v.to_string(),
        None => decls.push((k.to_string(), v.to_string())),
    }
}

fn get<'a>(decls: &'a Decls, k: &str) -> Option<&'a str> {
    decls
        .iter()
        .find(|(dk, _)| dk == k)
        .map(|(_, v)| v.as_str())
}

/// HTML（断片でも文書でもよい）を、取り込み用の完全な HTML 文書にする。
pub fn normalize_html(input: &str) -> Result<String, HtmlError> {
    if input.len() > MAX_HTML_BYTES {
        return Err(HtmlError::TooLarge);
    }
    let doc = Document::from(input);
    // <style> の規則を要素へ当てる（規則の出現順に重ね、インラインが最後に勝つ）。
    let mut rules: HashMap<NodeId, Decls> = HashMap::new();
    for style in doc.select("style").nodes() {
        for (selector, decls) in parse_rules(&style.text()) {
            let Some(sel) = doc.try_select(&selector) else {
                continue;
            };
            for node in sel.nodes() {
                let entry = rules.entry(node.id).or_default();
                for (k, v) in &decls {
                    set(entry, k, v);
                }
            }
        }
    }
    let Some(body) = doc.body() else {
        return Err(HtmlError::Empty);
    };
    if too_deep(&body) {
        return Err(HtmlError::TooDeep);
    }
    let mut cx = Ctx {
        rules,
        out: String::with_capacity(input.len() + 256),
        markers: 0,
    };
    for child in body.children() {
        walk(&mut cx, &child, &Vec::new());
    }
    if cx.out.trim().is_empty() {
        return Err(HtmlError::Empty);
    }
    Ok(format!(
        "<html><head><meta charset=\"utf-8\"></head><body><p>{IMPORT_SENTINEL}</p>{}<p>{IMPORT_SENTINEL}</p></body></html>",
        cx.out
    ))
}

/// 要素に残す属性（表・画像・リンクなどは取り込みが確実に読む属性へ写す）。
/// 残せない要素（外部の画像）は `None`。
fn element_attrs(node: &NodeRef<'_>, tag: &str, decls: &Decls) -> Option<String> {
    let mut attrs = String::new();
    match tag {
        "a" => {
            if let Some(href) = node.attr("href") {
                let h = href.trim().to_ascii_lowercase();
                if h.starts_with("http://") || h.starts_with("https://") || h.starts_with("mailto:")
                {
                    let _ = write!(attrs, " href=\"{}\"", escape_attr(href.trim()));
                }
            }
        }
        "img" => {
            let Some(src) = node.attr("src").filter(|s| {
                let l = s.trim().to_ascii_lowercase();
                l.starts_with("data:image/png;")
                    || l.starts_with("data:image/jpeg;")
                    || l.starts_with("data:image/gif;")
                    || l.starts_with("data:image/svg+xml;")
            }) else {
                // 外部の画像は取り込みに取りに行かせない（kit からの外向き通信）。
                return None;
            };
            let _ = write!(attrs, " src=\"{}\"", escape_attr(src.trim()));
            for a in ["width", "height", "alt"] {
                if let Some(v) = node.attr(a) {
                    let _ = write!(attrs, " {a}=\"{}\"", escape_attr(&v));
                }
            }
        }
        "td" | "th" => cell_attrs(node, decls, &mut attrs),
        "table" => {
            let width = get(decls, "width")
                .map(str::to_string)
                .or_else(|| node.attr("width").map(|v| v.to_string()));
            let _ = write!(
                attrs,
                " width=\"{}\"",
                escape_attr(&html_width(width.as_deref().unwrap_or("100%")))
            );
            let _ = write!(
                attrs,
                " cellspacing=\"0\" cellpadding=\"{}\"",
                node.attr("cellpadding")
                    .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
                    .as_deref()
                    .unwrap_or("4")
            );
            if get(decls, "margin-left") == Some("auto")
                || get(decls, "margin") == Some("0 auto")
                || node.attr("align").as_deref() == Some("center")
            {
                attrs.push_str(" align=\"center\"");
            }
            if let Some(b) = node
                .attr("border")
                .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
            {
                let _ = write!(attrs, " border=\"{b}\"");
            }
        }
        "col" => {
            if let Some(w) = get(decls, "width")
                .map(str::to_string)
                .or_else(|| node.attr("width").map(|v| v.to_string()))
            {
                let _ = write!(attrs, " width=\"{}\"", escape_attr(&column_width(&w)));
            }
            if let Some(n) = node
                .attr("span")
                .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
            {
                let _ = write!(attrs, " span=\"{n}\"");
            }
        }
        "ol" => {
            if let Some(s) = node
                .attr("start")
                .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
            {
                let _ = write!(attrs, " start=\"{s}\"");
            }
            if let Some(t) = node
                .attr("type")
                .filter(|v| ["1", "a", "A", "i", "I"].contains(&v.as_ref()))
            {
                let _ = write!(attrs, " type=\"{t}\"");
            }
        }
        "font" => {
            for a in ["color", "face", "size"] {
                if let Some(v) = node.attr(a) {
                    let _ = write!(attrs, " {a}=\"{}\"", escape_attr(&v));
                }
            }
        }
        _ => {}
    }
    Some(attrs)
}

/// 表のセルの属性（結合・塗り・幅・縦位置・揃え）。
fn cell_attrs(node: &NodeRef<'_>, decls: &Decls, attrs: &mut String) {
    for a in ["colspan", "rowspan"] {
        if let Some(v) = node
            .attr(a)
            .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
        {
            let _ = write!(attrs, " {a}=\"{v}\"");
        }
    }
    // 取り込みが確実に読むのは属性（bgcolor・width・valign・align）。
    if let Some(bg) = get(decls, "background-color")
        .or_else(|| get(decls, "background"))
        .or(node.attr("bgcolor").as_deref())
    {
        let _ = write!(
            attrs,
            " bgcolor=\"{}\"",
            escape_attr(bg.split_whitespace().next().unwrap_or(bg))
        );
    }
    if let Some(w) = get(decls, "width")
        .map(str::to_string)
        .or_else(|| node.attr("width").map(|v| v.to_string()))
    {
        let _ = write!(attrs, " width=\"{}\"", escape_attr(&column_width(&w)));
    }
    if let Some(v) = get(decls, "vertical-align") {
        let _ = write!(attrs, " valign=\"{}\"", escape_attr(v));
    }
    if let Some(a) = get(decls, "text-align") {
        let _ = write!(attrs, " align=\"{}\"", escape_attr(a));
    }
}

fn walk(cx: &mut Ctx, node: &NodeRef<'_>, inherited: &Decls) {
    if node.is_text() {
        cx.out.push_str(&escape_text(&node.text()));
        return;
    }
    if !node.is_element() {
        return;
    }
    let tag = node
        .node_name()
        .map(|t| t.to_ascii_lowercase())
        .unwrap_or_default();
    if DROPPED_WITH_CONTENT.contains(&tag.as_str()) {
        return;
    }
    if !ALLOWED_TAGS.contains(&tag.as_str()) {
        // 知らないタグ（section・article など）は中身だけを残す。
        for child in node.children() {
            walk(cx, &child, inherited);
        }
        return;
    }
    // この要素の CSS: 継承分 → <style> の規則 → インライン。
    let mut decls: Decls = Vec::new();
    let is_block = BLOCK_TAGS.contains(&tag.as_str());
    if is_block {
        for (k, v) in inherited {
            set(&mut decls, k, v);
        }
    }
    if let Some(r) = cx.rules.get(&node.id) {
        for (k, v) in r {
            set(&mut decls, k, v);
        }
    }
    // HTML 4 の align 属性（<p align="right">）。モデルがよく書くので text-align として受ける
    // （style が同じ項目を書いていればそちらが勝つ）。
    // 表の align は表の置き場所（文字の揃えではない・別に扱う）なので、セルへ継がせない。
    if let Some(align) = node.attr("align").filter(|_| tag != "table") {
        let a = align.trim().to_ascii_lowercase();
        if matches!(a.as_str(), "left" | "right" | "center" | "justify") {
            set(&mut decls, "text-align", &a);
        }
    }
    if let Some(inline) = node.attr("style") {
        for (k, v) in parse_decls(&inline) {
            set(&mut decls, &k, &v);
        }
    }
    // 子へ配る分（この要素で決まった継承プロパティ）。
    let mut pass: Decls = inherited.clone();
    for (k, v) in &decls {
        if INHERITED.contains(&k.as_str()) {
            set(&mut pass, k, v);
        }
    }
    let Some(mut attrs) = element_attrs(node, &tag, &decls) else {
        return;
    };
    // 段落の直下が文字だけなら、文字の書式は内側の span へ移す。
    let wrap: Vec<String> = if is_block && !has_block_child(node) {
        decls
            .iter()
            .filter(|(k, _)| CHAR_PROPS.contains(&k.as_str()))
            .map(|(k, v)| format!("{k}: {v}"))
            .collect()
    } else {
        Vec::new()
    };
    // 残す CSS をインラインへ（表の要素は属性へ写した分も style に残して害はない）。
    let style: Vec<String> = decls
        .iter()
        .filter(|(k, _)| !(tag == "table" && k == "width"))
        .filter(|(k, _)| wrap.is_empty() || !CHAR_PROPS.contains(&k.as_str()))
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    if !style.is_empty() {
        let _ = write!(attrs, " style=\"{}\"", escape_attr(&style.join("; ")));
    }
    if matches!(tag.as_str(), "br" | "hr" | "img" | "col") {
        let _ = write!(cx.out, "<{tag}{attrs}>");
        return;
    }
    let _ = write!(cx.out, "<{tag}{attrs}>");
    // 段落スタイルの指定は、段落の先頭のブックマークで運ぶ。
    if is_block {
        if let Some(st) = node.attr("data-style").filter(|s| !s.trim().is_empty()) {
            let marker = style_marker(st.trim(), cx.markers);
            cx.markers += 1;
            let _ = write!(cx.out, "<a name=\"{marker}\"></a>");
        }
        // 段落の間隔・書体を指定したかどうか。取り込みは指定の無い段落にも上 0・下 10pt 相当の間隔と
        // 既定の書体・大きさを直接付けるので、指定の無い段落はエンジンがスタイルの値へ戻す（指定したものだけ印で残す）。
        let mut flags = spacing_flags(&decls);
        if has_font(&decls) || authored_font(cx, node) {
            flags.push('f');
        }
        // 空の段落（空行のつもりの <p></p>・<p> </p>）は取り込みが捨てるので、空白を入れて印を付ける。
        if empty_block(&tag, node) {
            flags.push('e');
        }
        if !flags.is_empty() && !has_block_child(node) {
            let _ = write!(cx.out, "<a name=\"shiki-k-{flags}-{}\"></a>", cx.markers);
            cx.markers += 1;
        }
    }
    if !wrap.is_empty() {
        let _ = write!(cx.out, "<span style=\"{}\">", escape_attr(&wrap.join("; ")));
    }
    for child in node.children() {
        walk(cx, &child, &pass);
    }
    // 空の段落は &nbsp; を入れて取り込みに残させる（印 e でエンジンが空に戻す）。
    if empty_block(&tag, node) {
        cx.out.push_str("&nbsp;");
    }
    if !wrap.is_empty() {
        cx.out.push_str("</span>");
    }
    let _ = write!(cx.out, "</{tag}>");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn body(html: &str) -> String {
        let out = normalize_html(html).unwrap();
        let start =
            out.find(&format!("<p>{IMPORT_SENTINEL}</p>")).unwrap() + IMPORT_SENTINEL.len() + 7;
        let end = out.len() - "</body></html>".len() - IMPORT_SENTINEL.len() - 7;
        out[start..end].to_string()
    }

    #[test]
    fn inlines_class_and_descendant_rules() {
        let out = body(
            "<style>table.grid td { border: 1px solid #888; } th { background: #1F3864; color: #FFFFFF } .lead { color: #555 }</style>\
             <p class=\"lead\">x</p><table class=\"grid\"><tr><th>h</th></tr><tr><td>c</td></tr></table>",
        );
        // 文字の書式は段落ではなく内側の span へ（段落スタイルを当て直しても消えない）。
        assert!(
            out.contains("<p><span style=\"color: #555\">x</span></p>"),
            "{out}"
        );
        assert!(
            out.contains(
                "<th bgcolor=\"#1F3864\" style=\"background: #1F3864\"><span style=\"color: #FFFFFF\">h</span></th>"
            ),
            "{out}"
        );
        assert!(
            out.contains("<td style=\"border: 1px solid #888\">c</td>"),
            "{out}"
        );
        assert!(
            out.contains("<table width=\"100%\" cellspacing=\"0\" cellpadding=\"4\">"),
            "{out}"
        );
    }

    #[test]
    fn pushes_inherited_properties_down_to_blocks() {
        let out = body("<div style=\"font-family: 'Noto Sans JP'; color: #333; text-align: center\"><p>a</p><p style=\"color: red\">b</p></div>");
        assert!(
            out.contains(
                "<div style=\"font-family: 'Noto Sans JP'; color: #333; text-align: center\">"
            ),
            "{out}"
        );
        assert!(
            out.contains("<p style=\"text-align: center\"><a name=\"shiki-k-f-0\"></a><span style=\"font-family: 'Noto Sans JP'; color: #333\">a</span></p>"),
            "{out}"
        );
        assert!(
            out.contains("<span style=\"font-family: 'Noto Sans JP'; color: red\">b</span>"),
            "{out}"
        );
    }

    #[test]
    fn carries_paragraph_styles_as_bookmarks() {
        let out = body("<p data-style=\"Quotations\">q</p><h2 data-style=\"見出し\">h</h2>");
        assert!(
            out.contains("<p><a name=\"shiki-s-51756f746174696f6e73-0\"></a>q</p>"),
            "{out}"
        );
        assert!(
            out.contains("<h2><a name=\"shiki-s-e8a68be587bae38197-1\"></a>h</h2>"),
            "{out}"
        );
    }

    #[test]
    fn strips_external_fetches_and_scripts() {
        let out = body(
            "<style>@import url(http://evil/x.css); p { background: url(http://evil/a.png); color: red }</style>\
             <link rel=\"stylesheet\" href=\"http://evil/y.css\"><script>alert(1)</script>\
             <p>t<img src=\"http://evil/i.png\"><img src=\"data:image/png;base64,AAAA\" width=\"10\"></p>\
             <a href=\"javascript:alert(1)\">j</a><a href=\"https://example.com\">k</a><iframe src=\"http://x\"></iframe>",
        );
        assert!(!out.contains("evil"), "{out}");
        assert!(!out.contains("script"), "{out}");
        assert!(!out.contains("javascript"), "{out}");
        assert!(out.contains("<p><span style=\"color: red\">t<img src=\"data:image/png;base64,AAAA\" width=\"10\"></span></p>"), "{out}");
        assert!(
            out.contains("<a>j</a><a href=\"https://example.com\">k</a>"),
            "{out}"
        );
    }

    #[test]
    fn maps_page_breaks_and_keeps_merges() {
        let out = body("<p style=\"break-before: page\">p</p><table><tr><td colspan=\"2\" rowspan=\"x\" style=\"background-color:#FFF2CC;vertical-align:middle\">m</td></tr></table>");
        assert!(
            out.contains("<p style=\"page-break-before: always\">p</p>"),
            "{out}"
        );
        assert!(
            out.contains("<td colspan=\"2\" bgcolor=\"#FFF2CC\" valign=\"middle\""),
            "{out}"
        );
    }

    #[test]
    fn marks_paragraphs_that_set_their_own_spacing() {
        let out =
            body("<p style=\"margin-bottom: 12pt\">a</p><p>b</p><h2 style=\"margin: 0\">c</h2>");
        assert!(out.contains("<a name=\"shiki-k-b-0\"></a>a"), "{out}");
        assert!(out.contains("<p>b</p>"), "{out}");
        assert!(out.contains("<a name=\"shiki-k-tb-1\"></a>c"), "{out}");
        let out = body("<h2 style=\"page-break-after: avoid\">h</h2>");
        assert!(out.contains("<a name=\"shiki-k-n-0\"></a>h"), "{out}");
        // 書体・大きさを指定した段落（中の要素の指定も含む）だけ、取り込みの既定の書体を残す。
        let out = body("<style>.big { font-size: 20pt }</style><h2>a</h2><p>b <span class=\"big\">c</span></p><p>d <code>e</code></p>");
        assert!(out.contains("<h2>a</h2>"), "{out}");
        assert!(out.contains("<a name=\"shiki-k-f-0\"></a>b"), "{out}");
        assert!(out.contains("<a name=\"shiki-k-f-1\"></a>d"), "{out}");
    }

    #[test]
    fn keeps_empty_paragraphs_as_spacers() {
        let out = body("<p>a</p><p> </p><p></p><p>b</p>");
        assert!(
            out.contains("<p><a name=\"shiki-k-e-0\"></a> &nbsp;</p>"),
            "{out}"
        );
        assert!(
            out.contains("<p><a name=\"shiki-k-e-1\"></a>&nbsp;</p>"),
            "{out}"
        );
    }

    #[test]
    fn rejects_deep_nesting_without_overflowing_the_stack() {
        let deep = format!("{}x", "<span>".repeat(200_000));
        assert!(matches!(normalize_html(&deep), Err(HtmlError::TooDeep)));
        let ok = format!("{}x", "<span>".repeat(MAX_HTML_DEPTH - 4));
        assert!(normalize_html(&ok).is_ok());
    }

    #[test]
    fn detects_a_single_plain_paragraph() {
        assert!(single_plain_block("<h1>月次営業報告（2026年10月）</h1>"));
        assert!(single_plain_block("<p>本文 <b>強調</b></p>"));
        assert!(!single_plain_block("<p>a</p><p>b</p>"));
        assert!(!single_plain_block("<h1 data-style=\"Title\">題</h1>"));
        assert!(!single_plain_block("<table><tr><td>x</td></tr></table>"));
        assert!(!single_plain_block("<ul><li>x</li></ul>"));
    }

    #[test]
    fn the_align_attribute_becomes_text_align() {
        let out = body("<p align=\"right\">2026年11月5日</p><td align=\"center\">x</td>");
        assert!(out.contains("<p style=\"text-align: right\">"), "{out}");
        let table = body("<table align=\"center\"><tr><td>品目</td><td>1,200</td></tr></table>");
        assert!(!table.contains("<td align=\"center\""), "{table}");
    }

    #[test]
    fn column_percentages_become_proportional_pixels() {
        let out = body("<table><col width=\"25%\"><col width=\"75%\"><tr><td style=\"width: 50%\">a</td></tr></table>");
        assert!(
            out.contains("<col width=\"151\">") && out.contains("<col width=\"454\">"),
            "{out}"
        );
        assert!(out.contains("<td width=\"303\""), "{out}");
    }

    #[test]
    fn converts_width_units_to_pixels() {
        assert_eq!(html_width("40%"), "40%");
        assert_eq!(html_width("30mm"), "113");
        assert_eq!(html_width("72pt"), "96");
        assert_eq!(html_width("120px"), "120");
        assert_eq!(html_width("120"), "120");
    }

    #[test]
    fn rejects_empty_and_huge_input() {
        assert_eq!(normalize_html("<script>x</script>"), Err(HtmlError::Empty));
        assert_eq!(
            normalize_html(&"a".repeat(MAX_HTML_BYTES + 1)),
            Err(HtmlError::TooLarge)
        );
    }
}
