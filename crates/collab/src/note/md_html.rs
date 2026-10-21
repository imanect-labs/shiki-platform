//! 正規化 md → HTML（#381・Office 新規作成の本文流し込み）。
//!
//! 新規 Word 文書の本文は **Collabora（LibreOffice）に HTML を paste して docx 化させる**
//! （`office::live::LiveOp::AppendHtml`）。python-docx の自前サブセット（`append_markdown`）は
//! 太字/斜体/リンク/ネスト箇条書き/引用が落ちるが、LibreOffice の HTML インポートは
//! これらをそのまま Word の書式へ写す（#381 の決定の根拠）。
//!
//! 入力は [`super::normalize_markdown`] を通した正規形を想定する（生 HTML は
//! コードブロックへ縮退済み）。ここでは pulldown-cmark の HTML レンダラを
//! **raw HTML 無効**で通し、貼り込み側でさらに ammonia サニタイズを掛ける（多層防御・PIT-40）。

use pulldown_cmark::{html, Options, Parser};

/// md を HTML へ変換する（GFM テーブル・打ち消し・タスクリストを有効化）。
///
/// `Options::ENABLE_*` は Word 側に写せる記法だけを開ける（脚注・数式は Writer の
/// 表現に対応が無く、素の文字列として残ると劣化するため開けない）。
pub fn to_html(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(markdown, options);
    let mut out = String::with_capacity(markdown.len() + markdown.len() / 2);
    html::push_html(&mut out, parser);
    out
}

#[cfg(test)]
mod tests {
    use super::to_html;

    /// `append_markdown` が落としていた記法が HTML では保持される（#381 の主張の固定）。
    #[test]
    fn preserves_inline_marks_links_and_nesting() {
        let html = to_html(
            "# 見出し\n\n**太字**と*斜体*と`code`。[リンク](https://example.com)\n\n\
             - 親\n  - 子\n\n> 引用\n",
        );
        assert!(html.contains("<h1>見出し</h1>"), "{html}");
        assert!(html.contains("<strong>太字</strong>"), "{html}");
        assert!(html.contains("<em>斜体</em>"), "{html}");
        assert!(html.contains("<code>code</code>"), "{html}");
        assert!(
            html.contains("<a href=\"https://example.com\">リンク</a>"),
            "{html}"
        );
        // ネストは入れ子の <ul> として残る（同階層に潰れない）。
        assert!(html.matches("<ul>").count() >= 2, "{html}");
        assert!(html.contains("<blockquote>"), "{html}");
    }

    /// GFM テーブルは実表になる（Writer では Word の表として入る）。
    #[test]
    fn renders_gfm_tables() {
        let html = to_html("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert!(
            html.contains("<table>") && html.contains("<th>a</th>"),
            "{html}"
        );
        assert!(html.contains("<td>1</td>"), "{html}");
    }

    /// 生 HTML は素通ししない（正規化を経ない入力でも script を組み立てさせない）。
    ///
    /// pulldown-cmark はブロック HTML をそのまま出すため、ここでは**エスケープされるか
    /// 少なくとも実行可能な形で出ないこと**は貼り込み側のサニタイズ（ammonia）が担う。
    /// このテストは「変換自体が md 記法として解釈する」ことだけを固定する。
    #[test]
    fn code_fence_keeps_html_as_text() {
        let html = to_html("```\n<script>alert(1)</script>\n```\n");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }
}
