//! `word/document.xml` の生成。

use std::fmt::Write as _;

use crate::model::{Block, JtdDocument, Paragraph};

/// A4 縦の用紙サイズ（twip = 1/1440 インチ）。
///
/// **暫定値。** 実際の用紙・余白は `PaperMark` / `PageMark` に入っているが、
/// その解読は JTD.4 の範囲。ここで A4 を決め打ちにしているのは、
/// 用紙指定が無いと Word 側の既定（環境により Letter）に落ちて、
/// 日本語文書として明らかにおかしくなるため。
const PAGE_WIDTH_TWIPS: u32 = 11906;
const PAGE_HEIGHT_TWIPS: u32 = 16838;
/// 余白（twip）。同じく JTD.4 で実値に差し替える。
const MARGIN_TWIPS: u32 = 1134; // 20mm

/// [`JtdDocument`] を `word/document.xml` の文字列にする。
pub(super) fn render(document: &JtdDocument) -> String {
    let mut body = String::new();
    for block in document.blocks() {
        let Block::Paragraph(paragraph) = block;
        render_paragraph(&mut body, paragraph);
    }

    // 段落が 1 つも無い docx は Word が壊れていると見なすため、空文書には空段落を置く。
    if body.is_empty() {
        body.push_str("<w:p/>");
    }

    let width = PAGE_WIDTH_TWIPS;
    let height = PAGE_HEIGHT_TWIPS;
    let margin = MARGIN_TWIPS;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>{body}<w:sectPr>
<w:pgSz w:w="{width}" w:h="{height}"/>
<w:pgMar w:top="{margin}" w:right="{margin}" w:bottom="{margin}" w:left="{margin}" w:header="0" w:footer="0" w:gutter="0"/>
</w:sectPr></w:body>
</w:document>"#
    )
}

/// 段落 1 つを書き出す。
fn render_paragraph(out: &mut String, paragraph: &Paragraph) {
    out.push_str("<w:p>");
    for run in paragraph.runs() {
        render_run(out, run.text());
    }
    out.push_str("</w:p>");
}

/// ラン 1 つを書き出す。段落内の改行は `<w:br/>` にする。
fn render_run(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    out.push_str("<w:r>");
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push_str("<w:br/>");
        }
        if line.is_empty() {
            continue;
        }
        // `xml:space="preserve"` は必須。全角スペースの連なりが升目を作っているので、
        // 空白が畳まれると原本の見た目が壊れる。
        let _ = write!(
            out,
            r#"<w:t xml:space="preserve">{}</w:t>"#,
            quick_xml::escape::escape(line)
        );
    }
    out.push_str("</w:r>");
}
