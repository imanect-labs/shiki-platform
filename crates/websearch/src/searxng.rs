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
    /// 応答しなかった上流エンジンと理由（`[["brave", "too many requests"], ...]`）。
    #[serde(default)]
    unresponsive_engines: Vec<(String, String)>,
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

/// 応答しなかったエンジンを、エラーに何件まで書くか（ツール結果を埋めないように）。
const MAX_REPORTED_ENGINES: usize = 6;

/// SearXNG 応答 JSON → 共通 [`SearchHit`]（純関数・テスト対象）。
fn parse_searxng(body: &str, max_results: usize) -> Result<Vec<SearchHit>, SearchError> {
    let resp: SearxngResponse = serde_json::from_str(body)
        .map_err(|e| SearchError::Unavailable(format!("searxng response parse: {e}")))?;
    // **結果 0 件で上流が止まっているなら「該当なし」ではない。** 自己ホストの SearXNG は
    // 上流（brave / duckduckgo / google 等）に CAPTCHA や流量制限で止められることがあり、
    // そのまま 0 件を返すとエージェントは「見つからない」と受け取って言い換え検索を繰り返し、
    // 最後に「Web には情報が無かった」と誤って答える（プレビューで実際に起きた・2026-11-14）。
    if resp.results.iter().all(|r| r.url.is_empty()) && !resp.unresponsive_engines.is_empty() {
        let engines: Vec<String> = resp
            .unresponsive_engines
            .iter()
            .take(MAX_REPORTED_ENGINES)
            .map(|(name, why)| format!("{name}: {why}"))
            .collect();
        return Err(SearchError::Unavailable(format!(
            "web 検索の上流エンジンが一時的に応答していない（{}）。検索語を変えても結果は出ない。\
             時間をおいて再試行するか、分かっている公式サイトを web_fetch で直接開く",
            engines.join(" / ")
        )));
    }
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
#[allow(clippy::panic)]
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
    fn empty_results_with_blocked_engines_is_unavailable_not_no_hits() {
        let body = r#"{
            "results": [],
            "unresponsive_engines": [["brave", "too many requests"], ["duckduckgo", "CAPTCHA"]]
        }"#;
        let Err(SearchError::Unavailable(m)) = parse_searxng(body, 8) else {
            panic!("上流が止まっているときは Unavailable");
        };
        assert!(m.contains("brave: too many requests"), "{m}");
        assert!(m.contains("duckduckgo: CAPTCHA"), "{m}");
    }

    #[test]
    fn hits_win_over_partially_blocked_engines_and_plain_empty_is_no_hits() {
        let body = r#"{
            "results": [{"title": "T", "url": "https://example.com/", "content": "c"}],
            "unresponsive_engines": [["brave", "too many requests"]]
        }"#;
        assert_eq!(parse_searxng(body, 8).unwrap().len(), 1);
        // 上流が全部応えて 0 件なら、本当に「該当なし」。
        assert!(parse_searxng(r#"{"results": []}"#, 8).unwrap().is_empty());
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            parse_searxng("<html>", 8),
            Err(SearchError::Unavailable(_))
        ));
    }
}
