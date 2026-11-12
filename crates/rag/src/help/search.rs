use super::{
    corpus::{self, invalid, VectorRow},
    index::HelpIndex,
    HelpChunk, HelpConfig, HelpManifest, VectorManifest,
};
use crate::{EmbedInput, EmbeddingProvider, RagError};
use authz::AuthContext;
use serde::Serialize;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

#[derive(Debug, Clone, Serialize)]
pub struct HelpHit {
    pub corpus_version: String,
    pub chunk_id: String,
    pub title: String,
    pub heading_path: Vec<String>,
    pub snippet: String,
    pub url: String,
}
/// 全顧客共通の公開資産。tenant DB / Qdrant / StorageService への参照を持たない。
pub struct HelpSearchService {
    config: HelpConfig,
    manifest: HelpManifest,
    chunks: Vec<HelpChunk>,
    urls: Vec<String>,
    vectors: Vec<Vec<f32>>,
    vector_manifest: VectorManifest,
    index: HelpIndex,
    embedder: Arc<dyn EmbeddingProvider>,
    keyword_fallbacks: AtomicU64,
}
impl HelpSearchService {
    /// 未同梱と無効化は正常。破損した同梱資産は起動エラーにする。
    pub fn load(
        config: HelpConfig,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Result<Option<Self>, RagError> {
        let path = Path::new(&config.data_path);
        if !config.enabled || !path.join("manifest.json").exists() {
            return Ok(None);
        }
        if !(1..=1000).contains(&config.max_query_chars)
            || !(1..=20).contains(&config.max_top_k)
            || !(1..=4000).contains(&config.max_result_chars)
            || config.max_chunks > 20000
            || config.max_corpus_bytes > 256 * 1024 * 1024
        {
            return Err(invalid("limits"));
        }
        let (manifest, chunks) = corpus::load(path, &config)?;
        let base = reqwest::Url::parse(&config.base_url).map_err(|_| invalid("base URL"))?;
        if !matches!(base.scheme(), "https" | "http")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(invalid("base URL must be an origin"));
        }
        let urls = chunks
            .iter()
            .map(|c| {
                base.join(&c.relative_url)
                    .map(|u| u.to_string())
                    .map_err(|_| invalid("URL"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let vm: VectorManifest =
            serde_json::from_slice(&corpus::read(&path.join("vectors.manifest.json"), 65536)?)?;
        if vm.schema_version != 1
            || vm.corpus_version != manifest.corpus_version
            || vm.chunks_sha256 != manifest.chunks_sha256
            || !vm.normalized
            || !(1..=4096).contains(&vm.dimension)
            || vm.query_prefix != "検索クエリ: "
            || vm.document_prefix != "検索文書: "
        {
            return Err(invalid("vector manifest contract"));
        }
        let bytes = corpus::read(&path.join("vectors.jsonl"), config.max_corpus_bytes)?;
        if corpus::hash(&bytes) != vm.vectors_sha256 {
            return Err(invalid("vector hash"));
        }
        let mut rows = HashMap::new();
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let row: VectorRow = serde_json::from_slice(line)?;
            if rows.len() >= config.max_chunks
                || !valid_vector(&row.vector, vm.dimension)
                || rows.insert(row.chunk_id, row.vector).is_some()
            {
                return Err(invalid("vector contract"));
            }
        }
        if rows.len() != chunks.len() {
            return Err(invalid("vector count"));
        }
        let vectors = chunks
            .iter()
            .map(|c| {
                rows.remove(&c.chunk_id)
                    .ok_or_else(|| invalid("vector membership"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let index = HelpIndex::build(&chunks)?;
        let service = Self {
            config,
            manifest,
            chunks,
            urls,
            vectors,
            vector_manifest: vm,
            index,
            embedder,
            keyword_fallbacks: AtomicU64::new(0),
        };
        if service.embedder.model_version() != service.vector_manifest.model_version {
            service.fallback("model_mismatch");
        }
        Ok(Some(service))
    }
    pub fn max_top_k(&self) -> usize {
        self.config.max_top_k
    }
    pub fn max_query_chars(&self) -> usize {
        self.config.max_query_chars
    }

    /// メトリクス収集側から読める累積縮退件数。検索語や tenant 情報は記録しない。
    pub fn keyword_fallbacks(&self) -> u64 {
        self.keyword_fallbacks.load(Ordering::Relaxed)
    }
    fn fallback(&self, reason: &'static str) {
        opentelemetry::global::meter("shiki.help")
            .u64_counter("shiki.help.keyword_fallbacks")
            .build()
            .add(1, &[opentelemetry::KeyValue::new("reason", reason)]);
        self.keyword_fallbacks.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(reason, corpus_version = %self.manifest.corpus_version, "help_search keyword fallback");
    }
    pub async fn search(
        &self,
        ctx: &AuthContext,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<HelpHit>, RagError> {
        if query.trim().is_empty()
            || query.chars().count() > self.config.max_query_chars
            || top_k == 0
            || top_k > self.config.max_top_k
        {
            return Err(invalid("query/top_k limit"));
        }
        let candidates = (top_k * 4).min(80);
        let keyword = self.index.search(query, candidates)?;
        let dense = if self.embedder.model_version() == self.vector_manifest.model_version {
            match self
                .embedder
                .embed(ctx, EmbedInput::Query, &[query.to_string()])
                .await
            {
                Ok(response)
                    if response.model_version == self.vector_manifest.model_version
                        && response.dimension == self.vector_manifest.dimension
                        && response.vectors.len() == 1
                        && valid_vector(&response.vectors[0], self.vector_manifest.dimension) =>
                {
                    let q = &response.vectors[0];
                    let mut scores: Vec<_> = self
                        .vectors
                        .iter()
                        .enumerate()
                        .map(|(i, v)| (i, v.iter().zip(q).map(|(a, b)| a * b).sum::<f32>()))
                        .collect();
                    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                    scores
                        .into_iter()
                        .take(candidates)
                        .map(|(i, _)| i)
                        .collect()
                }
                _ => {
                    self.fallback("embedding_failed");
                    Vec::new()
                }
            }
        } else {
            self.fallback("model_mismatch");
            Vec::new()
        };
        let mut fused = HashMap::<usize, f64>::new();
        for list in [&keyword, &dense] {
            for (rank, i) in list.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let score = 1.0 / (60.0 + rank as f64 + 1.0);
                *fused.entry(*i).or_default() += score;
            }
        }
        let mut ranked: Vec<_> = fused.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(ranked
            .into_iter()
            .take(top_k)
            .map(|(i, _)| {
                let c = &self.chunks[i];
                HelpHit {
                    corpus_version: c.corpus_version.clone(),
                    chunk_id: c.chunk_id.clone(),
                    title: c.title.clone(),
                    heading_path: c.heading_path.clone(),
                    snippet: c.text.chars().take(self.config.max_result_chars).collect(),
                    url: self.urls[i].clone(),
                }
            })
            .collect())
    }
}
fn valid_vector(v: &[f32], dimension: usize) -> bool {
    let norm = v.iter().map(|x| x * x).sum::<f32>();
    v.len() == dimension && v.iter().all(|x| x.is_finite()) && (norm - 1.0).abs() < 0.01
}
