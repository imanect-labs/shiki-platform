//! HTML を解析器へ渡す前の防壁（敵対的な入力への備え・PIT-23）。
//!
//! html5ever は入れ子の深い文書で解析時間が二乗で伸びる（実測: `<div>` の 5 万段で 11 秒、20 万段で 200 秒）。
//! 解析は `spawn_blocking` の中で走り、呼び出し側の期限では止まらないので、**解析する前に**入れ子の深さを
//! 軽く数え、上限を超えた所から先は渡さない。
//!
//! 数え方は近似でよい（実在のページは 100 段に満たず、上限は十分に大きい）:
//! - 空要素（`<br>` 等）と、外部コンテンツ（SVG・MathML）の自己終了（`<x/>`）は数えない。
//! - 終了タグを省略できる要素（`<p>` `<li>` `<td>` 等）は、解析器と同じく同種の要素を開いたときに閉じる。
//! - コメントと、中身を生テキストとして扱う要素（`<script>` `<style>` 等）の中は読み飛ばす。

mod end_tag;

use end_tag::close;

/// 解析器へ渡す入れ子の深さの上限。
pub(crate) const MAX_PARSE_DEPTH: usize = 1024;

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// 開くと、開いている要素の一番内側から暗黙に閉じる要素（HTML 標準の暗黙の終了タグ。その部分集合）。
///
/// 終了タグを省略できる要素（`<p>` `<li>` `<td>` 等）を開いたままにする実在のページは多いので、
/// 解析器と同じく同種の要素で閉じる。ただし**一番内側にあるときだけ**閉じる（解析器が閉じない場面で
/// 閉じて数を減らさないよう、迷ったら閉じない＝多く数える側に倒す）。
fn implicitly_closes(name: &str, ruby_open: bool) -> &'static [&'static str] {
    match name {
        "p" => &["p"],
        "li" => &["li", "p"],
        "dt" | "dd" => &["dt", "dd", "p"],
        "tr" => &["tr", "td", "th", "p"],
        "td" | "th" => &["td", "th", "p"],
        "tbody" | "thead" | "tfoot" => &["tbody", "thead", "tfoot", "tr", "td", "th", "p"],
        "option" => &["option"],
        "optgroup" => &["option", "optgroup"],
        "colgroup" => &["colgroup"],
        // rb・rt・rp・rtc が閉じるのは ruby の中だけ（外では入れ子になる）。
        "rb" | "rtc" if ruby_open => &["rb", "rt", "rp", "rtc"],
        "rt" | "rp" if ruby_open => &["rb", "rt", "rp"],
        _ => &[],
    }
}

const RAW_TEXT: &[&str] = &[
    "script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes",
];

/// 終了タグの対応付けで越えない境界（HTML の解析器もこの外の要素を閉じない）。
const SCOPE_BOUNDARY: &[&str] = &[
    "table", "template", "caption", "marquee", "object", "applet", "html", "svg", "math",
];

/// 外部コンテンツ（SVG・MathML）から HTML へ抜ける要素（HTML 標準の「breakout」）。
const FOREIGN_BREAKOUT: &[&str] = &[
    "b",
    "big",
    "blockquote",
    "body",
    "br",
    "center",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "i",
    "img",
    "li",
    "listing",
    "menu",
    "meta",
    "nobr",
    "ol",
    "p",
    "pre",
    "ruby",
    "s",
    "small",
    "span",
    "strong",
    "strike",
    "sub",
    "sup",
    "table",
    "tt",
    "u",
    "ul",
    "var",
    "font",
];

