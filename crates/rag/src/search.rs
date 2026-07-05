//! `SearchService` — permission-aware ハイブリッド検索（Task 2.6/2.7/2.10）。
//!
//! 段取り（docs/design.md §4.3）:
//! 1. 可読集合の算出（pre-filter・上限超過で tenant-only 縮退）
//! 2. クエリ埋め込み → dense（Qdrant）/ keyword（Tantivy）並列取得（over-fetch）
//! 3. RRF 融合・重複排除
//! 4. **post-filter（OpenFGA file 粒度・HigherConsistency）を reranker の前に**（PIT-2）
//! 5. 不足時バックフィル（fetch_k 倍増・最大 3 ラウンド・候補が尽きるまで top_k を保証）
//! 6. rerank（認可済み候補の上位 rerank_pool 件のみ）
//! 7. ハイドレーション（node JOIN・`deleted_at is null` 強制・親チャンク展開）
//! 8. 引用監査（chunk_id 群＋file 粒度の認可判定を audit_log へ・trace_id 付き）

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use authz::{AuthContext, AuthzClient};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::authz_filter::{post_filter_by_file, readable_set, PostFilterOutcome};
use crate::config::RagConfig;
use crate::embedding::{EmbedInput, EmbeddingProvider};
use crate::error::RagError;
use crate::fulltext::FulltextIndex;
use crate::fusion::{rrf_fuse, RRF_K};
use crate::rerank::{RerankPassage, Reranker};
use crate::search_types::{SearchDebug, SearchMode, SearchResult, StageTimings};
use crate::vector_store::{PreFilter, ScoredChunk, VectorSearch, VectorStore};

/// バックフィルの上限（PIT-2: 候補が尽きるまで最終件数が top_k を下回らない）。
const MAX_BACKFILL_ROUNDS: u32 = 3;
/// 1 回の取得数（fetch_k）の上限。
const MAX_FETCH_K: usize = 256;

pub struct SearchService {
    pool: PgPool,
    config: RagConfig,
    embedder: Arc<dyn EmbeddingProvider>,
    reranker: Arc<dyn Reranker>,
    vector: Arc<dyn VectorStore>,
    fulltext: Arc<dyn FulltextIndex>,
    authz: Arc<dyn AuthzClient>,
    audit: AuditRecorder,
}

/// 検索の内部出力（API 層が debug の出し分けを行う）。
pub struct SearchOutput {
    pub results: Vec<SearchResult>,
    pub debug: SearchDebug,
}

impl SearchService {
    #[allow(clippy::too_many_arguments)] // 依存束の注入点（AppState からの一回きり）。
    pub fn new(
        pool: PgPool,
        config: RagConfig,
        embedder: Arc<dyn EmbeddingProvider>,
        reranker: Arc<dyn Reranker>,
        vector: Arc<dyn VectorStore>,
        fulltext: Arc<dyn FulltextIndex>,
        authz: Arc<dyn AuthzClient>,
        audit: AuditRecorder,
    ) -> Self {
        SearchService {
            pool,
            config,
            embedder,
            reranker,
            vector,
            fulltext,
            authz,
            audit,
        }
    }

