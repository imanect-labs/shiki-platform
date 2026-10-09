//! rag_chunk / rag_ingest_job の永続化（本文の正本・Task 2.2/2.8）。
//!
//! Qdrant / Tantivy には ID＋検索用データのみを持たせ、本文と authz_tags の正本は
//! Postgres の `rag_chunk` が持つ。move（タグ再評価）・全文の再投入・検索の
//! ハイドレーションはここを読む。

use authz::AuthContext;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::RagError;
use crate::types::{Chunk, ChunkKind, ParsedBlock};

/// rag_chunk の 1 行（ハイドレーション・再投入用）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoredChunk {
    pub id: Uuid,
    pub node_id: Uuid,
    pub version: i64,
    pub parent_id: Option<Uuid>,
    pub kind: String,
    pub page: Option<i32>,
    pub heading_path: Vec<String>,
    pub content: String,
    pub authz_tags: Vec<String>,
}

impl StoredChunk {
    /// 全文索引・埋め込みに使う検索用テキスト（`Chunk::searchable_text` と同じ規約）。
    pub fn searchable_text(&self) -> String {
        if self.heading_path.is_empty() {
            self.content.clone()
        } else {
            format!("{}\n{}", self.heading_path.join(" > "), self.content)
        }
    }
}

/// 1 文の INSERT に載せる行数の上限。
///
/// Postgres のバインド引数は 1 文あたり 65535 個までで、rag_chunk は 1 行が 21 個を使うので
/// 上限は約 3100 行（doc_block は 11 個）。余裕を見てここで切る。
const INSERT_BATCH_ROWS: usize = 1000;

