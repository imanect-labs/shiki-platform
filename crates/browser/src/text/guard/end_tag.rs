//! 終了タグで開いている要素を閉じる（HTML 標準の「in body」の終了タグの規則を縮めたもの・guard.rs から分割）。
//!
//! 解析器が**確実に閉じる**場面でだけ閉じる。閉じすぎると深さを少なく数え、防壁を抜けられる。
//! 規則が複雑な場面（formatting 要素の adoption agency で furthest block がある等）では閉じず、
//! 多く数える側に倒す。

use super::{Ns, Open};

/// HTML 標準の special カテゴリ（HTML の要素）。
const SPECIAL: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "keygen",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "search",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

/// 既定のスコープの境界（HTML の要素）。外部コンテンツの統合点も境界になる（[`is_special`]）。
const DEFAULT_SCOPE: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template",
];

const TABLE_PARTS: &[&str] = &[
    "table", "tbody", "thead", "tfoot", "tr", "td", "th", "caption",
];

fn is_special(o: &Open) -> bool {
    let n = o.name.as_str();
    match o.ns {
        Ns::Html => SPECIAL.contains(&n),
        Ns::Svg => matches!(n, "foreignobject" | "desc" | "title"),
        Ns::Math => matches!(n, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"),
    }
}

fn is_heading(n: &str) -> bool {
    matches!(n, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// 既定のスコープ（`extra` は list item / button スコープで足す境界）の中で `hit` に当たる HTML 要素の位置。
fn in_scope(open: &[Open], hit: impl Fn(&str) -> bool, extra: &[&str]) -> Option<usize> {
    for p in (0..open.len()).rev() {
        let o = &open[p];
        if o.ns == Ns::Html && hit(&o.name) {
            return Some(p);
        }
        let boundary = match o.ns {
            Ns::Html => {
                DEFAULT_SCOPE.contains(&o.name.as_str()) || extra.contains(&o.name.as_str())
            }
            Ns::Svg | Ns::Math => is_special(o),
        };
        if boundary {
            return None;
        }
    }
    None
}

/// table スコープ（境界は HTML の `html` / `table` / `template` だけ）の中の同名の要素の位置。
fn in_table_scope(open: &[Open], name: &str) -> Option<usize> {
    for p in (0..open.len()).rev() {
        let o = &open[p];
        if o.ns != Ns::Html {
            continue;
        }
        if o.name == name {
            return Some(p);
        }
        if matches!(o.name.as_str(), "html" | "table" | "template") {
            return None;
        }
    }
    None
}

/// 終了タグ `</name>` で閉じる。
pub(super) fn close(open: &mut Vec<Open>, name: &str) {
    // `</p>` と `</br>` は外部コンテンツを抜けてから HTML の規則で処理する（HTML の要素か統合点まで閉じる）。
    if matches!(name, "p" | "br") {
        while open.last().is_some_and(|o| o.content_foreign) {
            open.pop();
        }
    } else if open.last().is_some_and(|o| o.ns != Ns::Html) {
        // 外部コンテンツの中: HTML の要素に着くまでに同名の外部の要素があればそこまで閉じる。
        for p in (0..open.len()).rev() {
            if open[p].ns == Ns::Html {
                break;
            }
            if open[p].name == name {
                open.truncate(p);
                return;
            }
        }
    }
    let pos = match name {
        // 閉じない（body・html は閉じても以降の内容が body に入る。`</br>` は `<br>` になる）。
        "body" | "html" | "br" => None,
        // form は自分だけを取り除く（中の要素は開いたまま残る）。
        "form" => {
            if let Some(p) = in_scope(open, |n| n == "form", &[]) {
                open.remove(p);
            }
            return;
        }
        "p" => in_scope(open, |n| n == "p", &["button"]),
        "li" => in_scope(open, |n| n == "li", &["ol", "ul"]),
        _ if is_heading(name) => in_scope(open, is_heading, &[]),
        _ if TABLE_PARTS.contains(&name) => in_table_scope(open, name),
        // 特別でない要素（formatting 要素を含む）: 特別な要素に当たる前に同名の要素があればそこまで閉じる
        // （「any other end tag」と、furthest block の無い adoption agency）。
        _ if !SPECIAL.contains(&name) => {
            let mut found = None;
            for p in (0..open.len()).rev() {
                let o = &open[p];
                if o.ns == Ns::Html && o.name == name {
                    found = Some(p);
                    break;
                }
                if is_special(o) {
                    break;
                }
            }
            found
        }
        // その他の特別な要素: 既定のスコープにあればそこまで閉じる。
        _ => in_scope(open, |n| n == name, &[]),
    };
    if let Some(p) = pos {
        open.truncate(p);
    }
}
