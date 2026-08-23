//! OOXML ライタの単体テスト。
//!
//! 生成した docx を zip として開き直し、パートの有無と `document.xml` の中身を見る。
//! 実際に Word / Collabora で開けることの確認は `tests/fidelity_it.rs` と手動検証が担う。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Cursor, Read as _};

use super::to_docx;
use crate::model::{Block, JtdDocument, Paragraph, TextRun};

/// docx の中の 1 パートを文字列で取り出す。
fn part(docx: &[u8], path: &str) -> String {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(docx)).expect("docx は zip として開けること");
    let mut file = archive
        .by_name(path)
        .unwrap_or_else(|_| panic!("{path} が入っていること"));
    let mut out = String::new();
    file.read_to_string(&mut out).expect("UTF-8 であること");
    out
}

fn docx_of(paragraphs: &[&str]) -> Vec<u8> {
    let blocks = paragraphs
        .iter()
        .map(|text| Block::Paragraph(Paragraph::from_text(*text)))
        .collect();
    to_docx(&JtdDocument::new(blocks)).expect("書き出せること")
}

#[test]
fn produces_a_zip_package_with_the_required_parts() {
    let docx = docx_of(&["本文"]);

    assert_eq!(&docx[..2], b"PK", "zip（docx）ヘッダで始まること");
    for path in [
        "[Content_Types].xml",
        "_rels/.rels",
        "word/document.xml",
        "word/_rels/document.xml.rels",
        "word/styles.xml",
    ] {
        let _ = part(&docx, path);
    }
}

#[test]
fn writes_each_paragraph_as_a_single_w_p() {
    let xml = part(&docx_of(&["いち", "に", "さん"]), "word/document.xml");

    assert_eq!(xml.matches("<w:p>").count(), 3);
    assert!(xml.contains("いち"));
    assert!(xml.contains("さん"));
}

#[test]
fn preserves_ideographic_space_padding() {
    // 申請書の記入欄は全角スペースの連なりで升目を作っている。
    // `xml:space="preserve"` が無いと Word 側で畳まれ、原本の見た目が壊れる。
    let xml = part(&docx_of(&["氏　　名　　　　　"]), "word/document.xml");

    assert!(
        xml.contains(r#"<w:t xml:space="preserve">氏　　名　　　　　</w:t>"#),
        "全角スペースがそのまま保たれること: {xml}"
    );
}

#[test]
fn escapes_xml_metacharacters() {
    let xml = part(&docx_of(&["<tag> & \"quote\" 'apos'"]), "word/document.xml");

    assert!(xml.contains("&lt;tag&gt;"), "エスケープされること: {xml}");
    assert!(xml.contains("&amp;"));
    assert!(!xml.contains("<tag>"), "生の < が漏れていないこと");
}

#[test]
fn turns_line_breaks_inside_a_paragraph_into_br() {
    let xml = part(&docx_of(&["上\n下"]), "word/document.xml");

    assert_eq!(xml.matches("<w:p>").count(), 1, "段落は割らないこと");
    assert_eq!(xml.matches("<w:br/>").count(), 1);
    assert!(xml.contains("上"));
    assert!(xml.contains("下"));
}

#[test]
fn declares_a_japanese_default_font() {
    // 指定しないと日本語がプロポーショナルの欧文フォントに落ち、升目の幅が崩れる。
    let xml = part(&docx_of(&["本文"]), "word/styles.xml");

    assert!(xml.contains(r#"w:eastAsia="ＭＳ 明朝""#), "{xml}");
}

#[test]
fn sets_a4_portrait_page_size() {
    // 用紙指定が無いと Word の既定（環境により Letter）に落ちる。実値は JTD.4 で入る。
    let xml = part(&docx_of(&["本文"]), "word/document.xml");

    assert!(
        xml.contains(r#"<w:pgSz w:w="11906" w:h="16838"/>"#),
        "{xml}"
    );
}

#[test]
fn empty_document_still_produces_a_valid_body() {
    // 段落が 1 つも無い docx は Word が壊れていると見なす。
    let docx = to_docx(&JtdDocument::default()).expect("空でも書き出せること");
    let xml = part(&docx, "word/document.xml");

    assert!(xml.contains("<w:p/>"), "空段落が置かれること: {xml}");
}

#[test]
fn empty_runs_do_not_emit_empty_elements() {
    let document = JtdDocument::new(vec![Block::Paragraph(Paragraph::new(vec![
        TextRun::new(""),
        TextRun::new("実体"),
    ]))]);

    let xml = part(&to_docx(&document).unwrap(), "word/document.xml");

    assert_eq!(
        xml.matches("<w:r>").count(),
        1,
        "空ランは出さないこと: {xml}"
    );
}

#[test]
fn round_trips_characters_above_the_basic_plane() {
    let xml = part(&docx_of(&["𠮷田と𩸽"]), "word/document.xml");

    assert!(xml.contains("𠮷田と𩸽"), "{xml}");
}
