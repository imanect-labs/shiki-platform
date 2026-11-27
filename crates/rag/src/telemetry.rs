//! RAG の計装（`tracing` span → OTel・design §4.9）。
//!
//! span は**トレイトの呼び出し側**で付ける（[`EmbeddingProvider`]・[`DocumentParser`]・
//! [`VectorStore`](crate::vector_store::VectorStore)・[`Reranker`](crate::rerank::Reranker) の
//! 各実装ではなく）。cloud/onprem の差し替えで計装が抜けたり形が変わったりしないようにする。
//!
//! **本文は載せない。** 検索クエリ・文書本文・ファイル名は span に入れず、件数・モデル・
//! 所要時間だけを残す（本文の可視化は Langfuse の担当）。

use authz::AuthContext;
use tracing::{field::Empty, Instrument, Span};

use crate::authz_filter::{PostFilterOutcome, ReadableSet};
use crate::embedding::{EmbedInput, EmbedResponse, EmbeddingProvider};
use crate::error::RagError;
use crate::parser::{DocumentParser, ParseRequest};
use crate::search_types::{SearchDebug, SearchMode};
use crate::types::ParsedDocument;

/// 失敗を span に刻む（OTel の status と `error.type`）。文言は載せない（本文が混ざり得る）。
pub(crate) fn record_error(span: &Span, e: &RagError) {
    span.record("otel.status_code", "ERROR");
    span.record("error.type", error_kind(e));
}

/// エラーの種別名（`error.type` 属性）。
fn error_kind(e: &RagError) -> &'static str {
    match e {
        RagError::Parse { .. } => "parse",
        RagError::Http(_) => "http",
        RagError::EmbeddingVersionMismatch { .. } => "embedding_version_mismatch",
        RagError::Worker(_) => "worker",
        RagError::Vector(_) => "vector",
        RagError::Fulltext(_) => "fulltext",
        RagError::Authz(_) => "authz",
        RagError::Db(_) => "db",
        RagError::Jobq(_) => "jobq",
        RagError::Storage(_) => "storage",
        RagError::Serde(_) => "serde",
        RagError::Config(_) => "config",
        RagError::ChunkIdConflict { .. } => "chunk_id_conflict",
        RagError::Source(_) => "source",
        RagError::InvalidTenant(_) => "invalid_tenant",
    }
}

/// 結果に応じて span の status を刻む。
pub(crate) fn record_result<T>(span: &Span, res: &Result<T, RagError>) {
    if let Err(e) = res {
        record_error(span, e);
    }
}

/// 埋め込み（`rag.embed`）。入力の種類・件数・モデル版・次元を残す。
pub(crate) async fn embed(
    embedder: &dyn EmbeddingProvider,
    ctx: &AuthContext,
    input: EmbedInput,
    texts: &[String],
) -> Result<EmbedResponse, RagError> {
    let span = tracing::info_span!(
        "rag.embed",
        "rag.embed.input_type" = input.as_str(),
        "rag.embed.input_count" = texts.len(),
        "rag.embed.model" = embedder.model_version(),
        "rag.embed.dimension" = Empty,
        "otel.status_code" = Empty,
        "error.type" = Empty,
    );
    let res = embedder
        .embed(ctx, input, texts)
        .instrument(span.clone())
        .await;
    match &res {
        Ok(r) => {
            span.record("rag.embed.dimension", r.dimension);
        }
        Err(e) => record_error(&span, e),
    }
    res
}

/// 文書のパース（`rag.parse`）。content_type と出力ブロック数を残す（ファイル名は載せない）。
pub(crate) async fn parse(
    parser: &dyn DocumentParser,
    ctx: &AuthContext,
    req: ParseRequest<'_>,
) -> Result<ParsedDocument, RagError> {
    let span = tracing::info_span!(
        "rag.parse",
        "rag.parse.content_type" = req.content_type,
        "rag.parse.blocks" = Empty,
        "otel.status_code" = Empty,
        "error.type" = Empty,
    );
    let res = parser.parse(ctx, req).instrument(span.clone()).await;
    match &res {
        Ok(doc) => {
            span.record("rag.parse.blocks", doc.blocks.len());
        }
        Err(e) => record_error(&span, e),
    }
    res
}

/// pre-filter（可読集合の算出・`rag.pre_filter`）。可読タグ数と縮退の有無を残す。
pub(crate) async fn pre_filter(
    fut: impl std::future::Future<Output = Result<ReadableSet, RagError>>,
) -> Result<ReadableSet, RagError> {
    let span = tracing::info_span!(
        "rag.pre_filter",
        "rag.prefilter.readable_tags" = Empty,
        "rag.prefilter.overflowed" = Empty,
        "otel.status_code" = Empty,
        "error.type" = Empty,
    );
    let res = fut.instrument(span.clone()).await;
    match &res {
        Ok(r) => {
            span.record("rag.prefilter.readable_tags", r.tags.len());
            span.record("rag.prefilter.overflowed", r.overflowed);
        }
        Err(e) => record_error(&span, e),
    }
    res
}

