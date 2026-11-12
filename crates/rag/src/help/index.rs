use super::corpus::{invalid, HelpChunk};
use crate::{fulltext_tantivy::SharedJaTokenizer, RagError};
use tantivy::{
    collector::TopDocs,
    query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery},
    schema::{Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED},
    Index, IndexReader, TantivyDocument, Term,
};
pub(super) struct HelpIndex {
    index: Index,
    reader: IndexReader,
    text: Field,
    title: Field,
    ordinal: Field,
}
impl HelpIndex {
    pub(super) fn build(chunks: &[HelpChunk]) -> Result<Self, RagError> {
        let mut schema = Schema::builder();
        let text = schema.add_text_field(
            "text",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("help_ja")
                    .set_index_option(IndexRecordOption::WithFreqs),
            ),
        );
        let title = schema.add_text_field(
            "title",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("help_ja")
                    .set_index_option(IndexRecordOption::WithFreqs),
            ),
        );
        let ordinal = schema.add_u64_field("ordinal", STORED);
        let index = Index::create_in_ram(schema.build());
        index
            .tokenizers()
            .register("help_ja", SharedJaTokenizer::new()?);
        let mut writer = index
            .writer_with_num_threads(1, 20_000_000)
            .map_err(error)?;
        for (i, c) in chunks.iter().enumerate() {
            let mut doc = TantivyDocument::new();
            doc.add_u64(ordinal, i as u64);
            doc.add_text(
                title,
                format!(
                    "{} {} {} {}",
                    c.title,
                    c.summary,
                    c.tags.join(" "),
                    c.aliases.join(" ")
                ),
            );
            doc.add_text(
                text,
                format!("{} {} {}", c.title, c.heading_path.join(" "), c.text),
            );
            writer.add_document(doc).map_err(error)?;
        }
        writer.commit().map_err(error)?;
        let reader = index.reader().map_err(error)?;
        Ok(Self {
            index,
            reader,
            text,
            title,
            ordinal,
        })
    }
    pub(super) fn search(&self, query: &str, limit: usize) -> Result<Vec<usize>, RagError> {
        let mut analyzer = self
            .index
            .tokenizers()
            .get("help_ja")
            .ok_or_else(|| invalid("tokenizer"))?;
        let mut stream = analyzer.token_stream(query);
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        while let Some(token) = stream.next() {
            clauses.push((
                Occur::Should,
                Box::new(BoostQuery::new(
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.title, &token.text),
                        IndexRecordOption::WithFreqs,
                    )),
                    3.0,
                )),
            ));
            clauses.push((
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.text, &token.text),
                    IndexRecordOption::WithFreqs,
                )),
            ));
        }
        if clauses.is_empty() {
            return Ok(Vec::new());
        }
        let searcher = self.reader.searcher();
        searcher
            .search(&BooleanQuery::new(clauses), &TopDocs::with_limit(limit))
            .map_err(error)?
            .into_iter()
            .map(|(_, addr)| {
                let doc: TantivyDocument = searcher.doc(addr).map_err(error)?;
                doc.get_first(self.ordinal)
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or_else(|| invalid("ordinal"))
            })
            .collect()
    }
}
fn error(e: impl std::fmt::Display) -> RagError {
    RagError::Fulltext(e.to_string())
}
