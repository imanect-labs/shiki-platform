//! 結合テスト共通のフェイク実装・ヘルパ（pipeline_it / search_*_it が共有）。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
// 各テストバイナリで一部だけ使うため dead_code も許容する。
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use rag::embedding::{EmbedInput, EmbedResponse, EmbeddingProvider};
use rag::rerank::{RerankPassage, RerankScore, Reranker};
use rag::vector_store::{ChunkPoint, PreFilter, ScoredChunk, VectorSearch, VectorStore};
use rag::RagError;
use storage::{ObjectStore, ObjectStoreError};
use uuid::Uuid;

/// テスト用 AuthContext（tenant 固定）。
pub fn test_ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

/// 決定的フェイク埋め込み（テキストハッシュ → 正規化 8 次元）。
pub struct FakeEmbedder;

#[async_trait]
impl EmbeddingProvider for FakeEmbedder {
    async fn embed(
        &self,
        _ctx: &AuthContext,
        _input: EmbedInput,
        texts: &[String],
    ) -> Result<EmbedResponse, RagError> {
        Ok(EmbedResponse {
            vectors: texts.iter().map(|t| fake_vector(t)).collect(),
            model_version: "fake-model".into(),
            dimension: 8,
        })
    }

    fn model_version(&self) -> &str {
        "fake-model"
    }
}

pub fn fake_vector(text: &str) -> Vec<f32> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut v: Vec<f32> = (0..8u64)
        .map(|i| {
            let mut h = DefaultHasher::new();
            (text, i).hash(&mut h);
            (h.finish() % 1000) as f32 / 1000.0 + 0.001
        })
        .collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in &mut v {
        *x /= norm;
    }
    v
}

/// クエリとの共通文字数で採点する決定的フェイク reranker。
pub struct FakeReranker;

#[async_trait]
impl Reranker for FakeReranker {
    async fn rerank(
        &self,
        _ctx: &AuthContext,
        query: &str,
        passages: &[RerankPassage],
    ) -> Result<Vec<RerankScore>, RagError> {
        let q: HashSet<char> = query.chars().collect();
        Ok(passages
            .iter()
            .map(|p| RerankScore {
                id: p.id.clone(),
                score: p.text.chars().filter(|c| q.contains(c)).count() as f32,
            })
            .collect())
    }
}

/// インメモリ VectorStore（tenant 無条件 AND を含む本物と同じフィルタ意味論）。
#[derive(Default)]
pub struct FakeVectorStore {
    pub points: Mutex<Vec<(String, ChunkPoint)>>, // (tenant_id, point)
}