/// 入れ子の深さが `max_depth` を超える手前までを返す（超えなければ全体）。切ったかどうかも返す。
///
/// 少なく数えると防壁が効かないので、迷ったら多く数える側に倒す:
/// - 自己終了（`<x/>`）は、解析器が従う SVG・MathML の中でだけ閉じたとみなす（HTML の要素では無視される）。
/// - 終了タグは、開いている要素に同じ名前があり、間に境界（`table` 等）が無いときだけ閉じる
///   （対応しない終了タグを並べて数を減らさせない）。
pub(crate) fn clip_depth(html: &str, max_depth: usize) -> (&str, bool) {
    let b = html.as_bytes();
    let mut i = 0;
    let mut open: Vec<Open> = Vec::new();
    while let Some(off) = b[i..].iter().position(|&c| c == b'<') {
        i += off;
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            match comment_end(b, i + 4) {
                Some(end) => i = end,
                None => return (html, false),
            }
            continue;
        }
        if rest.len() > 1 && rest[1] == b'/' {
            close(&mut open, &name_at(b, i + 2).to_ascii_lowercase());
            i = tag_end(b, i + 2).unwrap_or(b.len());
            continue;
        }
        if rest.len() < 2 || !rest[1].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let lower = name_at(b, i + 1).to_ascii_lowercase();
        let Some(tag) = scan_tag(b, i + 1) else {
            return (html, false);
        };
        let end = tag.end;
        // `font` が抜けるのは color / face / size の属性があるときだけ。
        let breakout = FOREIGN_BREAKOUT.contains(&lower.as_str())
            && (lower != "font"
                || ["color", "face", "size"]
                    .iter()
                    .any(|a| tag.attr(b, a).is_some()));
        if in_foreign(&open) && breakout {
            // HTML の要素が来ると解析器は外部コンテンツを抜ける（HTML の要素か統合点まで閉じる）。
            while in_foreign(&open) {
                open.pop();
            }
        }
        let ns = namespace(&open, &lower);
        if ns == Ns::Html {
            let ruby_open =
                matches!(lower.as_str(), "rb" | "rt" | "rp" | "rtc") && ruby_in_scope(&open);
            let closes = implicitly_closes(&lower, ruby_open);
            while open
                .last()
                .is_some_and(|o| o.ns == Ns::Html && closes.contains(&o.name.as_str()))
            {
                open.pop();
            }
        }
        let self_closing = end >= 2 && b[end - 2] == b'/';
        let closes_itself = match ns {
            Ns::Html => VOID.contains(&lower.as_str()),
            // 自己終了が効くのは外部コンテンツの要素だけ。
            Ns::Svg | Ns::Math => self_closing,
        };
        if !closes_itself {
            let content_foreign = match ns {
                Ns::Html => false,
                Ns::Svg => !SVG_INTEGRATION.contains(&lower.as_str()),
                // annotation-xml が統合点になるのは encoding が HTML のときだけ。
                Ns::Math if lower == "annotation-xml" => {
                    let name_end = i + 1 + lower.len();
                    !annotation_is_html(html, name_end, &tag)
                }
                Ns::Math => !MATH_INTEGRATION.contains(&lower.as_str()),
            };
            open.push(Open {
                name: lower.clone(),
                ns,
                content_foreign,
            });
            if open.len() > max_depth {
                return (&html[..i], true);
            }
        }
        i = end;
        if ns == Ns::Html && RAW_TEXT.contains(&lower.as_str()) {
            // 生テキストの中の `<` はタグではない。対応する終了タグまで飛ばす。
            // 終了タグ名の直後が区切り（空白・`/`・`>`）のときだけ（`</stylex>` は style を閉じない）。
            let close_tag = format!("</{lower}");
            let mut from = i;
            i = loop {
                match find_ci(b, from, close_tag.as_bytes()) {
                    Some(p) => {
                        let after = b.get(p + close_tag.len()).copied();
                        if after.is_none_or(|c| c.is_ascii_whitespace() || c == b'/' || c == b'>') {
                            break p;
                        }
                        from = p + 1;
                    }
                    None => break b.len(),
                }
            };
        }
    }
    (html, false)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    Html,
    Svg,
    Math,
}

