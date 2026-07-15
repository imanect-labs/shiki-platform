//! スライド HTML の書込時サニタイズ（Task 11.1・design §4.8.3・**PIT-40 の第1層**）。
//!
//! スライドは自由 HTML を許可するため、サーバがすべての書込経路
//! （保存シリアライズ・外部書込インポート・AI 編集・下書き確定）で本モジュールを通し、
//! **「サニタイズ済みが正規形」**を保証する。描画側の DOMPurify（第2層）・
//! sandbox iframe（第3/4層）はこの上に重ねる多層防御であり、どれか1層に依存しない。
//!
//! 方針:
//! - script / iframe / object / embed / form 系・イベントハンドラ（on*）・`javascript:` は
//!   ammonia の許可リスト方式で構造的に落とす（拒否リストではない）。
//! - `style` 属性は許可するが、外部到達ベクタ（`url()` の data: 以外・`expression()`・
//!   `-moz-binding`）を含む値は**属性ごと**落とす（fail-closed）。
//! - 画像 `src` は `data:` とドライブ参照属性 `data-shiki-node`（描画側が閲覧者本人の
//!   ReBAC で解決）のみ。外部 URL の画像は exfil ベクタになるため許可しない。

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::OnceLock;

use ammonia::Builder;

/// スライド 1 枚の HTML をサニタイズする（冪等・正規形を返す）。
pub fn sanitize_html(html: &str) -> String {
    builder().clean(html).to_string()
}

fn builder() -> &'static Builder<'static> {
    static BUILDER: OnceLock<Builder<'static>> = OnceLock::new();
    BUILDER.get_or_init(|| {
        let mut b = Builder::default();
        b.tags(HashSet::from([
            // 構造
            "div",
            "section",
            "header",
            "footer",
            "main",
            "article",
            "span",
            "p",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "blockquote",
            "pre",
            "br",
            "hr",
            // リスト・表
            "ul",
            "ol",
            "li",
            "table",
            "thead",
            "tbody",
            "tfoot",
            "tr",
            "td",
            "th",
            "colgroup",
            "col",
            "caption",
            // インライン装飾
            "strong",
            "em",
            "b",
            "i",
            "u",
            "s",
            "small",
            "sup",
            "sub",
            "code",
            "mark",
            "a",
            // メディア・図
            "img",
            "figure",
            "figcaption",
            "svg",
            "path",
            "circle",
            "rect",
            "line",
            "polyline",
            "polygon",
            "g",
            "text",
            "defs",
            "linearGradient",
            "stop",
        ]));
        b.generic_attributes(HashSet::from(["class", "id", "style", "data-sid"]));
        b.tag_attributes(std::collections::HashMap::from([
            (
                "img",
                HashSet::from(["src", "alt", "width", "height", "data-shiki-node"]),
            ),
            ("a", HashSet::from(["href", "title"])),
            ("td", HashSet::from(["colspan", "rowspan"])),
            ("th", HashSet::from(["colspan", "rowspan"])),
            ("col", HashSet::from(["span"])),
            // SVG 基本属性（描画のみ・スクリプト系属性は許可リスト外で落ちる）
            (
                "svg",
                HashSet::from(["viewBox", "width", "height", "fill", "xmlns"]),
            ),
            (
                "path",
                HashSet::from(["d", "fill", "stroke", "stroke-width", "stroke-linecap"]),
            ),
            (
                "circle",
                HashSet::from(["cx", "cy", "r", "fill", "stroke", "stroke-width"]),
            ),
            (
                "rect",
                HashSet::from(["x", "y", "width", "height", "rx", "fill", "stroke"]),
            ),
            (
                "line",
                HashSet::from(["x1", "y1", "x2", "y2", "stroke", "stroke-width"]),
            ),
            (
                "polyline",
                HashSet::from(["points", "fill", "stroke", "stroke-width"]),
            ),
            ("polygon", HashSet::from(["points", "fill", "stroke"])),
            ("g", HashSet::from(["fill", "stroke", "transform"])),
            (
                "text",
                HashSet::from(["x", "y", "fill", "font-size", "text-anchor"]),
            ),
            ("linearGradient", HashSet::from(["x1", "y1", "x2", "y2"])),
            (
                "stop",
                HashSet::from(["offset", "stop-color", "stop-opacity"]),
            ),
        ]));
        // href は https/mailto、img src は data: のみ（下の attribute_filter で分別）。
        b.url_schemes(HashSet::from(["https", "http", "mailto", "data"]));
        b.attribute_filter(filter_attribute);
        b
    })
}

/// 属性値レベルの追加フィルタ（ammonia の許可リストを通過した後に適用）。
fn filter_attribute<'v>(element: &str, attribute: &str, value: &'v str) -> Option<Cow<'v, str>> {
    match attribute {
        // style は外部到達・スクリプト実行ベクタを含む値を属性ごと落とす。
        "style" => {
            if style_is_safe(value) {
                Some(Cow::Borrowed(value))
            } else {
                None
            }
        }
        // 画像 src は data: URL のみ（外部画像は exfil ベクタ・ドライブ参照は data-shiki-node で）。
        "src" if element == "img" => {
            if value.trim_start().to_lowercase().starts_with("data:image/") {
                Some(Cow::Borrowed(value))
            } else {
                None
            }
        }
        _ => Some(Cow::Borrowed(value)),
    }
}