    pub async fn search(
        &self,
        ctx: &AuthContext,
        query: &str,
        top_k: Option<u32>,
        mode: SearchMode,
        trace_id: Option<&str>,
    ) -> Result<SearchOutput, RagError> {
        let top_k = (top_k.unwrap_or(self.config.default_top_k as u32) as usize)
            .clamp(1, self.config.max_top_k);
        let mut debug = SearchDebug::default();
        let mut timings = StageTimings::default();

        // 1. 可読集合（pre-filter）。クエリごとに算出＝grant 即時反映（PIT-3）。
        let t = Instant::now();
        let readable =
            readable_set(ctx, self.authz.as_ref(), self.config.readable_tags_max).await?;
        timings.readable_set_ms = t.elapsed().as_millis() as u64;
        debug.readable_tags = readable.tags.len() as u32;
        let (prefilter, over_fetch) = if readable.overflowed {
            debug.prefilter_mode = "tenant_only".into();
            (PreFilter::TenantOnly, self.config.over_fetch_tenant_only)
        } else {
            debug.prefilter_mode = "tags".into();
            (PreFilter::Tags(readable.tags), self.config.over_fetch_tags)
        };

        // 2. クエリ埋め込み（keyword 単独モードでは不要）。
        let t = Instant::now();
        let query_vector = if mode == SearchMode::Keyword {
            None
        } else {
            let resp = self
                .embedder
                .embed(ctx, EmbedInput::Query, &[query.to_string()])
                .await?;
            resp.vectors.into_iter().next()
        };
        timings.embed_ms = t.elapsed().as_millis() as u64;

        // 3〜5. 取得 → RRF → post-filter → バックフィル。
        let pool_target = top_k.max(self.config.rerank_pool);
        let mut fetch_k = (pool_target * over_fetch.max(1)).min(MAX_FETCH_K);
        let mut allowed: Vec<ScoredChunk> = Vec::new();
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut file_decisions: HashMap<Uuid, bool> = HashMap::new();
        let t_retrieve = Instant::now();
        let mut post_filter_ms = 0u64;
        loop {
            debug.backfill_rounds += 1;
            let exclude: Vec<Uuid> = seen.iter().copied().collect();
            let (dense, keyword) = self
                .retrieve(
                    ctx,
                    query,
                    query_vector.as_deref(),
                    mode,
                    fetch_k,
                    &prefilter,
                    &exclude,
                )
                .await?;
            debug.dense_hits += dense.len() as u32;
            debug.keyword_hits += keyword.len() as u32;
            // 両系統とも fetch_k 未満 = 候補が尽きた（これ以上のバックフィルは無意味）。
            let exhausted = dense.len() < fetch_k && keyword.len() < fetch_k;

            let fused: Vec<ScoredChunk> = rrf_fuse(&[&dense, &keyword], RRF_K)
                .into_iter()
                .filter(|c| seen.insert(c.chunk_id))
                .collect();
            debug.fused += fused.len() as u32;

            let t_pf = Instant::now();
            let PostFilterOutcome {
                allowed: round_allowed,
                denied_chunks,
                denied_files,
                file_decisions: decisions,
            } = post_filter_by_file(ctx, self.authz.as_ref(), fused).await?;
            post_filter_ms += t_pf.elapsed().as_millis() as u64;
            debug.authz_denied_chunks += denied_chunks as u32;
            debug.authz_denied_files += denied_files as u32;
            file_decisions.extend(decisions);
            allowed.extend(round_allowed);

            if allowed.len() >= pool_target
                || exhausted
                || debug.backfill_rounds >= MAX_BACKFILL_ROUNDS
            {
                break;
            }
            fetch_k = (fetch_k * 2).min(MAX_FETCH_K);
        }
        timings.retrieve_ms = t_retrieve.elapsed().as_millis() as u64 - post_filter_ms;
        timings.post_filter_ms = post_filter_ms;

        // 融合スコア順で rerank プールへ。
        allowed.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        allowed.truncate(self.config.rerank_pool.max(top_k));

        // 7(前半). ハイドレーション（rerank は本文が必要なので先に読む）。
        let t = Instant::now();
        let rows = self.hydrate(ctx, &allowed).await?;
        timings.hydrate_ms = t.elapsed().as_millis() as u64;

        // 6. rerank（認可済み・生存チャンクのみ）。
        let t = Instant::now();
        let ranked = self.rerank(ctx, query, &allowed, &rows).await?;
        debug.reranked = ranked.len() as u32;
        timings.rerank_ms = t.elapsed().as_millis() as u64;

        // 7(後半). 上位 top_k を確定し、親チャンクを展開する。
        let final_ids: Vec<Uuid> = ranked.into_iter().take(top_k).collect();
        let results = self.build_results(ctx, &final_ids, &rows).await?;

        // 8. 引用監査（Task 2.7 受入条件: 引用 chunk と認可判定が監査ログに残る）。
        self.audit_citations(ctx, query, &results, &file_decisions, trace_id)
            .await?;

        debug.stage_ms = timings;
        Ok(SearchOutput { results, debug })
    }

    /// dense / keyword を並列取得する。
    #[allow(clippy::too_many_arguments)]
    async fn retrieve(
        &self,
        ctx: &AuthContext,
        query: &str,
        query_vector: Option<&[f32]>,
        mode: SearchMode,
        fetch_k: usize,
        prefilter: &PreFilter,
        exclude: &[Uuid],
    ) -> Result<(Vec<ScoredChunk>, Vec<ScoredChunk>), RagError> {
        let dense_fut = async {
            match (mode, query_vector) {
                (SearchMode::Keyword, _) | (_, None) => Ok(Vec::new()),
                (_, Some(vector)) => {
                    self.vector
                        .search(
                            ctx,
                            &VectorSearch {
                                vector,
                                limit: fetch_k,
                                prefilter,
                                exclude,
                            },
                        )
                        .await
                }
            }
        };
        let keyword_fut = async {
            if mode == SearchMode::Dense {
                return Ok(Vec::new());
            }
            // Tantivy は同期 API のため blocking スレッドで実行する。
            let fulltext = Arc::clone(&self.fulltext);
            let ctx = ctx.clone();
            let query = query.to_string();
            let prefilter = prefilter.clone();
            let exclude = exclude.to_vec();
            tokio::task::spawn_blocking(move || {
                fulltext.search(&ctx, &query, fetch_k, &prefilter, &exclude)
            })
            .await
            .map_err(|e| RagError::Fulltext(format!("spawn_blocking: {e}")))?
        };
        futures::try_join!(dense_fut, keyword_fut)
    }