/// 開いている要素。`content_foreign` は中身が外部コンテンツとして解析されるか
/// （SVG・MathML の要素で、HTML の統合点＝`foreignObject` 等でないもの）。
struct Open {
    name: String,
    ns: Ns,
    content_foreign: bool,
}

/// SVG の HTML 統合点（中の開始タグは HTML として解析される）。
const SVG_INTEGRATION: &[&str] = &["foreignobject", "desc", "title"];
/// MathML の統合点（`annotation-xml` は encoding によるが、多く数える側＝HTML とみなす）。
const MATH_INTEGRATION: &[&str] = &["mi", "mo", "mn", "ms", "mtext"];

/// `ruby` がスコープの中にあるか（間に `table`・`td` 等の境界があれば無い・HTML 標準の「has an element in scope」）。
fn ruby_in_scope(open: &[Open]) -> bool {
    for o in open.iter().rev() {
        if o.ns == Ns::Html && o.name == "ruby" {
            return true;
        }
        let html_boundary = o.ns == Ns::Html
            && (SCOPE_BOUNDARY.contains(&o.name.as_str())
                || matches!(o.name.as_str(), "td" | "th"));
        // 外部コンテンツの要素（svg・math・統合点）も境界になる。
        if html_boundary || o.ns != Ns::Html {
            return false;
        }
    }
    false
}

fn in_foreign(open: &[Open]) -> bool {
    open.last().is_some_and(|o| o.content_foreign)
}

/// 次に開く要素の名前空間。外部コンテンツの中では名前によらず親の名前空間を継ぐ
/// （解析器と同じ。`<svg><math>` の `math` は SVG の要素で、その中の `foreignObject` は SVG の統合点）。
fn namespace(open: &[Open], name: &str) -> Ns {
    match open.last() {
        // annotation-xml の直下の svg は SVG。
        Some(parent)
            if parent.content_foreign
                && parent.ns == Ns::Math
                && parent.name == "annotation-xml"
                && name == "svg" =>
        {
            Ns::Svg
        }
        Some(parent) if parent.content_foreign => parent.ns,
        // MathML のテキスト統合点の直下の mglyph / malignmark は MathML のまま。
        Some(parent)
            if parent.ns == Ns::Math
                && MATH_INTEGRATION.contains(&parent.name.as_str())
                && matches!(name, "mglyph" | "malignmark") =>
        {
            Ns::Math
        }
        _ => match name {
            "svg" => Ns::Svg,
            "math" => Ns::Math,
            _ => Ns::Html,
        },
    }
}

/// `i` から始まるタグ名。HTML の tokenizer と同じく、空白・`/`・`>` までをすべて名前とする
/// （`<div:x>` は `div` ではなく `div:x`。途中で切ると `</div>` で閉じたと誤認する）。
fn name_at(b: &[u8], i: usize) -> &str {
    let end = b[i.min(b.len())..]
        .iter()
        .position(|c| c.is_ascii_whitespace() || *c == b'/' || *c == b'>')
        .map_or(b.len(), |p| i + p);
    std::str::from_utf8(&b[i.min(end)..end]).unwrap_or("")
}

/// タグの中の tokenizer の状態（HTML 標準の tag / attribute 系の状態を縮めたもの）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum TagState {
    TagName,
    BeforeAttrName,
    AttrName,
    AfterAttrName,
    BeforeValue,
    Quoted(u8),
    Unquoted,
}

/// タグの `>` の直後の位置。`i` はタグ名の先頭。
fn tag_end(b: &[u8], i: usize) -> Option<usize> {
    scan_tag(b, i).map(|t| t.end)
}