/// インライン style の安全判定（`url()` は data: のみ・レガシー実行ベクタ拒否）。
///
/// fail-closed の原則: 判定を騙せる余地のある構文は**値ごと**落とす。
/// - `\` を含む値は全拒否（CSS エスケープ `u\72l(...)` による関数名難読化を構造的に遮断）
/// - `://` を含む値は全拒否（url() 以外の関数＝`image-set("https://…")` 等への
///   文字列 URL 密輸を遮断。data: URL には `://` が現れないため誤爆しない）
/// - `image-set(`/`src(` は URL を取り得る関数のため全拒否
fn style_is_safe(value: &str) -> bool {
    let lower = value.to_lowercase();
    if lower.contains('\\')
        || lower.contains("://")
        || lower.contains('<')
        || lower.contains("expression(")
        || lower.contains("-moz-binding")
        || lower.contains("image-set(")
        || lower.contains("src(")
    {
        return false;
    }
    // すべての url( ... ) の中身が data: で始まることを確認する（引用符・空白は剥がす）。
    let mut rest = lower.as_str();
    while let Some(pos) = rest.find("url(") {
        let inner = &rest[pos + 4..];
        let inner = inner
            .trim_start()
            .trim_start_matches(['"', '\''])
            .trim_start();
        if !inner.starts_with("data:") {
            return false;
        }
        rest = &rest[pos + 4..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scriptとイベントハンドラを除去する() {
        let dirty = r#"<div onclick="alert(1)"><script>alert(2)</script><p>本文</p></div>"#;
        let clean = sanitize_html(dirty);
        assert!(!clean.contains("script"));
        assert!(!clean.contains("onclick"));
        assert!(!clean.contains("alert"));
        assert!(clean.contains("<p>本文</p>"));
    }

    #[test]
    fn iframe_object_embed_formを除去する() {
        let dirty = r#"<iframe src="https://evil"></iframe><object data="x"></object><embed src="x"><form action="https://evil"><input></form><p>ok</p>"#;
        let clean = sanitize_html(dirty);
        for banned in ["iframe", "object", "embed", "<form", "<input"] {
            assert!(!clean.contains(banned), "{banned} が残留: {clean}");
        }
        assert!(clean.contains("<p>ok</p>"));
    }

    #[test]
    fn javascriptスキームのリンクを除去する() {
        let dirty = r#"<a href="javascript:alert(1)">x</a><a href="https://example.com">ok</a>"#;
        let clean = sanitize_html(dirty);
        assert!(!clean.contains("javascript:"));
        assert!(clean.contains("https://example.com"));
    }

    #[test]
    fn styleの外部url参照を属性ごと落とす() {
        let dirty =
            r#"<div style="background-image: url('https://evil/x.png'); color: red">x</div>"#;
        let clean = sanitize_html(dirty);
        assert!(!clean.contains("evil"));
        assert!(!clean.contains("style="));
        // data: URL の style は保持される。
        let ok =
            r#"<div style="background-image: url(data:image/png;base64,AAAA); color: red">x</div>"#;
        let cleaned = sanitize_html(ok);
        assert!(
            cleaned.contains("style="),
            "data: url の style が消えた: {cleaned}"
        );
    }

    #[test]
    fn 画像srcはdataのみ許可しドライブ参照属性は保持する() {
        let dirty = r#"<img src="https://evil/t.png" data-shiki-node="0190" alt="x"><img src="data:image/png;base64,AA">"#;
        let clean = sanitize_html(dirty);
        assert!(!clean.contains("evil"));
        assert!(clean.contains("data-shiki-node"));
        assert!(clean.contains("data:image/png"));
    }

    #[test]
    fn styleタグの中身ごと除去する() {
        let dirty = "<style>body{background:url(https://evil)}</style><p>ok</p>";
        let clean = sanitize_html(dirty);
        assert!(!clean.contains("evil"));
        assert!(clean.contains("<p>ok</p>"));
    }

    #[test]
    fn cssエスケープ難読化と文字列url密輸を拒否する() {
        // CSS エスケープで url( を難読化（u\72l = url）。
        let escaped = r#"<div style="background-image:u\72l(https://evil/p.png)">x</div>"#;
        assert!(!sanitize_html(escaped).contains("style="));
        // image-set への文字列 URL 密輸。
        let image_set = r#"<div style='background:image-set("https://evil/p.png" 1x)'>x</div>"#;
        assert!(!sanitize_html(image_set).contains("style="));
        // :// を含む値は関数を問わず拒否。
        let smuggle = r#"<div style="--x:'https://evil'">x</div>"#;
        assert!(!sanitize_html(smuggle).contains("style="));
    }

    #[test]
    fn サニタイズは冪等() {
        let dirty = r#"<div style="color:red" onclick="x()"><b>a</b><script>1</script></div>"#;
        let once = sanitize_html(dirty);
        assert_eq!(sanitize_html(&once), once);
    }
}
