//! `SearchService` — permission-aware ハイブリッド検索（Task 2.6/2.7/2.10）。
//!
//! 段取り（docs/design.md §4.3）:
//! 1. 可読集合の算出（pre-filter・上限超過で tenant-only 縮退）
//! 2. クエリ埋め込み → dense（Qdrant）/ keyword（Tantivy）並列取得（over-fetch）
//! 3. RRF 融合・重複排除
//! 4. **post-filter（OpenFGA file 粒度・HigherConsistency）を reranker の前に**（PIT-2）
//! 5. ハイドレーション（node JOIN・org 境界と `deleted_at is null` を強制）— **ループ内**。
//!    ここで落ちた候補は最終結果に出せないので、採択せずバックフィルの対象にする（#377）
//! 6. 不足時バックフィル（fetch_k 倍増・最大 3 ラウンド・候補が尽きるまで top_k を保証）
//! 7. rerank（認可済み・生存候補の上位 rerank_pool 件のみ）＋親チャンク展開
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
use crate::rerank::Reranker;
use crate::search_types::{SearchDebug, SearchMode, SearchResult, SearchScope, StageTimings};
use crate::vector_store::{PreFilter, ScoredChunk, VectorSearch, VectorStore};

mod hydrate;

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

    /// permission-aware 検索。`scope` は知識スコープ（skill・Task 6.8）による**絞り込み**で、
    /// `None` は従来どおり全可読範囲。スコープを広く設定しても最終可読性は post-filter
    /// （OpenFGA file check）が常に再検証する（広げる方向には一切働かない）。
    pub async fn search(
        &self,
        ctx: &AuthContext,
        query: &str,
        top_k: Option<u32>,
        mode: SearchMode,
        scope: Option<&SearchScope>,
        trace_id: Option<&str>,
    ) -> Result<SearchOutput, RagError> {
        let top_k = (top_k.unwrap_or(self.config.default_top_k as u32) as usize)
            .clamp(1, self.config.max_top_k);
        let mut debug = SearchDebug::default();
        let mut timings = StageTimings::default();

        // 知識スコープ → 構造タグ（`folder:<t>|<id>` は配下全体をカバー）。
        // 空スコープは「絞らない」として扱う（skill 側の保存時検証が空を拒否する前提の防御）。
        let scope_tags: Vec<String> = scope.map_or_else(Vec::new, |s| {
            let ns = ctx.ns();
            s.folders
                .iter()
                .map(|id| ns.folder(&id.to_string()).as_str().to_string())
                .chain(
                    s.files
                        .iter()
                        .map(|id| ns.file(&id.to_string()).as_str().to_string()),
                )
                .collect()
        });

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

        // 3〜5. 取得 → RRF → post-filter → **hydrate** → バックフィル。
        //
        // hydrate をループ**内**に置くのが #377 の要点。hydrate は org 境界（`n.org = ctx.org`・
        // PIT-45）と `deleted_at is null` を課すので、ここで行が引けない候補は最終結果に出ない。
        // ループ後に 1 回だけ hydrate していた頃は、それらが pool を埋めたまま黙って落ちるため、
        // マルチ org テナントで別 org の file に直接 viewer を持つユーザーだと、別 org の高スコア
        // チャンクが枠を食って要求 `top_k` より少ない（最悪 0 件）結果になり得た（漏洩はしない）。
        // 「行が引けたか」を採択条件にすると、既存の `exclude`／`fetch_k` 倍化機構がそのまま
        // 埋め直しに働く（索引側の変更＝再インデックスを要さない）。
        let pool_target = top_k.max(self.config.rerank_pool);
        let mut fetch_k = (pool_target * over_fetch.max(1)).min(MAX_FETCH_K);
        let mut allowed: Vec<ScoredChunk> = Vec::new();
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut file_decisions: HashMap<Uuid, bool> = HashMap::new();
        let t_retrieve = Instant::now();
        let mut post_filter_ms = 0u64;
        let mut liveness_ms = 0u64;
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
                    &scope_tags,
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

            // このラウンドの許可候補のうち、**最終結果に出せるものだけ**を採択する（#377）。
            // 判定は id だけを引く軽量クエリで行う（本文は truncate 後にまとめて読む・Codex P2:
            // ここで content を引くと fetch_k 件ぶんの本文転送が毎ラウンド走り、従来の
            // 「最終プール 32 件だけ hydrate」より DB 負荷が大幅に増える）。
            // 落ちた分（他 org / 削除済み）は `seen` に入っているので再取得されず、次ラウンドが
            // fetch_k を倍にして同 org の生存チャンクで枠を埋め直す。
            let t_h = Instant::now();
            let live = self.live_chunk_ids(ctx, &round_allowed).await?;
            liveness_ms += t_h.elapsed().as_millis() as u64;
            debug.hydrate_dropped += (round_allowed.len().saturating_sub(live.len())) as u32;
            allowed.extend(
                round_allowed
                    .into_iter()
                    .filter(|c| live.contains(&c.chunk_id)),
            );

            if allowed.len() >= pool_target
                || exhausted
                || debug.backfill_rounds >= MAX_BACKFILL_ROUNDS
            {
                break;
            }
            fetch_k = (fetch_k * 2).min(MAX_FETCH_K);
        }
        timings.retrieve_ms =
            t_retrieve.elapsed().as_millis() as u64 - post_filter_ms - liveness_ms;
        timings.post_filter_ms = post_filter_ms;

        // 融合スコア順で rerank プールへ。
        allowed.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        allowed.truncate(self.config.rerank_pool.max(top_k));

        // 本文の hydration は**プール確定後に 1 回だけ**（従来どおりの転送量）。ここでも org /
        // deleted_at の述語が再評価されるので、ループ中に soft-delete された node の本文が
        // 早いラウンドのスナップショット経由で結果に残ることもない（Codex P2）。
        let t = Instant::now();
        let rows = self.hydrate(ctx, &allowed).await?;
        timings.hydrate_ms = t.elapsed().as_millis() as u64 + liveness_ms;
        // 最終 hydration で落ちた分も採択から外す（rerank/build_results の filter_map と揃える）。
        // ラウンド判定〜ここまでの間に org 不一致・削除が判明した分も `hydrate_dropped` に含める
        // （除外件数を 1 つの指標で一貫して説明できるようにする・CodeRabbit）。
        let before_final_hydrate = allowed.len();
        allowed.retain(|c| rows.contains_key(&c.chunk_id));
        debug.hydrate_dropped += before_final_hydrate.saturating_sub(allowed.len()) as u32;

        // 6. rerank（認可済み・生存チャンクのみ）。
        let t = Instant::now();
        let ranked = self.rerank(ctx, query, &allowed, &rows).await?;
        debug.reranked = ranked.len() as u32;
        timings.rerank_ms = t.elapsed().as_millis() as u64;

        // 7(後半). 上位 top_k を確定し、親チャンクを展開する。
        let final_ids: Vec<Uuid> = ranked.into_iter().take(top_k).collect();
        let results = self.build_results(ctx, &final_ids, &rows).await?;

        // 8. 引用監査（Task 2.7 受入条件: 引用 chunk と認可判定が監査ログに残る）。
        self.audit_citations(ctx, query, &results, &file_decisions, scope, trace_id)
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
        scope_tags: &[String],
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
                                scope_tags,
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
            let scope_tags = scope_tags.to_vec();
            let exclude = exclude.to_vec();
            tokio::task::spawn_blocking(move || {
                fulltext.search(&ctx, &query, fetch_k, &prefilter, &scope_tags, &exclude)
            })
            .await
            .map_err(|e| RagError::Fulltext(format!("spawn_blocking: {e}")))?
        };
        futures::try_join!(dense_fut, keyword_fut)
    }

    /// 引用監査: LLM/UI に出す chunk_id 群とその時の file 粒度認可判定を記録する。
    #[allow(clippy::too_many_arguments)] // 監査に載せる文脈の束（呼び出しは search 内の 1 箇所）。
    async fn audit_citations(
        &self,
        ctx: &AuthContext,
        query: &str,
        results: &[SearchResult],
        file_decisions: &HashMap<Uuid, bool>,
        scope: Option<&SearchScope>,
        trace_id: Option<&str>,
    ) -> Result<(), RagError> {
        // tenant を混ぜ、tenant 横断でのレインボーテーブル再利用を防ぐ（真のペッパーは
        // KeyProvider（Phase 10）導入時に移行。監査ログ閲覧自体は管理権限で保護される）。
        let query_sha256 = hex_sha256(&format!("{}\x00{}", ctx.tenant_id, query));
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
        let mut metadata = serde_json::json!({
            "query_sha256": query_sha256,
            "cited_chunk_ids": results.iter().map(|r| r.chunk_id).collect::<Vec<_>>(),
            "cited_file_ids": results.iter().map(|r| r.file_id).collect::<Vec<_>>(),
            "file_decisions": { "allowed": allowed_files, "denied": denied_files },
        });
        // 知識スコープ（skill・Task 6.8/6.12）を適用した検索であることを監査に残す。
        if let Some(scope) = scope {
            metadata["scope"] = serde_json::json!({
                "folders": scope.folders,
                "files": scope.files,
            });
        }
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
