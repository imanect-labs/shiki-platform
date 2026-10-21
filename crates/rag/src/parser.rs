//! `DocumentParser` トレイト（Task 2.1）。
//!
//! 文書パースの差し替え点（docs/design.md §3.1）。既定実装は ingestion-worker への
//! HTTP 呼び出し（[`HttpDocumentParser`](crate::parser_http::HttpDocumentParser)）で、
//! 将来別のパースサービスへ差し替えてもアプリ本体は変わらない。

use async_trait::async_trait;
use authz::AuthContext;

use crate::error::RagError;
use crate::types::ParsedDocument;

/// パース対象の与え方。
///
/// **どちらを使うかはセキュリティ上の選択**であって、利便性の選択ではない（#405）。
/// worker は `Url` を渡されると**自分で取りに行く**（httpx・SSRF ガード無し）。
/// したがって、我々が検証していない URL を `Url` で渡すと worker が confused deputy になり、
/// `web_fetch` が積み上げた宛先制限（解決後 IP 検証＋アドレス固定・PIT-48）を丸ごと迂回できる。
pub enum ParseSource<'a> {
    /// StorageService（IndexerStorage）が発行した内部向け・短 TTL の presigned GET URL。
    /// **我々のオブジェクトストアを指す URL に限る。**
    Url(&'a str),
    /// 呼び出し側が**既にガード済み経路で取得した**バイト列。worker は取得を行わない。
    /// 外部 URL 由来の文書（`web_fetch` の PDF 等）は必ずこちらを使う。
    Bytes(&'a [u8]),
}

/// パース要求。
pub struct ParseRequest<'a> {
    pub source: ParseSource<'a>,
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
