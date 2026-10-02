//! JTD の中間モデル（トラックJTD）。
//!
//! 上流 `rjtd_core` の `ParsedDocumentText` は「読み順のテキスト」までしか持たず、
//! 表もページも表現できない。ここは**我々が OOXML へ写すために必要な形**を定義する場所で、
//! 後続タスク（表・罫線 → ページ幾何 → 文字書式 → 画像）が素直に伸ばせるよう、
//! 段階ごとに列挙子とフィールドを足していく前提で置いている。
//!
//! 本タスクの範囲は本文と段落まで。表とページは
//! [`Block`] / [`ParagraphKind`] に足す形で後から入る。

/// 1 つの JTD 文書。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JtdDocument {
    blocks: Vec<Block>,
}

impl JtdDocument {
    /// ブロック列から組む。
    pub fn new(blocks: Vec<Block>) -> Self {
        JtdDocument { blocks }
    }

    /// 読み順のブロック列。
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// ブロック列を取り出す（断片ごとに読んだ結果をつなぐため）。
    pub(crate) fn into_blocks(self) -> Vec<Block> {
        self.blocks
    }

    /// 読み順のプレーンテキスト（段落間は改行 1 つ）。
    ///
    /// 比較・検索用の平坦化であって、レイアウトは表現しない。
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        // 区切りは「出力が空かどうか」ではなく位置で決める。空段落が先頭にあると
        // 前者では区切りが落ち、段落数と改行数が合わなくなる。
        for (index, block) in self.blocks.iter().enumerate() {
            let Block::Paragraph(paragraph) = block;
            if index > 0 {
                out.push('\n');
            }
            for run in paragraph.runs() {
                out.push_str(run.text());
            }
        }
        out
    }
}

/// 文書を構成するブロック。
///
/// 現状は段落のみ。表（`Table`）は JTD.3 で、ページ区切りは JTD.4 で足す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// 段落（論理段落。`0x001C class=0x0010` レコードが始点）。
    Paragraph(Paragraph),
}

/// 段落。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paragraph {
    runs: Vec<TextRun>,
}

impl Paragraph {
    /// ラン列から組む。
    pub fn new(runs: Vec<TextRun>) -> Self {
        Paragraph { runs }
    }

    /// 文字列 1 本の段落を組む（テスト・単純な変換用）。
    pub fn from_text(text: impl Into<String>) -> Self {
        Paragraph {
            runs: vec![TextRun::new(text)],
        }
    }

    /// ラン列。
    pub fn runs(&self) -> &[TextRun] {
        &self.runs
    }

    /// 段落が文字を 1 つも持たないか。
    ///
    /// 全角スペースだけの行は**空ではない**。申請書の記入欄はそれで組まれているので、
    /// 空白を落とすと升目が消える。
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text().is_empty())
    }

    /// 段落のテキストを連結する。
    pub fn text(&self) -> String {
        self.runs.iter().map(TextRun::text).collect()
    }
}

/// 連続する 1 続きの文字列。
///
/// 文字書式（フォント・太字・サイズ）は JTD.5 でここに足す。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextRun {
    text: String,
}

impl TextRun {
    /// 文字列からランを作る。
    pub fn new(text: impl Into<String>) -> Self {
        TextRun { text: text.into() }
    }

    /// 文字列。
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::{Block, JtdDocument, Paragraph, TextRun};

    #[test]
    fn plain_text_joins_paragraphs_with_newlines() {
        let document = JtdDocument::new(vec![
            Block::Paragraph(Paragraph::from_text("一行目")),
            Block::Paragraph(Paragraph::from_text("二行目")),
        ]);

        assert_eq!(document.plain_text(), "一行目\n二行目");
    }

    #[test]
    fn paragraph_concatenates_runs() {
        let paragraph = Paragraph::new(vec![TextRun::new("あ"), TextRun::new("い")]);

        assert_eq!(paragraph.text(), "あい");
        assert!(!paragraph.is_empty());
    }

    #[test]
    fn ideographic_space_is_not_empty() {
        // 申請書の記入欄は全角スペースで組まれている。空扱いにすると升目が消える。
        let paragraph = Paragraph::from_text("　　　");

        assert!(!paragraph.is_empty(), "全角スペースだけの段落は空ではない");
    }

    #[test]
    fn paragraph_without_runs_is_empty() {
        assert!(Paragraph::default().is_empty());
        assert!(Paragraph::from_text("").is_empty());
    }

    #[test]
    fn empty_document_has_empty_text() {
        assert_eq!(JtdDocument::default().plain_text(), "");
        assert!(JtdDocument::default().blocks().is_empty());
    }

    #[test]
    fn empty_leading_paragraph_still_separates() {
        // 区切りを「出力が空か」で決めると、先頭の空段落で区切りが落ちる。
        let document = JtdDocument::new(vec![
            Block::Paragraph(Paragraph::default()),
            Block::Paragraph(Paragraph::from_text("二行目")),
        ]);

        assert_eq!(document.plain_text(), "\n二行目");
    }
}
