//! Test fixtures and helpers shared by rjtd crates.

use rjtd_model::{Block, Document, Inline, Metadata, Paragraph, TextRun};

pub fn document_with_text(text: impl Into<String>) -> Document {
    let paragraph = Paragraph::new(vec![Inline::Text(TextRun::new(text, None))], None);
    Document::new(Metadata::default(), vec![Block::Paragraph(paragraph)])
}
