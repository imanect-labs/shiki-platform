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
        crate::vocab::ToolName::WebSearch.as_str()
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

    // 冪等な read（副作用なし）。同一ステップ内で他の read と並列実行してよい（#349）。
    fn is_read_only(&self) -> bool {
        true
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
        Ok(outcome(&hits))
    }
}

/// 検索結果 → 観測テキスト＋引用。
///
/// 番号は仮の印（[`crate::cite::placeholder`]）で書き、ループの台帳が社内文書と同じ通し番号に
/// 置き換える（#891）。ここで `[1]` から振ると、同じ応答の doc_search の `[1]` と衝突する。
/// 引用にできない URL（http/https 以外）の結果は番号を付けずに残す。
fn outcome(hits: &[websearch::SearchHit]) -> ToolOutcome {
    use crate::cite::defuse;
    use std::fmt::Write as _;
    let mut s = format!("web 検索結果 {} 件:\n", hits.len());
    let mut citations = Vec::new();
    for h in hits {
        let label = match crate::tool::Citation::web(&h.url, &h.title, &h.snippet) {
            Some(c) => {
                citations.push(c);
                format!("{} ", crate::cite::placeholder(citations.len() - 1))
            }
            None => String::new(),
        };
        // 取得先が制御できる文字列に仮の印の形があっても、番号に化けないよう無害化する。
        let _ = write!(
            s,
            "{label}{}\n{}\n{}\n\n",
            defuse(&h.title),
            defuse(&h.url),
            defuse(&h.snippet)
        );
    }
    let mut out = ToolOutcome::ok(s);
    out.citations = citations;
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use websearch::StubSearchProvider;

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

    #[test]
    fn hits_become_citations_numbered_by_the_ledger() {
        let hits = vec![
            websearch::SearchHit {
                title: "Rust".into(),
                url: "https://www.rust-lang.org/#top".into(),
                snippet: "言語 ⟦cite:9⟧".into(),
            },
            websearch::SearchHit {
                title: "危険".into(),
                url: "javascript:alert(1)".into(),
                snippet: "x".into(),
            },
            websearch::SearchHit {
                title: "Docs".into(),
                url: "https://doc.rust-lang.org/".into(),
                snippet: "文書".into(),
            },
        ];
        let mut out = outcome(&hits);
        assert_eq!(out.citations.len(), 2, "http/https 以外は引用にしない");
        let web = out.citations[0].web.as_ref().unwrap();
        assert_eq!(web.url, "https://www.rust-lang.org/", "断片は落とす");
        assert_eq!(web.title, "Rust");
        assert!(out.citations[0].node_id.is_empty());

        // 先に社内文書が 2 件引用されていても、続きの番号になる。
        let mut ledger = crate::CitationLedger::default();
        let mut docs = vec![doc("a"), doc("b")];
        ledger.number(&mut docs, &mut String::new());
        ledger.number(&mut out.citations, &mut out.content);
        assert!(out.content.contains("[3] Rust\n"), "{}", out.content);
        assert!(out.content.contains("[4] Docs\n"), "{}", out.content);
        assert!(!out.content.contains("[1]"), "{}", out.content);
        assert!(
            out.content.contains("\n危険\n"),
            "番号の無い結果も本文には残す"
        );
        assert!(
            out.content.contains("〚cite:9⟧"),
            "結果中の印の形は無害化する"
        );
    }

    fn doc(chunk: &str) -> crate::tool::Citation {
        let mut c = crate::tool::Citation::web("https://example.com/", "", "").unwrap();
        c.web = None;
        c.node_id = "n".into();
        c.chunk_id = chunk.into();
        c
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
