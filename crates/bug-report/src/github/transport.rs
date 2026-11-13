use super::{Dispatch, GitHubIssueSink};
use std::time::Duration;
impl GitHubIssueSink {
    pub(super) async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, reqwest::header::HeaderMap, Vec<u8>), Dispatch> {
        // DNS の解決から body 読取までを一つの期限で囲む。
        tokio::time::timeout(
            Duration::from_secs(15),
            self.request_inner(method, path, token, body),
        )
        .await
        .unwrap_or(Err(Dispatch::Unknown))
    }
    async fn request_inner(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, reqwest::header::HeaderMap, Vec<u8>), Dispatch> {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(12))
            .user_agent("shiki-bug-report");
        #[allow(unused_mut)]
        let mut base = "https://api.github.com".to_string();
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            base.clone_from(stub);
        }
        if base == "https://api.github.com" {
            let addrs = tokio::time::timeout(
                Duration::from_secs(3),
                sandbox_client::net_guard::resolve_public_addrs("api.github.com", 443),
            )
            .await
            .map_err(|_| Dispatch::Retry {
                after: 0,
                code: "connect",
            })?
            .map_err(|_| Dispatch::Retry {
                after: 0,
                code: "connect",
            })?;
            builder = builder
                .https_only(true)
                .resolve_to_addrs("api.github.com", &addrs);
        }
        let client = builder.build().map_err(|_| Dispatch::Failed("client"))?;
        let mut request = client
            .request(method, format!("{base}{path}"))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request.send().await.map_err(|e| {
            if e.is_connect() {
                Dispatch::Retry {
                    after: 0,
                    code: "connect",
                }
            } else {
                Dispatch::Unknown
            }
        })?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Dispatch::Unknown)? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(Dispatch::Unknown);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, headers, bytes))
    }
}
