//! docx パッケージの固定パート。
//!
//! 本文（`word/document.xml`）以外はどの文書でも同じなので、ここに定数として置く。
//! 部品を増やすのはそれを使う機能が入るときにする（ヘッダ・フッタは JTD.4、
//! 画像の rels は JTD.6）。

/// 既定の日本語フォント。
///
/// 一太郎の実文書は MS 明朝を前提に組まれている。ここを指定しないと日本語が
/// プロポーショナルの欧文フォントに落ち、全角スペースで作った升目の幅が崩れる。
/// 実際に使うフォントの解読は JTD.5（`Font` ストリーム）の範囲。
const DEFAULT_EAST_ASIAN_FONT: &str = "ＭＳ 明朝";
/// 既定の文字サイズ（half-point 単位。21 = 10.5pt ＝ 日本語文書の標準）。
const DEFAULT_HALF_POINTS: u32 = 21;

/// `[Content_Types].xml`。
pub(super) const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#;

/// `_rels/.rels`。
pub(super) const PACKAGE_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/// `word/_rels/document.xml.rels`。
pub(super) const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

/// `word/styles.xml`。既定フォントと段落間隔だけを持つ。
///
/// 段落間隔を 0 にするのは、JTD の 1 段落が 1 表示行に対応するため。
/// Word 既定の段落後スペースが入ると、原本にない空きが全行に付いて別物になる。
pub(super) fn styles() -> String {
    let font = DEFAULT_EAST_ASIAN_FONT;
    let size = DEFAULT_HALF_POINTS;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults>
<w:rPrDefault><w:rPr>
<w:rFonts w:ascii="{font}" w:hAnsi="{font}" w:eastAsia="{font}" w:cs="{font}"/>
<w:sz w:val="{size}"/><w:szCs w:val="{size}"/>
</w:rPr></w:rPrDefault>
<w:pPrDefault><w:pPr>
<w:spacing w:before="0" w:after="0" w:line="240" w:lineRule="auto"/>
</w:pPr></w:pPrDefault>
</w:docDefaults>
</w:styles>"#
    )
}