    /// rag_chunk × node のハイドレーション。**`deleted_at is null` を強制**し、
    /// 索引除去が追いつく前でも削除済みファイルが結果に出ない（第三の防壁）。
    async fn hydrate(
        &self,
        ctx: &AuthContext,
        chunks: &[ScoredChunk],
    ) -> Result<HashMap<Uuid, HydratedChunk>, RagError> {
        if chunks.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<Uuid> = chunks.iter().map(|c| c.chunk_id).collect();
        let rows: Vec<HydratedChunk> = sqlx::query_as(
            "select c.id, c.node_id, c.version, c.parent_id, c.page, c.heading_path, c.content, \
                    n.name as file_name, n.parent_id as folder_id \
             from rag_chunk c \
             join node n on n.id = c.node_id and n.tenant_id = c.tenant_id \
             where c.tenant_id = $1 and c.id = any($2) and n.deleted_at is null",
        )
        .bind(&ctx.tenant_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| (r.id, r)).collect())
    }

    /// reranker で並べ替えた chunk_id 列を返す（本文が引けない chunk は落ちる）。
    async fn rerank(
        &self,
        ctx: &AuthContext,
        query: &str,
        allowed: &[ScoredChunk],
        rows: &HashMap<Uuid, HydratedChunk>,
    ) -> Result<Vec<Uuid>, RagError> {
        let passages: Vec<RerankPassage> = allowed
            .iter()
            .filter_map(|c| rows.get(&c.chunk_id))
            .map(|row| RerankPassage {
                id: row.id.to_string(),
                text: row.content.clone(),
            })
            .collect();
        if passages.len() <= 1 {
            return Ok(passages
                .iter()
                .filter_map(|p| Uuid::parse_str(&p.id).ok())
                .collect());
        }
        let mut scores = self.reranker.rerank(ctx, query, &passages).await?;
        scores.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        Ok(scores
            .into_iter()
            .filter_map(|s| Uuid::parse_str(&s.id).ok())
            .collect())
    }

    /// 最終結果の組み立て（親チャンク本文の展開・親の重複はそのまま許容）。
    async fn build_results(
        &self,
        ctx: &AuthContext,
        final_ids: &[Uuid],
        rows: &HashMap<Uuid, HydratedChunk>,
    ) -> Result<Vec<SearchResult>, RagError> {
        let parent_ids: Vec<Uuid> = final_ids
            .iter()
            .filter_map(|id| rows.get(id).and_then(|r| r.parent_id))
            .collect();
        let parents: HashMap<Uuid, String> = if parent_ids.is_empty() {
            HashMap::new()
        } else {
            let rows: Vec<(Uuid, String)> = sqlx::query_as(
                "select id, content from rag_chunk where tenant_id = $1 and id = any($2)",
            )
            .bind(&ctx.tenant_id)
            .bind(&parent_ids)
            .fetch_all(&self.pool)
            .await?;
            rows.into_iter().collect()
        };

        Ok(final_ids
            .iter()
            .enumerate()
            .filter_map(|(rank, id)| rows.get(id).map(|r| (rank, r)))
            .map(|(rank, row)| {
                // rank は top_k（≦max_top_k=50）に有界で f32 の精度内。
                #[allow(clippy::cast_precision_loss)]
                let score = 1.0 / (rank as f32 + 1.0);
                SearchResult {
                    chunk_id: row.id,
                    file_id: row.node_id,
                    file_name: row.file_name.clone(),
                    folder_id: row.folder_id,
                    page: row.page,
                    heading_path: row.heading_path.clone(),
                    content: row.content.clone(),
                    parent_content: row.parent_id.and_then(|p| parents.get(&p).cloned()),
                    // rerank 後の順位ベースのスコア（表示用に単調減少へ正規化）。
                    score,
                    version: row.version,
                }
            })
            .collect())
    }

    /// 引用監査: LLM/UI に出す chunk_id 群とその時の file 粒度認可判定を記録する。
    async fn audit_citations(
        &self,
        ctx: &AuthContext,
        query: &str,
        results: &[SearchResult],
        file_decisions: &HashMap<Uuid, bool>,
        trace_id: Option<&str>,
    ) -> Result<(), RagError> {
        let query_sha256 = hex_sha256(query);
        let (allowed_files, denied_files): (Vec<&Uuid>, Vec<&Uuid>) = {
            let mut allowed = Vec::new();
            let mut denied = Vec::new();
            for (file, ok) in file_decisions {
                if *ok {
                    allowed.push(file);
                } else {
                    denied.push(file);
                }
            }
            (allowed, denied)
        };
        let metadata = serde_json::json!({
            "query_sha256": query_sha256,
            "cited_chunk_ids": results.iter().map(|r| r.chunk_id).collect::<Vec<_>>(),
            "cited_file_ids": results.iter().map(|r| r.file_id).collect::<Vec<_>>(),
            "file_decisions": { "allowed": allowed_files, "denied": denied_files },
        });
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "rag.search",
                    object_type: "rag_query",
                    object_id: &query_sha256,
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await?;
        Ok(())
    }
}

fn hex_sha256(text: &str) -> String {
    use std::fmt::Write;
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// ハイドレーション結果の 1 行。
#[derive(Debug, Clone, sqlx::FromRow)]
struct HydratedChunk {
    id: Uuid,
    node_id: Uuid,
    version: i64,
    parent_id: Option<Uuid>,
    page: Option<i32>,
    heading_path: Vec<String>,
    content: String,
    file_name: String,
    folder_id: Option<Uuid>,
}
