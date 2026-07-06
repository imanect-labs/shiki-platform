//! `web_search` ツール（Phase 4 web ツール）。公開 web を検索し、上位ヒットをモデルへ返す。
//!
//! 検索は **shiki-server ホスト側**で [`SearchProvider`] を直接呼ぶ（サンドボックス非経由・
//! design §4.4）。結果 URL の本文が要るときはモデルが `web_fetch` を続けて呼ぶ。
//! 社内文書の検索（権限考慮）は `doc_search` が担い、こちらは公開 web 専用。

use std::sync::Arc;

use authz::AuthContext;
use websearch::SearchProvider;

use crate::tool::{Tool, ToolError, ToolOutcome};

/// 1 回の検索で返す既定の上限。
const DEFAULT_MAX_RESULTS: usize = 8;

/// `web_search` ツール。プロバイダ（Brave/SearXNG/Stub）はトレイト裏で差し替える。
pub struct WebSearchTool {
    provider: Arc<dyn SearchProvider>,
    max_results: usize,
}

impl WebSearchTool {
    pub fn new(provider: Arc<dyn SearchProvider>) -> Self {
        WebSearchTool {
            provider,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "web_search"
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "公開 web を検索し、タイトル・URL・スニペットを返す。最新情報や社外の情報が必要なときに使う。\
         ページ本文が必要なら結果の URL を web_fetch で取得する。社内文書は doc_search を使うこと。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "検索クエリ（自然文/キーワード）" }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    // 読み取りのみ（ホスト側から検索 API を叩くだけ）。確認不要。
    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let query = input
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Invalid("missing 'query'".into()))?;
        let hits = self
            .provider
            .search(ctx, query, self.max_results)
            .await
            .map_err(|e| match e {
                websearch::SearchError::Invalid(m) => ToolError::Invalid(m),
                websearch::SearchError::Unavailable(m) => ToolError::Unavailable(m),
            })?;
        if hits.is_empty() {
            return Ok(ToolOutcome::ok("検索結果はありませんでした。"));
        }
        use std::fmt::Write as _;
        let mut s = format!("web 検索結果 {} 件:\n", hits.len());
        for (i, h) in hits.iter().enumerate() {
            let _ = write!(s, "[{}] {}\n{}\n{}\n\n", i + 1, h.title, h.url, h.snippet);
        }
        Ok(ToolOutcome::ok(s))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use websearch::StubSearchProvider;

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "org1".into(),
            "t1".into(),
        )
    }

    #[tokio::test]
    async fn formats_hits_with_urls() {
        let tool = WebSearchTool::new(Arc::new(StubSearchProvider::new()));
        let out = tool
            .call(&ctx(), serde_json::json!({"query": "rust"}), None)
            .await
            .expect("ok");
        assert!(!out.is_error);
        assert!(out.content.contains("rust"));
        assert!(out.content.contains("https://example.com/stub-1"));
    }

    #[tokio::test]
    async fn missing_query_is_invalid() {
        let tool = WebSearchTool::new(Arc::new(StubSearchProvider::new()));
        let err = tool
            .call(&ctx(), serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }

    #[tokio::test]
    async fn empty_query_is_invalid() {
        let tool = WebSearchTool::new(Arc::new(StubSearchProvider::new()));
        let err = tool
            .call(&ctx(), serde_json::json!({"query": "  "}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }
}
