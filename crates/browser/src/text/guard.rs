//! HTML を解析器へ渡す前の防壁（敵対的な入力への備え・PIT-23）。
//!
//! html5ever は入れ子の深い文書で解析時間が二乗で伸びる（実測: `<div>` の 5 万段で 11 秒、20 万段で 200 秒）。
//! 解析は `spawn_blocking` の中で走り、呼び出し側の期限では止まらないので、**解析する前に**入れ子の深さを
//! 軽く数え、上限を超えた所から先は渡さない。
//!
//! 数え方は近似でよい（実在のページは 100 段に満たず、上限は十分に大きい）:
//! - 空要素（`<br>` 等）・自己終了（`<x/>`）・終了タグを省略できる要素（`<p>` `<li>` `<td>` 等）は数えない。
//! - コメントと、中身を生テキストとして扱う要素（`<script>` `<style>` 等）の中は読み飛ばす。

/// 解析器へ渡す入れ子の深さの上限。
pub(crate) const MAX_PARSE_DEPTH: usize = 1024;

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// 終了タグを省略できる要素（開いたまま次へ進む実在のページが多いので、深さに数えない）。
const OPTIONAL_END: &[&str] = &[
    "p", "li", "dt", "dd", "tr", "td", "th", "option", "optgroup", "tbody", "thead", "tfoot",
    "colgroup", "rb", "rt", "rp",
];

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
    let mut open: Vec<String> = Vec::new();
    while let Some(off) = b[i..].iter().position(|&c| c == b'<') {
        i += off;
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            match find(b, i + 4, b"-->") {
                Some(end) => i = end + 3,
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
        let Some(end) = tag_end(b, i + 1) else {
            return (html, false);
        };
        let mut foreign = open.iter().any(|n| n == "svg" || n == "math");
        if foreign && FOREIGN_BREAKOUT.contains(&lower.as_str()) {
            // HTML の要素が来ると解析器は外部コンテンツを抜ける。
            if let Some(p) = open.iter().rposition(|n| n == "svg" || n == "math") {
                open.truncate(p);
            }
            foreign = false;
        }
        let self_closing = end >= 2 && b[end - 2] == b'/';
        let closes_itself =
            VOID.contains(&lower.as_str()) || is_optional_end(&lower) || (foreign && self_closing);
        if !closes_itself {
            open.push(lower.clone());
            if open.len() > max_depth {
                return (&html[..i], true);
            }
        }
        i = end;
        if !foreign && RAW_TEXT.contains(&lower.as_str()) {
            // 生テキストの中の `<` はタグではない。対応する終了タグまで飛ばす。
            let close_tag = format!("</{lower}");
            i = find_ci(b, i, close_tag.as_bytes()).unwrap_or(b.len());
        }
    }
    (html, false)
}

/// 終了タグで閉じる（同じ名前が開いていて、間に境界が無いときだけ）。
fn close(open: &mut Vec<String>, name: &str) {
    for p in (0..open.len()).rev() {
        if open[p] == name {
            open.truncate(p);
            return;
        }
        if SCOPE_BOUNDARY.contains(&open[p].as_str()) {
            return;
        }
    }
}

fn is_optional_end(name: &str) -> bool {
    OPTIONAL_END.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// `i` から始まるタグ名（英数字と `-`）。
fn name_at(b: &[u8], i: usize) -> &str {
    let end = b[i.min(b.len())..]
        .iter()
        .position(|c| !(c.is_ascii_alphanumeric() || *c == b'-'))
        .map_or(b.len(), |p| i + p);
    std::str::from_utf8(&b[i.min(end)..end]).unwrap_or("")
}

/// タグの `>` の直後の位置（引用符の中の `>` は飛ばす）。
fn tag_end(b: &[u8], mut i: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) if c == q => quote = None,
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'>' => return Some(i + 1),
            Some(_) | None => {}
        }
        i += 1;
    }
    None
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
        // 開いたままの <p> / <li> は深さに数えない。
        let flat = "<p>a".repeat(10_000);
        assert!(!clip_depth(&flat, 16).1);
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
