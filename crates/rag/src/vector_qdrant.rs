//! Qdrant REST による [`VectorStore`] 実装（Task 2.4）。
//!
//! 公式 qdrant-client（tonic/prost 一式）は持ち込まず、必要 API（collection/alias/
//! upsert/delete/payload/search）だけの薄い REST クライアントにする。トレイト裏に
//! 閉じているため、gRPC クライアントや pgvector への差し替えはこのファイルの外に
//! 影響しない。
//!
//! - collection 名はモデル版を織り込む（`rag_chunks__<version>`）。検索・書込は
//!   alias `rag_chunks_active` 経由。モデル更新は shadow collection を再構築して
//!   alias を切り替える（PIT-8・docs/design.md §4.3）。
//! - payload: `tenant_id` / `node_id` / `version` / `authz_tags`。検索は
//!   `tenant_id = ctx.tenant_id` を**この実装内で無条件 AND**（呼び出し側は外せない）。

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use authz::AuthContext;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::RagError;
use crate::vector_store::{ChunkPoint, PreFilter, ScoredChunk, VectorSearch, VectorStore};

/// 検索・書込に使う alias（正本設計 §4.3 の `rag_chunks_active`）。
pub const ACTIVE_ALIAS: &str = "rag_chunks_active";

pub struct QdrantVectorStore {
    http: reqwest::Client,
    base_url: String,
    collection: String,
    ready: AtomicBool,
}

impl QdrantVectorStore {
    pub fn new(http: reqwest::Client, base_url: &str, embedding_model_version: &str) -> Self {
        QdrantVectorStore {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            collection: collection_name(embedding_model_version),
            ready: AtomicBool::new(false),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// 非 2xx を [`RagError::Vector`] に写す。
    async fn ensure_ok(resp: reqwest::Response) -> Result<Value, RagError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json().await.unwrap_or(Value::Null));
        }
        let body = resp.text().await.unwrap_or_default();
        Err(RagError::Vector(format!("qdrant HTTP {status}: {body}")))
    }

    /// tenant 無条件 AND ＋ prefilter ＋ 除外を Qdrant filter JSON に組む。
    fn build_filter(ctx: &AuthContext, prefilter: &PreFilter, exclude: &[Uuid]) -> Value {
        // tenant フィルタは常に must（authz_tags と独立の防壁・design §4.3）。
        let mut must = vec![json!({"key": "tenant_id", "match": {"value": ctx.tenant_id}})];
        if let PreFilter::Tags(tags) = prefilter {
            must.push(json!({"key": "authz_tags", "match": {"any": tags}}));
        }
        let mut filter = json!({"must": must});
        if !exclude.is_empty() {
            let ids: Vec<String> = exclude.iter().map(Uuid::to_string).collect();
            filter["must_not"] = json!([{"has_id": ids}]);
        }
        filter
    }
}

/// モデル版から collection 名を組む（英数以外を `-` に正規化）。
fn collection_name(embedding_model_version: &str) -> String {
    let sanitized: String = embedding_model_version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("rag_chunks__{sanitized}")
}

#[async_trait]
impl VectorStore for QdrantVectorStore {
    async fn ensure_ready(&self, dimension: usize) -> Result<(), RagError> {
        if self.ready.load(Ordering::Acquire) {
            return Ok(());
        }
        // collection 存在確認 → 無ければ作成（並行作成の 409 は成功扱い＝冪等）。
        let exists = self
            .http
            .get(self.url(&format!("/collections/{}/exists", self.collection)))
            .send()
            .await?;
        let exists = Self::ensure_ok(exists).await?["result"]["exists"]
            .as_bool()
            .unwrap_or(false);
        if !exists {
            let resp = self
                .http
                .put(self.url(&format!("/collections/{}", self.collection)))
                .json(&json!({
                    "vectors": {"size": dimension, "distance": "Cosine"}
                }))
                .send()
                .await?;
            if resp.status() != reqwest::StatusCode::CONFLICT {
                Self::ensure_ok(resp).await?;
            }
            // pre-filter の走査経路に payload index を張る。
            for (field, schema) in [
                ("tenant_id", "keyword"),
                ("authz_tags", "keyword"),
                ("node_id", "keyword"),
                ("version", "integer"),
            ] {
                let resp = self
                    .http
                    .put(self.url(&format!("/collections/{}/index", self.collection)))
                    .json(&json!({"field_name": field, "field_schema": schema}))
                    .send()
                    .await?;
                if resp.status() != reqwest::StatusCode::CONFLICT {
                    Self::ensure_ok(resp).await?;
                }
            }
        }
        // alias を自 collection へ向ける（shadow 移行はここを別 collection に切替える）。
        let resp = self
            .http
            .post(self.url("/collections/aliases"))
            .json(&json!({
                "actions": [
                    {"delete_alias": {"alias_name": ACTIVE_ALIAS}},
                    {"create_alias": {
                        "alias_name": ACTIVE_ALIAS,
                        "collection_name": self.collection
                    }}
                ]
            }))
            .send()
            .await?;
        // delete_alias は初回（alias 不在）に 404 相当で失敗するため、その場合は
        // create のみで再試行する。
        if !resp.status().is_success() {
            let resp = self
                .http
                .post(self.url("/collections/aliases"))
                .json(&json!({
                    "actions": [{"create_alias": {
                        "alias_name": ACTIVE_ALIAS,
                        "collection_name": self.collection
                    }}]
                }))
                .send()
                .await?;
            Self::ensure_ok(resp).await?;
        }
        self.ready.store(true, Ordering::Release);
        Ok(())
    }

