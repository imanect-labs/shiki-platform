//! shiki-websearch — web 検索の可搬トレイト [`SearchProvider`] と実装群（Phase 4 web ツール）。
//!
//! 設計の正本: docs/design.md §3.1（トレイト境界）・§4.4（web ツール）。
//! クラウド/オンプレの差はこのトレイト裏で吸収する:
//! - [`BraveSearchProvider`] — SaaS（Brave Search API・API キーは config）。
//! - [`SearxngSearchProvider`] — オンプレ（compose の SearXNG・自己ホスト）。
//! - [`StubSearchProvider`] — テスト/エアギャップ（決定的・外部依存なし）。
//!
//! 検索は **shiki-server ホスト側**で実行する（サンドボックス非経由）。sandbox egress を使うのは
//! ページ取得（web_fetch ツール・agent-core 側）のみ。

// #[cfg(test)] は本番のみ厳格化する lint を許容する。
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::pedantic
    )
)]

mod brave;
mod searxng;
mod stub;

use authz::AuthContext;

pub use brave::BraveSearchProvider;
pub use searxng::SearxngSearchProvider;
pub use stub::StubSearchProvider;

/// 検索ヒット 1 件（プロバイダ非依存の共通形）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 検索エラー。
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// クエリ不正（空など）。
    #[error("invalid query: {0}")]
    Invalid(String),
    /// プロバイダ側の一時障害（HTTP エラー・タイムアウト）。
    #[error("search provider unavailable: {0}")]
    Unavailable(String),
}

/// web 検索の可搬トレイト（差し替え点）。shiki-server はこれだけに依存する。
///
/// `ctx` は監査・（将来の）テナント別クォータのために受け取る。認可判定は不要
/// （検索対象は社外の公開 web であり、権限考慮が要る社内文書検索は rag::SearchService が担う）。
#[async_trait::async_trait]
pub trait SearchProvider: Send + Sync {
    /// プロバイダ名（監査・表示用）。
    fn name(&self) -> &'static str;

    /// クエリを実行し、上位 `max_results` 件を返す。
    async fn search(
        &self,
        ctx: &AuthContext,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchHit>, SearchError>;
}

/// クエリの共通検証（空を拒否・過長を切る）。各実装が呼ぶ。
pub(crate) fn validate_query(query: &str) -> Result<&str, SearchError> {
    let q = query.trim();
    if q.is_empty() {
        return Err(SearchError::Invalid("query is empty".into()));
    }
    Ok(q)
}
