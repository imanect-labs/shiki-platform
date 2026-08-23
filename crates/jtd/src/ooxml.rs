//! [`JtdDocument`] → docx（OOXML）バイト列（トラックJTD）。
//!
//! # なぜ自前で書くのか
//!
//! 既存の `DocxComposer`（`crates/office`）は Python worker の python-docx 経由で、
//! セル単位の罫線・絶対配置フレーム・ルビ・段組・OMML を組むには足りない。
//! 後続タスク（表・罫線 → ページ幾何 → 文字書式 → 画像）で必要になるものが
//! 最初から書けないので、ここは `zip` ＋ `quick-xml` で直接 OOXML を書く。
//!
//! # 生成するパッケージ
//!
//! Word が開ける最小構成に絞る。部品を増やすのは、それを使う機能が入るときにする。
//!
//! ```text
//! [Content_Types].xml          各パートの MIME
//! _rels/.rels                  パッケージ → document
//! word/document.xml            本文
//! word/_rels/document.xml.rels document → styles
//! word/styles.xml              既定フォント（日本語）
//! ```
//!
//! # 日本語での注意点
//!
//! - `w:t` には必ず `xml:space="preserve"` を付ける。申請書の記入欄は**全角スペースの
//!   連なりで升目を作っている**ので、空白が畳まれると原本の見た目が壊れる。
//! - 既定フォントは `w:eastAsia` を明示する。指定しないと日本語がプロポーショナルの
//!   欧文フォントに落ちて字送りが崩れる。

mod document;
mod parts;

use std::io::{Cursor, Write as _};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::error::JtdError;
use crate::model::JtdDocument;

/// docx の MIME。
pub const DOCX_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// [`JtdDocument`] を docx バイト列へ書き出す。
///
/// **表・罫線・ページ幾何はまだ写らない。** 段落として縦に並ぶ（JTD.3 / JTD.4 の範囲）。
pub(crate) fn to_docx(document: &JtdDocument) -> Result<Vec<u8>, JtdError> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for (path, contents) in [
        ("[Content_Types].xml", parts::CONTENT_TYPES.to_string()),
        ("_rels/.rels", parts::PACKAGE_RELS.to_string()),
        (
            "word/_rels/document.xml.rels",
            parts::DOCUMENT_RELS.to_string(),
        ),
        ("word/styles.xml", parts::styles()),
        ("word/document.xml", document::render(document)),
    ] {
        zip.start_file(path, options)
            .map_err(|error| map_zip(&error))?;
        zip.write_all(contents.as_bytes()).map_err(|error| {
            tracing::debug!(%error, part = path, "jtd: docx パートの書き込みに失敗しました");
            JtdError::DocxWrite
        })?;
    }

    let cursor = zip.finish().map_err(|error| map_zip(&error))?;
    Ok(cursor.into_inner())
}

fn map_zip(error: &zip::result::ZipError) -> JtdError {
    tracing::debug!(%error, "jtd: docx パッケージの組み立てに失敗しました");
    JtdError::DocxWrite
}

#[cfg(test)]
mod tests;
