//! Brave Search API プロバイダ（SaaS 向け）。
//!
//! <https://api.search.brave.com/res/v1/web/search> を叩く。API キーは config 経由
//! （将来 crates/secrets へ移行・docs/miniapp-platform.md）。応答の解析は純関数
//! [`parse_brave`] に分離してフィクスチャでテストする。

use authz::AuthContext;
use serde::Deserialize;

use crate::{validate_query, SearchError, SearchHit, SearchProvider};

/// Brave Web Search API の既定エンドポイント。
const DEFAULT_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

/// Brave Search API プロバイダ。
pub struct BraveSearchProvider {
    http: reqwest::Client,
    api_key: String,
    endpoint: String,
}

impl BraveSearchProvider {
    /// `endpoint` が `None` なら既定の公開 API を使う（テストでは差し替える）。
    pub fn new(http: reqwest::Client, api_key: String, endpoint: Option<String>) -> Self {
        BraveSearchProvider {
            http,
            api_key,
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
        }
    }
}

// 応答のうち使うフィールドだけを写す（全件デシリアライズしない）。
#[derive(Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

/// Brave 応答 JSON → 共通 [`SearchHit`]（純関数・テスト対象）。
fn parse_brave(body: &str, max_results: usize) -> Result<Vec<SearchHit>, SearchError> {
    let resp: BraveResponse = serde_json::from_str(body)
        .map_err(|e| SearchError::Unavailable(format!("brave response parse: {e}")))?;
    Ok(resp
        .web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !r.url.is_empty())
        .take(max_results)
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.description,
        })
        .collect())
}

#[async_trait::async_trait]
impl SearchProvider for BraveSearchProvider {
    fn name(&self) -> &'static str {
        "brave"
    }

    async fn search(
        &self,
        _ctx: &AuthContext,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let q = validate_query(query)?;
        let resp = self
            .http
            .get(&self.endpoint)
            .query(&[("q", q), ("count", &max_results.to_string())])
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| SearchError::Unavailable(format!("brave request: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchError::Unavailable(format!(
                "brave status: {}",
                resp.status()
            )));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| SearchError::Unavailable(format!("brave body: {e}")))?;
        parse_brave(&body, max_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_brave_fixture() {
        let body = r#"{
            "web": { "results": [
                {"title": "Rust", "url": "https://www.rust-lang.org/", "description": "A language"},
                {"title": "NoUrl", "url": "", "description": "dropped"},
                {"title": "Second", "url": "https://example.com/", "description": "desc"}
            ]}
        }"#;
        let hits = parse_brave(body, 8).unwrap();
        assert_eq!(hits.len(), 2, "url 空は落とす");
        assert_eq!(hits[0].title, "Rust");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        // max_results で打ち切る。
        assert_eq!(parse_brave(body, 1).unwrap().len(), 1);
    }

    #[test]
    fn parses_empty_and_rejects_garbage() {
        assert!(parse_brave("{}", 8).unwrap().is_empty());
        assert!(matches!(
            parse_brave("not json", 8),
            Err(SearchError::Unavailable(_))
        ));
    }
}
