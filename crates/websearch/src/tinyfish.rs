//! TinyFish Search API プロバイダ（SaaS 向け）。
//!
//! <https://api.search.tinyfish.ai> を叩く（同期・`X-API-Key`）。自己ホストの SearXNG は上流の
//! 検索サイトに CAPTCHA・流量制限で丸ごと弾かれることがあり（プレビューで全エンジン停止・
//! 2026-11-14）、API 型の検索を選べるようにする。応答の解析は純関数 [`parse_tinyfish`] に
//! 分離してフィクスチャでテストする。

use authz::AuthContext;
use serde::Deserialize;

use crate::{validate_query, SearchError, SearchHit, SearchProvider};

/// TinyFish Search API の既定エンドポイント。
const DEFAULT_ENDPOINT: &str = "https://api.search.tinyfish.ai";

/// TinyFish Search API プロバイダ。
pub struct TinyfishSearchProvider {
    http: reqwest::Client,
    api_key: String,
    endpoint: String,
}

impl TinyfishSearchProvider {
    /// `endpoint` が `None` なら既定の公開 API を使う（テストでは差し替える）。
    pub fn new(http: reqwest::Client, api_key: String, endpoint: Option<String>) -> Self {
        TinyfishSearchProvider {
            http,
            api_key,
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
        }
    }
}

// 応答のうち使うフィールドだけを写す。
#[derive(Deserialize)]
struct TinyfishResponse {
    #[serde(default)]
    results: Vec<TinyfishResult>,
}

#[derive(Deserialize)]
struct TinyfishResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    snippet: String,
}

/// TinyFish 応答 JSON → 共通 [`SearchHit`]（純関数・テスト対象）。
fn parse_tinyfish(body: &str, max_results: usize) -> Result<Vec<SearchHit>, SearchError> {
    let resp: TinyfishResponse = serde_json::from_str(body)
        .map_err(|e| SearchError::Unavailable(format!("tinyfish response parse: {e}")))?;
    Ok(resp
        .results
        .into_iter()
        .filter(|r| !r.url.is_empty())
        .take(max_results)
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.snippet,
        })
        .collect())
}

/// 検索語に日本語（かな・漢字）が入っていれば日本の結果を求める。
///
/// TinyFish は `language`/`location` を省くと英語・米国（`en`/`US`）で引くので、日本語の
/// 質問に英語圏の結果が並ぶ。英語の検索語は指定しない（既定のまま）。
fn locale_for(query: &str) -> Option<(&'static str, &'static str)> {
    let japanese = query.chars().any(|c| {
        matches!(c,
            '\u{3040}'..='\u{30FF}'   // ひらがな・カタカナ
            | '\u{3400}'..='\u{4DBF}' // CJK 拡張 A
            | '\u{4E00}'..='\u{9FFF}' // CJK 統合漢字
            | '\u{FF66}'..='\u{FF9F}' // 半角カナ
        )
    });
    japanese.then_some(("ja", "JP"))
}

/// HTTP 状態 → 利用者・エージェントに出す理由（キー・残高の問題は再試行しても直らない）。
fn status_reason(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => format!("tinyfish status: {status}（API キーが無効）"),
        402 => format!("tinyfish status: {status}（無料枠・残高が尽きた）"),
        429 => format!("tinyfish status: {status}（流量制限。時間をおいて再試行）"),
        _ => format!("tinyfish status: {status}"),
    }
}

