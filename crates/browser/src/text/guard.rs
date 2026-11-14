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

/// 入れ子の深さが `max_depth` を超える手前までを返す（超えなければ全体）。切ったかどうかも返す。
pub(crate) fn clip_depth(html: &str, max_depth: usize) -> (&str, bool) {
    let b = html.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
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
            depth = depth.saturating_sub(usize::from(!is_optional_end(name_at(b, i + 2))));
            i = tag_end(b, i + 2).unwrap_or(b.len());
            continue;
        }
        if rest.len() < 2 || !rest[1].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let name = name_at(b, i + 1);
        let Some(end) = tag_end(b, i + 1) else {
            return (html, false);
        };
        let self_closing = end >= 2 && b[end - 2] == b'/';
        let lower = name.to_ascii_lowercase();
        if !(self_closing || VOID.contains(&lower.as_str()) || is_optional_end(name)) {
            depth += 1;
            if depth > max_depth {
                return (&html[..i], true);
            }
        }
        i = end;
        if RAW_TEXT.contains(&lower.as_str()) {
            // 生テキストの中の `<` はタグではない。対応する終了タグまで飛ばす。
            let close = format!("</{lower}");
            i = find_ci(b, i, close.as_bytes()).unwrap_or(b.len());
        }
    }
    (html, false)
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