/// post-filter（OpenFGA の file 粒度の再検証・`rag.post_filter`）。バックフィルのラウンドごとに
/// 1 つ。入力・許可・拒否の件数を残す（二段 authz で何件落ちたか）。
pub(crate) async fn post_filter(
    round: u32,
    input: usize,
    fut: impl std::future::Future<Output = Result<PostFilterOutcome, RagError>>,
) -> Result<PostFilterOutcome, RagError> {
    let span = tracing::info_span!(
        "rag.post_filter",
        "rag.postfilter.round" = round,
        "rag.postfilter.input" = input,
        "rag.postfilter.allowed" = Empty,
        "rag.postfilter.denied_chunks" = Empty,
        "rag.postfilter.denied_files" = Empty,
        "otel.status_code" = Empty,
        "error.type" = Empty,
    );
    let res = fut.instrument(span.clone()).await;
    match &res {
        Ok(o) => {
            span.record("rag.postfilter.allowed", o.allowed.len());
            span.record("rag.postfilter.denied_chunks", o.denied_chunks);
            span.record("rag.postfilter.denied_files", o.denied_files);
        }
        Err(e) => record_error(&span, e),
    }
    res
}

/// 検索全体（`rag.search`）の span。件数は終わってから [`record_search`] で埋める。
pub(crate) fn search_span(top_k: usize, mode: SearchMode, scoped: bool) -> Span {
    tracing::info_span!(
        "rag.search",
        "rag.search.top_k" = top_k,
        "rag.search.mode" = mode_str(mode),
        "rag.search.scoped" = scoped,
        "rag.prefilter.mode" = Empty,
        "rag.prefilter.readable_tags" = Empty,
        "rag.search.dense_hits" = Empty,
        "rag.search.keyword_hits" = Empty,
        "rag.postfilter.input" = Empty,
        "rag.postfilter.allowed" = Empty,
        "rag.postfilter.denied_chunks" = Empty,
        "rag.postfilter.denied_files" = Empty,
        "rag.search.hydrate_dropped" = Empty,
        "rag.search.backfill_rounds" = Empty,
        "rag.search.reranked" = Empty,
        "rag.search.below_min_score" = Empty,
        "rag.search.hits" = Empty,
        "otel.status_code" = Empty,
        "error.type" = Empty,
    )
}

/// 検索の件数を span に刻む。pre-filter（可読タグ）と post-filter（file check）の前後が
/// 分かるようにする（二段 authz で何件落ちたか）。
pub(crate) fn record_search(span: &Span, debug: &SearchDebug, hits: usize) {
    span.record("rag.prefilter.mode", debug.prefilter_mode.as_str());
    span.record("rag.prefilter.readable_tags", debug.readable_tags);
    span.record("rag.search.dense_hits", debug.dense_hits);
    span.record("rag.search.keyword_hits", debug.keyword_hits);
    span.record("rag.postfilter.input", debug.fused);
    span.record(
        "rag.postfilter.allowed",
        debug.fused.saturating_sub(debug.authz_denied_chunks),
    );
    span.record("rag.postfilter.denied_chunks", debug.authz_denied_chunks);
    span.record("rag.postfilter.denied_files", debug.authz_denied_files);
    span.record("rag.search.hydrate_dropped", debug.hydrate_dropped);
    span.record("rag.search.backfill_rounds", debug.backfill_rounds);
    span.record("rag.search.reranked", debug.reranked);
    span.record("rag.search.below_min_score", debug.below_min_score);
    span.record("rag.search.hits", hits);
}

const fn mode_str(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Hybrid => "hybrid",
        SearchMode::Dense => "dense",
        SearchMode::Keyword => "keyword",
    }
}

#[cfg(test)]
#[path = "../tests/common/capture.rs"]
mod capture;

#[cfg(test)]
mod tests {
    use super::capture::Captured;
    use super::*;

    #[test]
    fn error_kind_names_each_variant_without_message() {
        let span = tracing::Span::none();
        // Span::none でも record は no-op で落ちない。
        record_error(&span, &RagError::Worker("secret body".into()));
        assert_eq!(error_kind(&RagError::Worker("x".into())), "worker");
        assert_eq!(error_kind(&RagError::Vector("x".into())), "vector");
        assert_eq!(error_kind(&RagError::Config("x".into())), "config");
        assert_eq!(
            error_kind(&RagError::Parse {
                code: "c".into(),
                detail: "d".into()
            }),
            "parse"
        );
    }

