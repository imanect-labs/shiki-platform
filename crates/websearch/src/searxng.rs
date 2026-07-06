//! SearXNG プロバイダ（オンプレ/自己ホスト向け）。
//!
//! compose で立てた SearXNG の `/search?format=json` を叩く。応答の解析は純関数
//! [`parse_searxng`] に分離してフィクスチャでテストする。

use authz::AuthContext;
use serde::Deserialize;

use crate::{validate_query, SearchError, SearchHit, SearchProvider};

/// SearXNG プロバイダ。
pub struct SearxngSearchProvider {
    http: reqwest::Client,
    /// SearXNG のベース URL（例 `http://searxng:8080`）。
    base_url: String,
}

impl SearxngSearchProvider {
    pub fn new(http: reqwest::Client, base_url: &str) -> Self {
        SearxngSearchProvider {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[derive(Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
struct SearxngResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

/// SearXNG 応答 JSON → 共通 [`SearchHit`]（純関数・テスト対象）。
fn parse_searxng(body: &str, max_results: usize) -> Result<Vec<SearchHit>, SearchError> {
    let resp: SearxngResponse = serde_json::from_str(body)
        .map_err(|e| SearchError::Unavailable(format!("searxng response parse: {e}")))?;
    Ok(resp
        .results
        .into_iter()
        .filter(|r| !r.url.is_empty())
        .take(max_results)
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.content,
        })
        .collect())
}

#[async_trait::async_trait]
impl SearchProvider for SearxngSearchProvider {
    fn name(&self) -> &'static str {
        "searxng"
    }

    async fn search(
        &self,
        _ctx: &AuthContext,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let q = validate_query(query)?;
        let url = format!("{}/search", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("q", q), ("format", "json")])
            .send()
            .await
            .map_err(|e| SearchError::Unavailable(format!("searxng request: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchError::Unavailable(format!(
                "searxng status: {}",
                resp.status()
            )));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| SearchError::Unavailable(format!("searxng body: {e}")))?;
        parse_searxng(&body, max_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_searxng_fixture() {
        let body = r#"{
            "results": [
                {"title": "Doc", "url": "https://docs.searxng.org/", "content": "meta search"},
                {"title": "NoUrl", "url": "", "content": "dropped"}
            ]
        }"#;
        let hits = parse_searxng(body, 8).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].snippet, "meta search");
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            parse_searxng("<html>", 8),
            Err(SearchError::Unavailable(_))
        ));
    }
}
