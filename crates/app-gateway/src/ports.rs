//! 重量級サブシステムへの narrow port（Task 9.8）。
//!
//! rag.query の委譲先は `rag::SearchService`（permission-aware・pre/post-filter 二段 authz）
//! だが、app-gateway が qdrant/tantivy/埋め込みまで依存するのは過剰なため、必要最小の
//! trait（[`RagPort`]）だけを定義し、実装は api 層の配線が `SearchService` を包んで提供する。
//! **可読性の担保はこの trait の実装側（SearchService の post-filter）が持つ**——ゲートウェイは
//! 呼出ユーザーの [`AuthContext`] をそのまま渡すだけで、検索結果に非可読文書は混入しない。

use async_trait::async_trait;
use authz::AuthContext;
use serde::Serialize;
use uuid::Uuid;

use crate::GatewayError;

/// permission-aware RAG 検索の 1 ヒット（ミニアプリへ返す最小 DTO）。
#[derive(Debug, Clone, Serialize)]
pub struct RagHit {
    pub chunk_id: Uuid,
    pub file_id: Uuid,
    pub file_name: String,
    pub page: Option<i32>,
    pub heading_path: Vec<String>,
    pub content: String,
    pub score: f32,
}

/// permission-aware RAG 検索の port（実装は api 配線の `SearchService` ラッパ）。
#[async_trait]
pub trait RagPort: Send + Sync {
    /// 呼出ユーザーの ReBAC で検索する（非可読文書は実装側 post-filter が落とす）。
    async fn query(
        &self,
        ctx: &AuthContext,
        query: &str,
        top_k: Option<u32>,
        trace_id: Option<&str>,
    ) -> Result<Vec<RagHit>, GatewayError>;
}

/// RAG 未構成時のフォールバック（rag.query は 502 を返す）。
pub struct NoRag;

#[async_trait]
impl RagPort for NoRag {
    async fn query(
        &self,
        _ctx: &AuthContext,
        _query: &str,
        _top_k: Option<u32>,
        _trace_id: Option<&str>,
    ) -> Result<Vec<RagHit>, GatewayError> {
        Err(GatewayError::Upstream(
            "RAG 検索がこの環境では構成されていません".into(),
        ))
    }
}