    #[test]
    fn record_search_reports_counts_before_and_after_post_filter() {
        let cap = Captured::default();
        cap.run(|| {
            let span = search_span(5, SearchMode::Hybrid, true);
            let debug = SearchDebug {
                prefilter_mode: "tags".into(),
                readable_tags: 3,
                dense_hits: 10,
                keyword_hits: 7,
                fused: 12,
                authz_denied_chunks: 4,
                authz_denied_files: 2,
                backfill_rounds: 1,
                reranked: 8,
                ..SearchDebug::default()
            };
            record_search(&span, &debug, 5);
        });
        let s = cap.span("rag.search");
        assert_eq!(s.field("rag.search.top_k"), "5");
        assert_eq!(s.field("rag.search.mode"), "hybrid");
        assert_eq!(s.field("rag.prefilter.mode"), "tags");
        assert_eq!(s.field("rag.postfilter.input"), "12");
        assert_eq!(s.field("rag.postfilter.allowed"), "8");
        assert_eq!(s.field("rag.postfilter.denied_chunks"), "4");
        assert_eq!(s.field("rag.search.hits"), "5");
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t".into()),
            },
            "acme".into(),
            authz::TenantId::parse("t").unwrap(),
        )
    }

    struct Embedder {
        fail: bool,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for Embedder {
        async fn embed(
            &self,
            _ctx: &AuthContext,
            _input: EmbedInput,
            texts: &[String],
        ) -> Result<EmbedResponse, RagError> {
            tracing::info!("worker called");
            if self.fail {
                return Err(RagError::Worker("本文を含むかもしれない失敗".into()));
            }
            Ok(EmbedResponse {
                vectors: texts.iter().map(|_| vec![0.0; 4]).collect(),
                model_version: "m-1".into(),
                dimension: 4,
            })
        }
        fn model_version(&self) -> &'static str {
            "m-1"
        }
    }

    struct Parser;

    #[async_trait::async_trait]
    impl DocumentParser for Parser {
        async fn parse(
            &self,
            _ctx: &AuthContext,
            _req: ParseRequest<'_>,
        ) -> Result<ParsedDocument, RagError> {
            Ok(ParsedDocument {
                blocks: vec![],
                used_ocr: false,
            })
        }
    }

    #[tokio::test]
    async fn embed_span_records_count_and_model_but_not_text() {
        let cap = Captured::default();
        let _g = cap.guard();
        let texts = vec!["社外秘の検索語".to_string(), "二件目".to_string()];
        let out = embed(&Embedder { fail: false }, &ctx(), EmbedInput::Query, &texts)
            .await
            .unwrap();
        assert_eq!(out.vectors.len(), 2);
        let s = cap.span("rag.embed");
        assert_eq!(s.field("rag.embed.input_type"), "query");
        assert_eq!(s.field("rag.embed.input_count"), "2");
        assert_eq!(s.field("rag.embed.model"), "m-1");
        assert_eq!(s.field("rag.embed.dimension"), "4");
        // 実装の中のログは span の内側で出る（span がトレイト呼び出しを包んでいる）。
        assert_eq!(s.events, vec!["worker called".to_string()]);
        assert!(s.closed);
        assert!(!cap.all_values().iter().any(|v| v.contains("社外秘")));
    }

    #[tokio::test]
    async fn embed_span_marks_error_without_message() {
        let cap = Captured::default();
        let _g = cap.guard();
        let res = embed(&Embedder { fail: true }, &ctx(), EmbedInput::Document, &[]).await;
        assert!(res.is_err());
        let s = cap.span("rag.embed");
        assert_eq!(s.field("otel.status_code"), "ERROR");
        assert_eq!(s.field("error.type"), "worker");
        assert!(!cap.all_values().iter().any(|v| v.contains("本文")));
    }

    #[tokio::test]
    async fn parse_span_records_content_type_and_blocks_but_not_file_name() {
        let cap = Captured::default();
        let _g = cap.guard();
        let req = ParseRequest {
            source: crate::parser::ParseSource::Url("http://worker/blob"),
            content_type: "application/pdf",
            file_name: "人事評価.pdf",
        };
        parse(&Parser, &ctx(), req).await.unwrap();
        let s = cap.span("rag.parse");
        assert_eq!(s.field("rag.parse.content_type"), "application/pdf");
        assert_eq!(s.field("rag.parse.blocks"), "0");
        assert!(!cap.all_values().iter().any(|v| v.contains("人事評価")));
    }
}
