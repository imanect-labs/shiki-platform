//! 検索の後段: 採択判定（軽量）・本文ハイドレーション・rerank・最終結果の組み立て。
//!
//! `search.rs`（親）の `SearchService` に対する impl ブロックを分割したもの（行数ガード）。
//! 子モジュールなので親の private フィールド（`pool` 等）へそのまま触れる。
//!
//! **org 境界（#371・PIT-45）と `deleted_at is null` を課すのはこのモジュールの責務**。
//! 採択判定（[`SearchService::live_chunk_ids`]）と本文取得（[`SearchService::hydrate`]）は
//! **同じ述語**を持たなければならない（食い違うと「採択したのに本文が引けない」欠員が復活する）。

use std::collections::{HashMap, HashSet};

use authz::AuthContext;
use uuid::Uuid;

use crate::error::RagError;
use crate::rerank::RerankPassage;
use crate::search::SearchService;
use crate::search_types::SearchResult;
use crate::vector_store::ScoredChunk;

/// ハイドレーション結果の 1 行。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct HydratedChunk {
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

impl SearchService {
    /// 候補のうち「最終結果に出せる」chunk_id だけを返す軽量クエリ（#377）。
    ///
    /// [`Self::hydrate`] と**同じ述語**（org 境界＋`deleted_at is null`）を課すが、`content` や
    /// `heading_path` を読まない。バックフィルの採択判定はラウンドごとに走るため、ここで本文まで
    /// 引くと fetch_k 件ぶんの転送が毎ラウンド発生する（本文は最終プール確定後に 1 回だけ読む）。
    ///
    /// ⚠️ hydrate と述語が食い違うと「採択したのに本文が引けない」欠員が復活するので、両者は
    /// 対で保守する（テスト `backfill_recovers_top_k_when_cross_org_chunks_outrank` が回帰を押さえる）。
    pub(super) async fn live_chunk_ids(
        &self,
        ctx: &AuthContext,
        chunks: &[ScoredChunk],
    ) -> Result<HashSet<Uuid>, RagError> {
        if chunks.is_empty() {
            return Ok(HashSet::new());
        }
        let ids: Vec<Uuid> = chunks.iter().map(|c| c.chunk_id).collect();
        let live: Vec<Uuid> = sqlx::query_scalar(
            "select c.id from rag_chunk c \
             join node n on n.id = c.node_id and n.tenant_id = c.tenant_id \
             where c.tenant_id = $1 and c.id = any($2) and n.org = $3 and n.deleted_at is null",
        )
        .bind(&ctx.tenant_id)
        .bind(&ids)
        .bind(&ctx.org)
        .fetch_all(&self.pool)
        .await?;
        Ok(live.into_iter().collect())
    }

    /// rag_chunk × node のハイドレーション。**`deleted_at is null` を強制**し、
    /// 索引除去が追いつく前でも削除済みファイルが結果に出ない（第三の防壁）。
    ///
    /// org 境界（#371・PIT-45）: `storage::load_node` は `org = ctx.org AND tenant_id` で絞るため、
    /// org は tenant 内のもう一段の隔離境界。hydrate も `n.org = ctx.org` を課し、マルチ org テナントで
    /// 他 org 文書のチャンクが回答に混入しない（storage の直接オープンと同じ境界へ揃える）。
    ///
    /// **プール確定後に 1 回だけ呼ぶ**（#377・CodeRabbit）。バックフィル各ラウンドでの採択判定は
    /// 本メソッドではなく [`Self::live_chunk_ids`]（id だけを引く軽量クエリ）が担う —— ここで毎
    /// ラウンド本文を引くと `fetch_k` 件ぶんの転送が繰り返され、従来より DB 負荷が大幅に増える。
    /// ここでの本文取得は最終結果の組み立てと rerank のためで、落ちた chunk は呼び出し側の
    /// `retain` で `allowed` から外れる（判定〜最終取得の間に削除された node がここで消える）。
    pub(super) async fn hydrate(
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
             where c.tenant_id = $1 and c.id = any($2) and n.org = $3 and n.deleted_at is null",
        )
        .bind(&ctx.tenant_id)
        .bind(&ids)
        .bind(&ctx.org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| (r.id, r)).collect())
    }

    /// reranker で並べ替えた chunk_id 列を返す（本文が引けない chunk は落ちる）。
    pub(super) async fn rerank(
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
    pub(super) async fn build_results(
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
            // 親本文も node.org と deleted_at で絞る（#371・CodeRabbit）。親子は同一 node（同 org）
            // だが、hydrate / live_chunk_ids と**同じ述語**を明示適用して parent_content 経由の
            // 他 org 混入と削除済みノードの本文流出を構造的に断つ（子の hydrate とこの呼び出しの
            // 間に soft-delete され得るため、org だけでは第三の防壁に穴が残る）。
            let rows: Vec<(Uuid, String)> = sqlx::query_as(
                "select c.id, c.content from rag_chunk c \
                 join node n on n.id = c.node_id and n.tenant_id = c.tenant_id \
                 where c.tenant_id = $1 and c.id = any($2) and n.org = $3 \
                   and n.deleted_at is null",
            )
            .bind(&ctx.tenant_id)
            .bind(&parent_ids)
            .bind(&ctx.org)
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
}
