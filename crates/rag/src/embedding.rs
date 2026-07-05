//! `EmbeddingProvider` トレイト＋worker `/embed` 実装（Task 2.3）。
//!
//! 差し替え点（docs/design.md §3.1）。既定は ingestion-worker（Ruri v3, CPU）で、
//! TEI / 外部 API へは本トレイトの別実装で差し替える。モデル固有のプレフィックス
//! （`検索クエリ: ` 等）は worker 側に閉じ、Rust は [`EmbedInput`] の区別だけ渡す。

use async_trait::async_trait;
use authz::AuthContext;
use serde::Deserialize;

use crate::error::RagError;
use crate::parser_http::map_worker_error;

/// Ruri v3 系の非対称埋め込み（クエリ/文書でプレフィックスが異なる）の区別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedInput {
    Query,
    Document,
}

impl EmbedInput {
    fn as_str(self) -> &'static str {
        match self {
            EmbedInput::Query => "query",
            EmbedInput::Document => "document",
        }
    }
}

/// 埋め込み応答。`model_version` は実際に推論したモデル（突合ガードの根拠）。
#[derive(Debug, Clone, Deserialize)]
pub struct EmbedResponse {
    pub vectors: Vec<Vec<f32>>,
    pub model_version: String,
    pub dimension: usize,
}

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// テキスト群をバッチ埋め込みする。応答ベクトルは L2 正規化済み（cosine 用）。
    async fn embed(
        &self,
        ctx: &AuthContext,
        input: EmbedInput,
        texts: &[String],
    ) -> Result<EmbedResponse, RagError>;

    /// 設定上の期待モデル版。インデックス（collection/index）単位で固定される（PIT-8）。
    fn model_version(&self) -> &str;
}

/// ingestion-worker `/embed` を呼ぶ実装。
///
/// **版突合ガード**: worker が返した `model_version` が設定と食い違う場合はエラー。
/// 版の混在したベクタがインデックスへ入る事故を、書込より前の地点で構造的に防ぐ。
pub struct HttpEmbeddingProvider {
    http: reqwest::Client,
    base_url: String,
    expected_model_version: String,
    /// 1 リクエストに載せる最大テキスト数（worker の DTO 上限と対）。
    batch_size: usize,
}

impl HttpEmbeddingProvider {
    pub fn new(http: reqwest::Client, base_url: &str, expected_model_version: &str) -> Self {
        HttpEmbeddingProvider {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            expected_model_version: expected_model_version.to_string(),
            batch_size: 64,
        }
    }
}

#[async_trait]
impl EmbeddingProvider for HttpEmbeddingProvider {
    async fn embed(
        &self,
        ctx: &AuthContext,
        input: EmbedInput,
        texts: &[String],
    ) -> Result<EmbedResponse, RagError> {
        let mut vectors = Vec::with_capacity(texts.len());
        let mut model_version = self.expected_model_version.clone();
        let mut dimension = 0usize;
        for batch in texts.chunks(self.batch_size.max(1)) {
            let resp = self
                .http
                .post(format!("{}/embed", self.base_url))
                .json(&serde_json::json!({
                    "tenant_id": ctx.tenant_id,
                    "input_type": input.as_str(),
                    "texts": batch,
                }))
                .send()
                .await?;
            if !resp.status().is_success() {
                return Err(map_worker_error(resp).await);
            }
            let body: EmbedResponse = resp.json().await?;
            // バッチ単位で件数を突合する（総数チェックだけでは複数バッチ間の過不足が
            // 相殺され、テキスト↔ベクトルの対応がずれたまま保存され得る）。
            if body.vectors.len() != batch.len() {
                return Err(RagError::Worker(format!(
                    "埋め込み応答数の不一致: 期待 {} 実際 {}",
                    batch.len(),
                    body.vectors.len()
                )));
            }
            if body.model_version != self.expected_model_version {
                return Err(RagError::EmbeddingVersionMismatch {
                    expected: self.expected_model_version.clone(),
                    actual: body.model_version,
                });
            }
            model_version = body.model_version;
            dimension = body.dimension;
            vectors.extend(body.vectors);
        }
        Ok(EmbedResponse {
            vectors,
            model_version,
            dimension,
        })
    }

    fn model_version(&self) -> &str {
        &self.expected_model_version
    }
}