/// ノードのチャンクを差し替える（旧版含む全行 DELETE → 新版 INSERT・単一 Tx）。
///
/// 同じトランザクションで、この版のブロック列（`doc_block`）も書く。ブロック列は版ごとに
/// 残す（古い版を引用した会話の出典パネル用・#508）。
///
/// 決定的 chunk_id と合わせ、同一版の再実行も冪等になる。
#[allow(clippy::too_many_arguments)] // 対象ノード・版・チャンク・ブロック・タグ・モデル版は本質的。
pub async fn replace_chunks(
    pool: &PgPool,
    ctx: &AuthContext,
    node_id: Uuid,
    version: i64,
    chunks: &[Chunk],
    blocks: &[ParsedBlock],
    authz_tags: &[String],
    embedding_model_version: &str,
) -> Result<(), RagError> {
    let mut tx = pool.begin().await?;
    sqlx::query("delete from rag_chunk where tenant_id = $1 and node_id = $2")
        .bind(&ctx.tenant_id)
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    replace_blocks(&mut tx, ctx, node_id, version, blocks).await?;
    // 1 行 1 クエリで回さない。チャンク数は文書長に比例し、空行を持たない大きな
    // text/plain では数万行に達する（PIT-67）。往復ぶんだけトランザクションが伸び、
    // その間 delete で取った行ロックを握り続けることになる。
    for batch in chunks.chunks(INSERT_BATCH_ROWS) {
        let mut qb = sqlx::QueryBuilder::new(
            "insert into rag_chunk \
                 (id, tenant_id, org, node_id, version, parent_id, kind, ordinal, page, \
                  heading_path, content, char_count, authz_tags, embedding_model_version, \
                  block_start, off_start, block_end, off_end, quote_prefix, quote_suffix, boxes) ",
        );
        qb.push_values(batch, |mut row, chunk| {
            // 埋め込み対象（leaf/table）にのみ model version を刻む（PIT-8）。
            let model_version = match chunk.kind {
                ChunkKind::Parent => None,
                ChunkKind::Leaf | ChunkKind::Table => Some(embedding_model_version),
            };
            row.push_bind(chunk.id)
                .push_bind(&ctx.tenant_id)
                .push_bind(&ctx.org)
                .push_bind(node_id)
                .push_bind(version)
                .push_bind(chunk.parent_id)
                .push_bind(chunk.kind.as_str())
                .push_bind(chunk.ordinal)
                .push_bind(chunk.page)
                .push_bind(&chunk.heading_path)
                .push_bind(&chunk.content)
                .push_bind(i32::try_from(chunk.content.chars().count()).unwrap_or(i32::MAX))
                .push_bind(authz_tags)
                .push_bind(model_version)
                .push_bind(chunk.anchor.map(|a| a.block_start))
                .push_bind(chunk.anchor.map(|a| a.off_start))
                .push_bind(chunk.anchor.map(|a| a.block_end))
                .push_bind(chunk.anchor.map(|a| a.off_end))
                .push_bind(&chunk.quote_prefix)
                .push_bind(&chunk.quote_suffix)
                .push_bind(sqlx::types::Json(&chunk.boxes));
        });
        // 同一 (node, version) の重複ジョブが並行実行されても衝突しない（決定的 ID
        // かつ内容も決定的なので do nothing で同値。at-least-once 配信の並行冪等性）。
        qb.push(" on conflict (id) do nothing");
        qb.build().execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// この版のブロック列を書き直す（同じ版の再インジェストでも同じ内容になる）。
async fn replace_blocks(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    node_id: Uuid,
    version: i64,
    blocks: &[ParsedBlock],
) -> Result<(), RagError> {
    sqlx::query("delete from doc_block where tenant_id = $1 and node_id = $2 and version = $3")
        .bind(&ctx.tenant_id)
        .bind(node_id)
        .bind(version)
        .execute(&mut **tx)
        .await?;
    let numbered: Vec<(i32, &ParsedBlock)> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (i32::try_from(i).unwrap_or(i32::MAX), b))
        .collect();
    for batch in numbered.chunks(INSERT_BATCH_ROWS) {
        let mut qb = sqlx::QueryBuilder::new(
            "insert into doc_block \
                 (tenant_id, org, node_id, version, ordinal, type, level, text, list_marker, \
                  page, prov) ",
        );
        qb.push_values(batch, |mut row, (ordinal, block)| {
            row.push_bind(&ctx.tenant_id)
                .push_bind(&ctx.org)
                .push_bind(node_id)
                .push_bind(version)
                .push_bind(*ordinal)
                .push_bind(block.block_type.as_str())
                .push_bind(block.level.and_then(|l| i32::try_from(l).ok()))
                .push_bind(&block.text)
                .push_bind(&block.list_marker)
                .push_bind(block.page)
                .push_bind(sqlx::types::Json(&block.prov));
        });
        qb.push(" on conflict do nothing");
        qb.build().execute(&mut **tx).await?;
    }
    Ok(())
}

/// move: authz_tags を再評価して全行更新する（本文・ベクタは触らない）。
pub async fn update_tags(
    pool: &PgPool,
    ctx: &AuthContext,
    node_id: Uuid,
    authz_tags: &[String],
) -> Result<(), RagError> {
    sqlx::query("update rag_chunk set authz_tags = $3 where tenant_id = $1 and node_id = $2")
        .bind(&ctx.tenant_id)
        .bind(node_id)
        .bind(authz_tags)
        .execute(pool)
        .await?;
    Ok(())
}

/// delete: ノードの全チャンク行と、全版のブロック列を削除する。
pub async fn delete_node(pool: &PgPool, ctx: &AuthContext, node_id: Uuid) -> Result<(), RagError> {
    let mut tx = pool.begin().await?;
    sqlx::query("delete from rag_chunk where tenant_id = $1 and node_id = $2")
        .bind(&ctx.tenant_id)
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from doc_block where tenant_id = $1 and node_id = $2")
        .bind(&ctx.tenant_id)
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// ノードの全チャンク行（move 時の全文再投入用・ordinal 順）。
pub async fn chunks_for_node(
    pool: &PgPool,
    ctx: &AuthContext,
    node_id: Uuid,
) -> Result<Vec<StoredChunk>, RagError> {
    let rows = sqlx::query_as::<_, StoredChunk>(
        "select id, node_id, version, parent_id, kind, page, heading_path, content, authz_tags \
         from rag_chunk where tenant_id = $1 and node_id = $2 order by ordinal",
    )
    .bind(&ctx.tenant_id)
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// テナント消去（SAAS.2）: rag_chunk / rag_ingest_job を破棄する。
pub async fn purge_tenant(conn: &mut PgConnection, tenant_id: &str) -> Result<u64, RagError> {
    let chunks = sqlx::query("delete from rag_chunk where tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    let blocks = sqlx::query("delete from doc_block where tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    let jobs = sqlx::query("delete from rag_ingest_job where tenant_id = $1")
        .bind(tenant_id)
        .execute(conn)
        .await?
        .rows_affected();
    Ok(chunks + blocks + jobs)
}