/// `annotation-xml` が HTML 統合点か（`encoding` が `text/html` / `application/xhtml+xml`）。
///
/// 値は文字参照（`text&#47;html` 等）をデコードしてから比べる。文字参照を含むときだけ、
/// 解析器と同じ規則でデコードするため、そのタグの属性部分だけを解析器に読ませる（タグの長さに比例する）。
fn annotation_is_html(html: &str, name_end: usize, tag: &Tag) -> bool {
    let b = html.as_bytes();
    let Some(raw) = tag.attr(b, "encoding") else {
        return false;
    };
    let is_html = |e: &[u8]| {
        e.eq_ignore_ascii_case(b"text/html") || e.eq_ignore_ascii_case(b"application/xhtml+xml")
    };
    if !raw.contains(&b'&') {
        return is_html(raw);
    }
    let attrs = html.get(name_end..tag.end).unwrap_or(">");
    let doc = dom_query::Document::from(format!("<div{attrs}"));
    doc.select("div")
        .attr("encoding")
        .is_some_and(|e| is_html(e.as_bytes()))
}

/// 属性の名前と値のバイト範囲。
type AttrSpan = (std::ops::Range<usize>, Option<std::ops::Range<usize>>);

/// 読み取ったタグ（`end` は `>` の直後）。
struct Tag {
    end: usize,
    attrs: Vec<AttrSpan>,
}

impl Tag {
    /// 属性の値（名前は ASCII で大小を区別しない。値の無い属性は空）。同名は最初のものが有効。
    fn attr<'a>(&self, b: &'a [u8], name: &str) -> Option<&'a [u8]> {
        self.attrs
            .iter()
            .find(|(n, _)| b[n.clone()].eq_ignore_ascii_case(name.as_bytes()))
            .map(|(_, v)| v.clone().map_or(&b[0..0], |v| &b[v]))
    }
}

/// タグを読む。`i` はタグ名の先頭。
///
/// 引用符が値を囲むのは `=` の直後だけ（`<div x=a'>` の `'` は値の一部、`<div a"b>` の `"` は名前の一部）。
/// 引用符を一律に数えると、閉じない引用符で後続のタグをまとめて読み飛ばし、深さを少なく数える。
fn scan_tag(b: &[u8], mut i: usize) -> Option<Tag> {
    use TagState::{
        AfterAttrName, AttrName, BeforeAttrName, BeforeValue, Quoted, TagName, Unquoted,
    };
    let mut state = TagName;
    let mut attrs: Vec<AttrSpan> = Vec::new();
    while i < b.len() {
        let c = b[i];
        let ws = c.is_ascii_whitespace();
        let next = match state {
            Quoted(q) if c == q => BeforeAttrName,
            Quoted(q) => Quoted(q),
            _ if c == b'>' => return Some(Tag { end: i + 1, attrs }),
            BeforeValue if c == b'"' || c == b'\'' => Quoted(c),
            BeforeValue if ws => BeforeValue,
            BeforeValue | Unquoted if !ws => Unquoted,
            AttrName | AfterAttrName if c == b'=' => BeforeValue,
            AttrName if !ws && c != b'/' => AttrName,
            TagName if !ws && c != b'/' => TagName,
            AttrName | AfterAttrName if ws => AfterAttrName,
            _ if c == b'/' || ws => BeforeAttrName,
            // 属性名の先頭（`=` で始まる名前も名前の一部）。
            _ => AttrName,
        };
        // 名前と値の範囲を記録する。
        match (state, next) {
            (AttrName, AttrName) => {
                if let Some((n, _)) = attrs.last_mut() {
                    n.end = i + 1;
                }
            }
            (_, AttrName) => attrs.push((i..i + 1, None)),
            (BeforeValue, Quoted(_)) => {
                if let Some((_, v)) = attrs.last_mut() {
                    *v = Some(i + 1..i + 1);
                }
            }
            (BeforeValue, Unquoted) => {
                if let Some((_, v)) = attrs.last_mut() {
                    *v = Some(i..i + 1);
                }
            }
            (Quoted(_), Quoted(_)) | (Unquoted, Unquoted) => {
                if let Some((_, Some(v))) = attrs.last_mut() {
                    v.end = i + 1;
                }
            }
            _ => {}
        }
        state = next;
        i += 1;
    }
    None
}