    async fn upsert(&self, ctx: &AuthContext, points: &[ChunkPoint]) -> Result<(), RagError> {
        if points.is_empty() {
            return Ok(());
        }
        let body_points: Vec<Value> = points
            .iter()
            .map(|p| {
                json!({
                    "id": p.chunk_id.to_string(),
                    "vector": p.vector,
                    "payload": {
                        "tenant_id": ctx.tenant_id,
                        "node_id": p.node_id.to_string(),
                        "version": p.version,
                        "authz_tags": p.authz_tags,
                    }
                })
            })
            .collect();
        let resp = self
            .http
            .put(self.url(&format!("/collections/{ACTIVE_ALIAS}/points?wait=true")))
            .json(&json!({"points": body_points}))
            .send()
            .await?;
        Self::ensure_ok(resp).await?;
        Ok(())
    }

    async fn delete_node(&self, ctx: &AuthContext, node_id: Uuid) -> Result<(), RagError> {
        let resp = self
            .http
            .post(self.url(&format!(
                "/collections/{ACTIVE_ALIAS}/points/delete?wait=true"
            )))
            .json(&json!({"filter": {"must": [
                {"key": "tenant_id", "match": {"value": ctx.tenant_id}},
                {"key": "node_id", "match": {"value": node_id.to_string()}}
            ]}}))
            .send()
            .await?;
        Self::ensure_ok(resp).await?;
        Ok(())
    }

    async fn delete_stale_versions(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        keep_version: i64,
    ) -> Result<(), RagError> {
        let resp = self
            .http
            .post(self.url(&format!(
                "/collections/{ACTIVE_ALIAS}/points/delete?wait=true"
            )))
            .json(&json!({"filter": {
                "must": [
                    {"key": "tenant_id", "match": {"value": ctx.tenant_id}},
                    {"key": "node_id", "match": {"value": node_id.to_string()}}
                ],
                "must_not": [{"key": "version", "match": {"value": keep_version}}]
            }}))
            .send()
            .await?;
        Self::ensure_ok(resp).await?;
        Ok(())
    }

    async fn set_authz_tags(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        tags: &[String],
    ) -> Result<(), RagError> {
        let resp = self
            .http
            .post(self.url(&format!(
                "/collections/{ACTIVE_ALIAS}/points/payload?wait=true"
            )))
            .json(&json!({
                "payload": {"authz_tags": tags},
                "filter": {"must": [
                    {"key": "tenant_id", "match": {"value": ctx.tenant_id}},
                    {"key": "node_id", "match": {"value": node_id.to_string()}}
                ]}
            }))
            .send()
            .await?;
        Self::ensure_ok(resp).await?;
        Ok(())
    }

    async fn search(
        &self,
        ctx: &AuthContext,
        query: &VectorSearch<'_>,
    ) -> Result<Vec<ScoredChunk>, RagError> {
        let resp = self
            .http
            .post(self.url(&format!("/collections/{ACTIVE_ALIAS}/points/search")))
            .json(&json!({
                "vector": query.vector,
                "limit": query.limit,
                "filter": Self::build_filter(ctx, query.prefilter, query.exclude),
                "with_payload": ["node_id"],
            }))
            .send()
            .await?;
        // 未インジェスト（collection/alias 不在）は「ヒット 0」として扱う。
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        let body = Self::ensure_ok(resp).await?;
        let hits = body["result"].as_array().cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(hits.len());
        for hit in hits {
            let chunk_id = hit["id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| RagError::Vector("qdrant 応答の id が不正です".into()))?;
            let node_id = hit["payload"]["node_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| RagError::Vector("qdrant 応答の node_id が不正です".into()))?;
            let score = hit["score"].as_f64().unwrap_or(0.0) as f32;
            out.push(ScoredChunk {
                chunk_id,
                node_id,
                score,
            });
        }
        Ok(out)
    }

    async fn purge_tenant(&self, tenant_id: &str) -> Result<(), RagError> {
        let resp = self
            .http
            .post(self.url(&format!(
                "/collections/{ACTIVE_ALIAS}/points/delete?wait=true"
            )))
            .json(&json!({"filter": {"must": [
                {"key": "tenant_id", "match": {"value": tenant_id}}
            ]}}))
            .send()
            .await?;
        // インデックス自体が未作成なら消すものが無い。
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Self::ensure_ok(resp).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_name_is_sanitized_per_model_version() {
        assert_eq!(
            collection_name("cl-nagoya/ruri-v3-30m"),
            "rag_chunks__cl-nagoya-ruri-v3-30m"
        );
    }

    #[test]
    fn filter_always_contains_tenant_and_optionally_tags() {
        let ctx = authz::AuthContext::new(
            authz::Principal {
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("a-corp".into()),
            },
            "acme".into(),
            "a-corp".into(),
        );
        // TenantOnly 縮退でも tenant must は残る（第二の防壁）。
        let f = QdrantVectorStore::build_filter(&ctx, &PreFilter::TenantOnly, &[]);
        assert_eq!(f["must"][0]["key"], "tenant_id");
        assert_eq!(f["must"][0]["match"]["value"], "a-corp");
        assert_eq!(f["must"].as_array().map(Vec::len), Some(1));

        let f = QdrantVectorStore::build_filter(
            &ctx,
            &PreFilter::Tags(vec!["file:a-corp|f1".into()]),
            &[Uuid::nil()],
        );
        assert_eq!(f["must"][0]["key"], "tenant_id");
        assert_eq!(f["must"][1]["key"], "authz_tags");
        assert_eq!(f["must"][1]["match"]["any"][0], "file:a-corp|f1");
        assert!(f["must_not"][0]["has_id"].is_array());
    }
}
