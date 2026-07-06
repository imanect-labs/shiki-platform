//! `doc_search` ツール（Task 3.4）＋古典 RAG 注入の共有ヘルパ。
//!
//! Phase 2 の permission-aware 検索（`rag::SearchService`）を**呼び出し時のユーザー権限で**実行する。
//! 二段 authz（pre-filter＋post-filter・file 粒度 HigherConsistency）と引用監査は `SearchService`
//! 内で走るため、ここは `&AuthContext` を素通しするだけで confused-deputy を避けられる。
//!
//! - **エージェントモード**: [`DocSearchTool`]（LLM が自律的に呼ぶ）。
//! - **通常チャット（OFF）**: chat ドメインが [`run_doc_search`] を事前に直接呼び、文脈注入する。

use std::sync::Arc;

use authz::AuthContext;
use rag::{SearchMode, SearchResult, SearchService};

use crate::tool::{Citation, Tool, ToolError, ToolOutcome};

/// doc_search の実行結果（引用＋モデル/注入用テキスト）。
#[derive(Debug, Clone, PartialEq)]
pub struct DocSearchResult {
    /// UI/監査へ流す引用チャンク。
    pub citations: Vec<Citation>,
    /// モデルが読む観測テキスト（tool_result content）／古典注入の文脈本文。
    pub context_text: String,
}

/// 検索結果 → Citation。
fn to_citation(r: &SearchResult) -> Citation {
    Citation {
        node_id: r.file_id.to_string(),
        chunk_id: r.chunk_id.to_string(),
        snippet: r.content.clone(),
        page: r.page,
        heading_path: r.heading_path.clone(),
        score: r.score,
    }
}

/// permission-aware 検索を呼び出しユーザーの権限で実行し、引用＋文脈テキストへ写す。
///
/// エージェントの doc_search ツールと通常チャットの古典 RAG 注入の**単一実装**。
pub async fn run_doc_search(
    search: &SearchService,
    ctx: &AuthContext,
    query: &str,
    top_k: Option<u32>,
    trace_id: Option<&str>,
) -> Result<DocSearchResult, ToolError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(ToolError::Invalid("query is empty".into()));
    }
    let out = search
        .search(ctx, query, top_k, SearchMode::Hybrid, trace_id)
        .await
        .map_err(|e| ToolError::Unavailable(format!("doc_search failed: {e}")))?;

    let citations: Vec<Citation> = out.results.iter().map(to_citation).collect();
    let context_text = if out.results.is_empty() {
        "検索結果はありませんでした（権限内に該当文書なし）。".to_string()
    } else {
        use std::fmt::Write as _;
        let mut s = format!("検索結果 {} 件:\n", out.results.len());
        for (i, r) in out.results.iter().enumerate() {
            let heading = if r.heading_path.is_empty() {
                String::new()
            } else {
                format!(" / {}", r.heading_path.join(" > "))
            };
            let _ = write!(
                s,
                "[{}] 出典: {}{}\n{}\n\n",
                i + 1,
                r.file_name,
                heading,
                r.content.trim()
            );
        }
        s
    };
    Ok(DocSearchResult {
        citations,
        context_text,
    })
}

/// `doc_search` ツール（エージェントモード）。呼び出しユーザーの権限で検索する。
pub struct DocSearchTool {
    search: Arc<SearchService>,
    /// 1 回の検索で返す上限（既定 8）。
    top_k: Option<u32>,
}

impl DocSearchTool {
    pub fn new(search: Arc<SearchService>) -> Self {
        DocSearchTool {
            search,
            top_k: None,
        }
    }
}

#[async_trait::async_trait]
impl Tool for DocSearchTool {
    // トレイト定義が `-> &str` のため literal 返しでも &'static 化できない（allow）。
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "doc_search"
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "社内文書を検索し、権限を守った引用チャンクを返す。ユーザーの質問に答えるための根拠が必要なときに使う。"
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

    // doc_search は安全（読み取りのみ・権限を守る）。確認不要。
    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let query = input
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Invalid("missing 'query'".into()))?;
        let result = run_doc_search(&self.search, ctx, query, self.top_k, trace_id).await?;
        Ok(ToolOutcome {
            content: result.context_text,
            citations: result.citations,
            artifacts: Vec::new(),
            is_error: false,
        })
    }
}
