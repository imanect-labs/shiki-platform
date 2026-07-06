//! 決定的スタブプロバイダ（テスト/CI/エアギャップ用・外部依存なし）。

use authz::AuthContext;

use crate::{validate_query, SearchError, SearchHit, SearchProvider};

/// 決定的なスタブ。クエリを織り込んだ固定ヒットを返す（agent ループ/E2E の検証用）。
#[derive(Default)]
pub struct StubSearchProvider;

impl StubSearchProvider {
    pub fn new() -> Self {
        StubSearchProvider
    }
}

#[async_trait::async_trait]
impl SearchProvider for StubSearchProvider {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn search(
        &self,
        _ctx: &AuthContext,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let q = validate_query(query)?;
        let hits = vec![
            SearchHit {
                title: format!("スタブ結果 1: {q}"),
                url: "https://example.com/stub-1".to_string(),
                snippet: format!("「{q}」に関する決定的なテスト用スニペット（1 件目）。"),
            },
            SearchHit {
                title: format!("スタブ結果 2: {q}"),
                url: "https://example.com/stub-2".to_string(),
                snippet: format!("「{q}」に関する決定的なテスト用スニペット（2 件目）。"),
            },
        ];
        Ok(hits.into_iter().take(max_results).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use authz::Principal;

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
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
    async fn returns_deterministic_hits() {
        let p = StubSearchProvider::new();
        let hits = p.search(&ctx(), "rust", 8).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits[0].title.contains("rust"));
        // max_results で打ち切れる。
        assert_eq!(p.search(&ctx(), "rust", 1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejects_empty_query() {
        let p = StubSearchProvider::new();
        assert!(matches!(
            p.search(&ctx(), "  ", 8).await,
            Err(SearchError::Invalid(_))
        ));
    }
}
