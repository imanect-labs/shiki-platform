//! `DocumentParser` トレイト（Task 2.1）。
//!
//! 文書パースの差し替え点（docs/design.md §3.1）。既定実装は ingestion-worker への
//! HTTP 呼び出し（[`HttpDocumentParser`](crate::parser_http::HttpDocumentParser)）で、
//! 将来別のパースサービスへ差し替えてもアプリ本体は変わらない。

use async_trait::async_trait;
use authz::AuthContext;

use crate::error::RagError;
use crate::types::ParsedDocument;

/// パース要求。`source_url` は StorageService（IndexerStorage）が発行した
/// 内部向け・短 TTL の presigned GET URL。
pub struct ParseRequest<'a> {
    pub source_url: &'a str,
    pub content_type: &'a str,
    pub file_name: &'a str,
}

/// 文書 → 構造化ブロック列（見出し・段落・表 Markdown・キャプション）のパース抽象。
///
/// 公開トレイトの第一引数は `&AuthContext`（tenant_id を worker まで必須で通す。
/// docs/design.md §4.3 のインジェスト経路 tenant 必須化）。
#[async_trait]
pub trait DocumentParser: Send + Sync {
    async fn parse(
        &self,
        ctx: &AuthContext,
        req: ParseRequest<'_>,
    ) -> Result<ParsedDocument, RagError>;
}