/// コメント（`<!--` の直後の `from` から）の終わりの直後の位置。HTML tokenizer と同じく、
/// `<!-->` / `<!--->` は空のコメントとしてすぐ閉じ、`-->` のほか `--!>` でも閉じる。
fn comment_end(b: &[u8], from: usize) -> Option<usize> {
    let rest = b.get(from..)?;
    if rest.starts_with(b">") {
        return Some(from + 1);
    }
    if rest.starts_with(b"->") {
        return Some(from + 2);
    }
    let close = find(b, from, b"-->").map(|p| p + 3);
    let bang = find(b, from, b"--!>").map(|p| p + 4);
    match (close, bang) {
        (Some(a), Some(c)) => Some(a.min(c)),
        (a, c) => a.or(c),
    }
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    b.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

fn find_ci(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    b.get(from..)?
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|p| from + p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_pages_are_not_clipped() {
        let html = r#"<!doctype html><html><head><title>a<b</title><script>if (a < b && c > d) { x("<div>") }</script>
            <style>p > a {}</style></head><body><!-- <div><div> --><div class="x>y"><p>一<p>二<ul><li>a<li>b</ul>
            <br><img src=a><input/><table><tr><td>1<td>2</table></div></body></html>"#;
        assert_eq!(clip_depth(html, 8), (html, false));
    }

    #[test]
    fn deep_nesting_is_clipped_before_parsing() {
        let html = format!(
            "<body>{}x{}</body>",
            "<div>".repeat(5_000),
            "</div>".repeat(5_000)
        );
        let (clipped, cut) = clip_depth(&html, MAX_PARSE_DEPTH);
        assert!(cut);
        assert!(
            clipped.len() < 6 * (MAX_PARSE_DEPTH + 2),
            "{}",
            clipped.len()
        );
        // 開いたままの <p> / <li> / 表 / 選択肢 / ruby は同種で閉じ、深さに数えない。
        for flat in [
            "<p>a<b>太字</b>".repeat(10_000),
            format!("<ul>{}</ul>", "<li><a href=x>a</a>".repeat(10_000)),
            format!("<table>{}</table>", "<tr><td>1<td><p>2".repeat(10_000)),
            format!("<select>{}</select>", "<option>a".repeat(10_000)),
            format!("<dl>{}</dl>", "<dt>a<dd>b".repeat(10_000)),
            format!("<ruby>{}</ruby>", "漢<rp>(<rt>かん<rp>)".repeat(10_000)),
        ] {
            assert!(!clip_depth(&flat, 16).1, "{}", &flat[..40]);
        }
    }

    #[test]
    fn tricks_that_undercount_are_still_clipped() {
        // HTML の要素の自己終了は無視される（解析器は入れ子にする）。
        let selfclose = "<div/>".repeat(100_000);
        assert!(clip_depth(&selfclose, MAX_PARSE_DEPTH).1);
        // 対応しない終了タグでは減らない。
        let bogus = "<div></bogus>".repeat(100_000);
        assert!(clip_depth(&bogus, MAX_PARSE_DEPTH).1);
        // 境界の外の要素は終了タグで閉じない。
        let scoped = "<div><table></div>".repeat(100_000);
        assert!(clip_depth(&scoped, MAX_PARSE_DEPTH).1);
        // SVG の中の自己終了は閉じる（アイコンの多いページを切らない）。
        let icons = format!("<svg>{}</svg>", "<path d=\"M0\"/>".repeat(10_000)).repeat(20);
        assert!(!clip_depth(&icons, MAX_PARSE_DEPTH).1);
        // タグ名は空白・`/`・`>` まで（`<div:x>` は `</div>` では閉じない）。
        let colon = "<div:x></div>".repeat(100_000);
        assert!(clip_depth(&colon, MAX_PARSE_DEPTH).1);
        // 空のコメント（`<!-->` / `<!--->`）と `--!>` はその場で閉じる（後続を読み飛ばさない）。
        for opener in ["<!-->", "<!--->", "<!-- x --!>"] {
            let s = format!("{opener}{}", "<div>".repeat(100_000));
            assert!(clip_depth(&s, MAX_PARSE_DEPTH).1, "{opener}");
        }
        // 生テキストの終了タグは名前全体で照合する（`</stylex>` で style を抜けない）。
        let rawtext = format!(
            "<style></stylex><script></style>{}",
            "<div>".repeat(100_000)
        );
        assert!(clip_depth(&rawtext, MAX_PARSE_DEPTH).1);
        // 引用符の無い値の中の引用符は値の一部（後続のタグを引用として飛ばさない）。
        let quote_in_value = format!("<div x=a'>{}", "<div>".repeat(100_000));
        assert!(clip_depth(&quote_in_value, MAX_PARSE_DEPTH).1);
        let quote_in_name = format!("<div a\"b>{}", "<div>".repeat(100_000));
        assert!(clip_depth(&quote_in_name, MAX_PARSE_DEPTH).1);
        // 引用符で囲んだ値の中の `>` はタグの終わりではない。
        let quoted = format!("<div x = \"a>b\" y='c>d'>{}", "<div>".repeat(100_000));
        assert!(clip_depth(&quoted, MAX_PARSE_DEPTH).1);
        assert_eq!(tag_end(b"div x = \"a>b\">rest", 0), Some(14));
        let t = scan_tag(b"font COLOR = 'red' face=x size>", 0).unwrap();
        let b = b"font COLOR = 'red' face=x size>";
        assert_eq!(t.attr(b, "color"), Some(&b"red"[..]));
        assert_eq!(t.attr(b, "face"), Some(&b"x"[..]));
        assert_eq!(t.attr(b, "size"), Some(&b""[..]));
        assert_eq!(t.attr(b, "class"), None);
        // 属性の無い font は外部コンテンツを抜けない（中の input は SVG の要素として入れ子になる）。
        let font = format!("<svg><font>{}", "<input>".repeat(100_000));
        assert!(clip_depth(&font, MAX_PARSE_DEPTH).1);
        // 属性のある font は抜ける（以降の input は HTML の空要素）。
        let font_attr = format!("<svg><font color=red>{}", "<input>".repeat(100_000));
        assert!(!clip_depth(&font_attr, MAX_PARSE_DEPTH).1);
        // encoding の無い・HTML でない annotation-xml は統合点ではない。
        for open in [
            "<math><annotation-xml>",
            "<math><annotation-xml encoding=\"image/svg+xml\">",
        ] {
            let s = format!("{open}{}", "<input>".repeat(100_000));
            assert!(clip_depth(&s, MAX_PARSE_DEPTH).1, "{open}");
        }
        // 文字参照で書いた encoding もデコードしてから比べる（統合点になり、中の自己終了は効かない）。
        let entity = format!(
            "<math><annotation-xml encoding=\"text&#47;html\">{}",
            "<x-node/>".repeat(100_000)
        );
        assert!(clip_depth(&entity, MAX_PARSE_DEPTH).1);
        let named = format!(
            "<math><annotation-xml encoding=\"text&sol;html\">{}",
            "<x-node/>".repeat(100_000)
        );
        assert!(clip_depth(&named, MAX_PARSE_DEPTH).1);
        let html_xml = format!(
            "<math><annotation-xml encoding=\"Text/HTML\">{}",
            "<input>".repeat(100_000)
        );
        assert!(!clip_depth(&html_xml, MAX_PARSE_DEPTH).1);
        // MathML のテキスト統合点の直下の mglyph / malignmark は MathML のまま。
        for tag in ["mglyph", "malignmark"] {
            let s = format!("<math><mi><{tag}>{}", "<input>".repeat(100_000));
            assert!(clip_depth(&s, MAX_PARSE_DEPTH).1, "{tag}");
        }
        // annotation-xml の直下の svg は SVG（その foreignObject は統合点）。
        let ann_svg = format!(
            "<math><annotation-xml><svg><foreignObject>{}",
            "<x-node/>".repeat(100_000)
        );
        assert!(clip_depth(&ann_svg, MAX_PARSE_DEPTH).1);
        // SVG の HTML 統合点（foreignObject 等）の中では自己終了は効かない。
        let integration = format!("<svg><foreignObject>{}", "<x-node/>".repeat(100_000));
        assert!(clip_depth(&integration, MAX_PARSE_DEPTH).1);
        // 外部コンテンツの中の `math` は親（SVG）の名前空間を継ぐので、その中の foreignObject も統合点。
        let nested = format!("<svg><math><foreignObject>{}", "<x-node/>".repeat(100_000));
        assert!(clip_depth(&nested, MAX_PARSE_DEPTH).1);
        // 逆に MathML の統合点（mi）の中の svg は SVG になる。
        let mi = format!("<math><mi><svg>{}</svg>", "<path/>".repeat(100_000));
        assert!(!clip_depth(&mi, MAX_PARSE_DEPTH).1);
        // ruby の外の rb・rt・rp は暗黙に閉じず入れ子になる。
        for tag in ["rb", "rt", "rp"] {
            let s = format!("<body>{}", format!("<{tag}>").repeat(100_000));
            assert!(clip_depth(&s, MAX_PARSE_DEPTH).1, "{tag}");
        }
        // form の終了タグは form だけを取り除き、中の要素は開いたまま。
        let form = "<form><div></form>".repeat(100_000);
        assert!(clip_depth(&form, MAX_PARSE_DEPTH).1);
        // li の終了タグは ul・ol を越えない（list item スコープ）。
        let list = "<li><ul></li>".repeat(100_000);
        assert!(clip_depth(&list, MAX_PARSE_DEPTH).1);
        // formatting 要素の終了タグは、間に特別な要素があると閉じない（adoption agency）。
        let formatting = "<b><div></b>".repeat(100_000);
        assert!(clip_depth(&formatting, MAX_PARSE_DEPTH).1);
        // p の終了タグは button を越えない（button スコープ）。
        let button = "<p><button></p>".repeat(100_000);
        assert!(clip_depth(&button, MAX_PARSE_DEPTH).1);
        // ruby が開いていても、間に table・td があるとスコープの外（閉じない）。
        let cell = format!("<ruby><table><tbody><tr><td>{}", "<rb>".repeat(100_000));
        assert!(clip_depth(&cell, MAX_PARSE_DEPTH).1);
        // 一番内側でない（間に閉じない要素がある）ときは閉じない。
        let section = "<li><section>".repeat(100_000);
        assert!(clip_depth(&section, MAX_PARSE_DEPTH).1);
        // 外部コンテンツの中では、終了タグを省略できる HTML の要素名でも入れ子になる。
        let foreign_option = format!("<svg>{}", "<option>".repeat(100_000));
        assert!(clip_depth(&foreign_option, MAX_PARSE_DEPTH).1);
        // SVG の中でも HTML の要素は抜け出して入れ子になる。
        let breakout = "<svg><div/>".repeat(100_000);
        assert!(clip_depth(&breakout, MAX_PARSE_DEPTH).1);
    }

    #[test]
    fn unterminated_constructs_do_not_panic() {
        for s in [
            "<",
            "<!--",
            "<div",
            "<div class=\"",
            "</",
            "<script>x",
            "<a\u{3042}>",
            "\u{3042}<",
        ] {
            let _ = clip_depth(s, 2);
        }
    }
}