#[async_trait]
impl VectorStore for FakeVectorStore {
    async fn ensure_ready(&self, _dimension: usize) -> Result<(), RagError> {
        Ok(())
    }
    async fn upsert(&self, ctx: &AuthContext, points: &[ChunkPoint]) -> Result<(), RagError> {
        let mut store = self.points.lock().unwrap();
        for p in points {
            store.retain(|(_, q)| q.chunk_id != p.chunk_id);
            store.push((
                ctx.tenant_id.clone(),
                ChunkPoint {
                    chunk_id: p.chunk_id,
                    node_id: p.node_id,
                    version: p.version,
                    vector: p.vector.clone(),
                    authz_tags: p.authz_tags.clone(),
                },
            ));
        }
        Ok(())
    }
    async fn delete_node(&self, ctx: &AuthContext, node_id: Uuid) -> Result<(), RagError> {
        self.points
            .lock()
            .unwrap()
            .retain(|(t, p)| !(t == &ctx.tenant_id && p.node_id == node_id));
        Ok(())
    }
    async fn delete_stale_versions(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        keep_version: i64,
    ) -> Result<(), RagError> {
        self.points.lock().unwrap().retain(|(t, p)| {
            !(t == &ctx.tenant_id && p.node_id == node_id && p.version != keep_version)
        });
        Ok(())
    }
    async fn set_authz_tags(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        tags: &[String],
    ) -> Result<(), RagError> {
        for (t, p) in self.points.lock().unwrap().iter_mut() {
            if t == &ctx.tenant_id && p.node_id == node_id {
                p.authz_tags = tags.to_vec();
            }
        }
        Ok(())
    }
    async fn search(
        &self,
        ctx: &AuthContext,
        query: &VectorSearch<'_>,
    ) -> Result<Vec<ScoredChunk>, RagError> {
        let store = self.points.lock().unwrap();
        let mut hits: Vec<ScoredChunk> = store
            .iter()
            // tenant 無条件 AND（本物と同じ意味論）。
            .filter(|(t, _)| t == &ctx.tenant_id)
            .filter(|(_, p)| match query.prefilter {
                PreFilter::TenantOnly => true,
                PreFilter::Tags(tags) => p.authz_tags.iter().any(|t| tags.contains(t)),
            })
            .filter(|(_, p)| !query.exclude.contains(&p.chunk_id))
            .map(|(_, p)| ScoredChunk {
                chunk_id: p.chunk_id,
                node_id: p.node_id,
                score: p.vector.iter().zip(query.vector).map(|(a, b)| a * b).sum(),
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(query.limit);
        Ok(hits)
    }
    async fn purge_tenant(&self, tenant_id: &str) -> Result<(), RagError> {
        self.points.lock().unwrap().retain(|(t, _)| t != tenant_id);
        Ok(())
    }
}

/// presign だけ機能するフェイク ObjectStore。
pub struct FakeObjectStore;

#[async_trait]
impl ObjectStore for FakeObjectStore {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(
        &self,
        _key: &str,
        _ttl: Duration,
        _len: i64,
    ) -> Result<String, ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn presign_get(
        &self,
        _key: &str,
        _ttl: Duration,
        _filename: Option<&str>,
        _content_type: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn presign_get_internal(
        &self,
        key: &str,
        _ttl: Duration,
    ) -> Result<String, ObjectStoreError> {
        Ok(format!("http://fake-minio/{key}"))
    }
    async fn read_and_hash(&self, _key: &str) -> Result<(String, u64), ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn put_object(
        &self,
        _key: &str,
        _bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<(), ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn get_object(&self, _key: &str) -> Result<Vec<u8>, ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn exists(&self, _key: &str) -> Result<bool, ObjectStoreError> {
        Ok(true)
    }
    async fn copy(&self, _src: &str, _dst: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _prefix: &str,
        _continuation: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _keys: &[String]) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

/// 台本つきフェイク authz（バックフィル/縮退テスト用）。
///
/// - `list_objects`: 固定の可読タグ集合を返す（型別に振り分け）。
/// - `check`: `denied_files` に載っている file は deny。
pub struct ScriptedAuthz {
    pub readable_folders: Vec<String>,
    pub readable_files: Vec<String>,
    pub denied_files: HashSet<String>,
}

#[async_trait]
impl AuthzClient for ScriptedAuthz {
    async fn check(
        &self,
        _subject: &Subject,
        _relation: Relation,
        object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(!self.denied_files.contains(object.as_str()))
    }
    async fn write_tuple(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        unreachable!("テストでは使わない")
    }
    async fn delete_tuple(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        unreachable!("テストでは使わない")
    }
    async fn read_tuples(
        &self,
        _object: &FgaObject,
        _relation: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        unreachable!("テストでは使わない")
    }
    async fn list_objects(
        &self,
        _subject: &Subject,
        _relation: Relation,
        object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(match object_type {
            ObjectType::Folder => self.readable_folders.clone(),
            ObjectType::File => self.readable_files.clone(),
            _ => vec![],
        })
    }
    async fn delete_object_tuples(&self, _object: &FgaObject) -> Result<u32, AuthzError> {
        unreachable!("テストでは使わない")
    }
    async fn read_subject_objects(
        &self,
        _subject: &Subject,
        _object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        unreachable!("テストでは使わない")
    }
}
