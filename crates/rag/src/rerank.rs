//! `Reranker` トレイト＋worker `/rerank` 実装（Task 2.6 の並べ替え段）。
//!
//! 差し替え点。既定は ingestion-worker（日本語 cross-encoder, CPU）。
//! reranker は**認可済み候補にのみ**適用する（post-filter の後段。PIT-2 対策で
//! 読めないチャンクへ計算を浪費しない）。

use async_trait::async_trait;
use authz::AuthContext;
use serde::Deserialize;

use crate::error::RagError;
use crate::parser_http::map_worker_error;

/// rerank 対象のパッセージ。`id` は chunk_id 文字列（worker は解釈しない）。
pub struct RerankPassage {
    pub id: String,
    pub text: String,
}

/// 1 パッセージの関連度スコア（大きいほど関連）。
#[derive(Debug, Clone, Deserialize)]
pub struct RerankScore {
    pub id: String,
    pub score: f32,
}

#[async_trait]
pub trait Reranker: Send + Sync {
    /// クエリとの関連度で採点する。返り値は入力と同順（並べ替えは呼び出し側）。
    async fn rerank(
        &self,
        ctx: &AuthContext,
        query: &str,
        passages: &[RerankPassage],
    ) -> Result<Vec<RerankScore>, RagError>;
}

/// ingestion-worker `/rerank` を呼ぶ実装。
pub struct HttpReranker {
    http: reqwest::Client,
    base_url: String,
}

impl HttpReranker {
    pub fn new(http: reqwest::Client, base_url: &str) -> Self {
        HttpReranker {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[derive(Deserialize)]
struct RerankResponseBody {
    scores: Vec<RerankScore>,
}

#[async_trait]
impl Reranker for HttpReranker {
    async fn rerank(
        &self,
        ctx: &AuthContext,
        query: &str,
        passages: &[RerankPassage],
    ) -> Result<Vec<RerankScore>, RagError> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "tenant_id": ctx.tenant_id,
            "query": query,
            "passages": passages
                .iter()
                .map(|p| serde_json::json!({"id": p.id, "text": p.text}))
                .collect::<Vec<_>>(),
        });
        let resp = self
            .http
            .post(format!("{}/rerank", self.base_url))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(map_worker_error(resp).await);
        }
        Ok(resp.json::<RerankResponseBody>().await?.scores)
    }
}