#[async_trait::async_trait]
impl SearchProvider for TinyfishSearchProvider {
    fn name(&self) -> &'static str {
        "tinyfish"
    }

    async fn search(
        &self,
        _ctx: &AuthContext,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let q = validate_query(query)?;
        let mut params: Vec<(&str, &str)> = vec![("query", q)];
        if let Some((language, location)) = locale_for(q) {
            params.push(("language", language));
            params.push(("location", location));
        }
        let resp = self
            .http
            .get(&self.endpoint)
            .query(&params)
            .header("X-API-Key", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| SearchError::Unavailable(format!("tinyfish request: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchError::Unavailable(status_reason(resp.status())));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| SearchError::Unavailable(format!("tinyfish body: {e}")))?;
        parse_tinyfish(&body, max_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// docs.tinyfish.ai の Search API の応答例（フィールド名をそのまま写す）。
    #[test]
    fn parses_tinyfish_fixture() {
        let body = r#"{
            "query": "web automation tools",
            "results": [
                {"position": 1, "site_name": "tinyfish.ai", "title": "TinyFish",
                 "snippet": "Automate any website", "url": "https://tinyfish.ai"},
                {"position": 2, "site_name": "x", "title": "NoUrl", "snippet": "dropped", "url": ""},
                {"position": 3, "site_name": "github.com", "title": "Tools",
                 "snippet": "A curated list", "url": "https://github.com/example/tools", "date": "2026-01-01"}
            ],
            "total_results": 10,
            "page": 0
        }"#;
        let hits = parse_tinyfish(body, 8).unwrap();
        assert_eq!(hits.len(), 2, "url 空は落とす");
        assert_eq!(hits[0].title, "TinyFish");
        assert_eq!(hits[0].snippet, "Automate any website");
        assert_eq!(hits[1].url, "https://github.com/example/tools");
        assert_eq!(
            parse_tinyfish(body, 1).unwrap().len(),
            1,
            "max_results で打ち切る"
        );
    }

    #[test]
    fn parses_empty_and_rejects_garbage() {
        assert!(parse_tinyfish("{}", 8).unwrap().is_empty());
        assert!(matches!(
            parse_tinyfish("<html>", 8),
            Err(SearchError::Unavailable(_))
        ));
    }

    #[test]
    fn japanese_queries_ask_for_japanese_results() {
        assert_eq!(locale_for("交通事故 対策"), Some(("ja", "JP")));
        assert_eq!(locale_for("ｶﾀｶﾅ"), Some(("ja", "JP")));
        assert_eq!(locale_for("さくら"), Some(("ja", "JP")));
        assert_eq!(locale_for("traffic safety 2025"), None);
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "org1".into(),
            authz::TenantId::parse("t1").unwrap(),
        )
    }

    /// 1 回だけ応答する HTTP サーバ。受け取ったリクエストの先頭（リクエスト行とヘッダ）を返す。
    async fn one_shot(
        status: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (url, handle)
    }

    #[tokio::test]
    async fn sends_key_and_japanese_locale_and_parses_hits() {
        let (url, req) = one_shot(
            "200 OK",
            r#"{"results":[{"title":"警察庁","snippet":"交通安全","url":"https://www.npa.go.jp/"}]}"#,
        )
        .await;
        let p = TinyfishSearchProvider::new(reqwest::Client::new(), "k-123".into(), Some(url));
        let hits = p.search(&ctx(), "交通事故 対策", 8).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://www.npa.go.jp/");
        let req = req.await.unwrap().to_ascii_lowercase();
        assert!(req.contains("x-api-key: k-123"), "{req}");
        assert!(
            req.contains("language=ja") && req.contains("location=jp"),
            "{req}"
        );
        assert!(req.starts_with("get /?query="), "{req}");
    }

    #[tokio::test]
    async fn quota_exhaustion_is_unavailable_with_reason() {
        let (url, _req) = one_shot(
            "402 Payment Required",
            r#"{"error":"FREE_ALLOWANCE_EXHAUSTED"}"#,
        )
        .await;
        let p = TinyfishSearchProvider::new(reqwest::Client::new(), "k".into(), Some(url));
        let Err(SearchError::Unavailable(m)) = p.search(&ctx(), "rust", 8).await else {
            panic!("402 は Unavailable");
        };
        assert!(m.contains("無料枠"), "{m}");
        assert_eq!(p.name(), "tinyfish");
    }

    #[test]
    fn status_reasons_tell_key_and_quota_problems_apart() {
        use reqwest::StatusCode;
        assert!(status_reason(StatusCode::UNAUTHORIZED).contains("API キー"));
        assert!(status_reason(StatusCode::PAYMENT_REQUIRED).contains("無料枠"));
        assert!(status_reason(StatusCode::TOO_MANY_REQUESTS).contains("流量制限"));
        assert_eq!(
            status_reason(StatusCode::BAD_GATEWAY),
            "tinyfish status: 502 Bad Gateway"
        );
    }
}
