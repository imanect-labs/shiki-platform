//! shiki-rag — permission-aware RAG（インジェスト＋検索・Phase 2）。
//!
//! 正本設計: docs/design.md §4.3。要点:
//! - **二段 authz**: pre-filter（authz_tags ∩ 可読集合・PIT-1 (b) 権限定義オブジェクト方式）
//!   ＋ post-filter（OpenFGA file 粒度検証）。片方が壊れても権限を守る。
//! - **テナント分離**: Qdrant は payload `tenant_id` を無条件 AND、Tantivy は
//!   index-per-tenant。authz_tags とは独立の防壁。
//! - **差し替え点**: [`DocumentParser`] / [`EmbeddingProvider`] / [`Reranker`] /
//!   `VectorStore` / `FulltextIndex` はトレイト裏（クラウド/オンプレ差はここで吸収）。
//! - 公開トレイトの第一引数は `&AuthContext`。

pub mod chunker;
pub mod config;
pub mod embedding;
pub mod error;
pub mod parser;
pub mod parser_http;
pub mod rerank;
pub mod types;

pub use config::RagConfig;
pub use embedding::{EmbedInput, EmbeddingProvider, HttpEmbeddingProvider};
pub use error::RagError;
pub use parser::{DocumentParser, ParseRequest};
pub use parser_http::HttpDocumentParser;
pub use rerank::{HttpReranker, Reranker};
