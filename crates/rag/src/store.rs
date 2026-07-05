//! rag_chunk / rag_ingest_job の永続化（本文の正本・Task 2.2/2.8）。
//!
//! Qdrant / Tantivy には ID＋検索用データのみを持たせ、本文と authz_tags の正本は
//! Postgres の `rag_chunk` が持つ。move（タグ再評価）・全文の再投入・検索の
//! ハイドレーションはここを読む。

use authz::AuthContext;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::RagError;
use crate::types::{Chunk, ChunkKind};

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

/// ノードのチャンクを差し替える（旧版含む全行 DELETE → 新版 INSERT・単一 Tx）。
///
/// 決定的 chunk_id と合わせ、同一版の再実行も冪等になる。
pub async fn replace_chunks(
    pool: &PgPool,
    ctx: &AuthContext,
    node_id: Uuid,
    version: i64,
    chunks: &[Chunk],
    authz_tags: &[String],
    embedding_model_version: &str,
) -> Result<(), RagError> {
    let mut tx = pool.begin().await?;
    sqlx::query("delete from rag_chunk where tenant_id = $1 and node_id = $2")
        .bind(&ctx.tenant_id)
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    for chunk in chunks {
        // 埋め込み対象（leaf/table）にのみ model version を刻む（PIT-8）。
        let model_version = match chunk.kind {
            ChunkKind::Parent => None,
            ChunkKind::Leaf | ChunkKind::Table => Some(embedding_model_version),
        };
        sqlx::query(
            "insert into rag_chunk \
                 (id, tenant_id, org, node_id, version, parent_id, kind, ordinal, page, \
                  heading_path, content, char_count, authz_tags, embedding_model_version) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(chunk.id)
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(node_id)
        .bind(version)
        .bind(chunk.parent_id)
        .bind(chunk.kind.as_str())
        .bind(chunk.ordinal)
        .bind(chunk.page)
        .bind(&chunk.heading_path)
        .bind(&chunk.content)
        .bind(i32::try_from(chunk.content.chars().count()).unwrap_or(i32::MAX))
        .bind(authz_tags)
        .bind(model_version)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
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

/// delete: ノードの全チャンク行を削除する。
pub async fn delete_node(pool: &PgPool, ctx: &AuthContext, node_id: Uuid) -> Result<(), RagError> {
    sqlx::query("delete from rag_chunk where tenant_id = $1 and node_id = $2")
        .bind(&ctx.tenant_id)
        .bind(node_id)
        .execute(pool)
        .await?;
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
    let jobs = sqlx::query("delete from rag_ingest_job where tenant_id = $1")
        .bind(tenant_id)
        .execute(conn)
        .await?
        .rows_affected();
    Ok(chunks + jobs)
}
