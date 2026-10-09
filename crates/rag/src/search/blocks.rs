//! 版のブロック列（`doc_block`）の読み出し（出典パネル・#508）。
//!
//! **認可は呼び出し側が StorageService で済ませた証拠（[`storage::Node`]）を受け取る形にする。**
//! ここで独自の権限判定はしない（単一チョークポイント）。`Node` は `get_metadata` などの
//! viewer 判定を通った結果としてしか手に入らないので、未認可のまま読む経路を型で塞ぐ。
//! 加えて tenant_id と org で必ず絞る（node の所属と同じ境界）。

use authz::AuthContext;

use crate::error::RagError;
use crate::search::SearchService;
use crate::search_types::{DocBlock, DocBlocksPage};

/// 1 回に返すブロック数の上限（大きな文書でも窓で引く・全件取得しない）。
pub const MAX_BLOCKS_PER_PAGE: u32 = 200;

impl SearchService {
    /// `node` の版 `version` のブロック列を、ordinal `from` から `limit` 件返す。
    pub async fn blocks(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        version: i64,
        from: i32,
        limit: u32,
    ) -> Result<DocBlocksPage, RagError> {
        let limit = limit.clamp(1, MAX_BLOCKS_PER_PAGE);
        // 1 件多く取り、続きがあるかを判定する。
        let mut rows: Vec<DocBlock> = sqlx::query_as(
            "select ordinal, type, level, text, list_marker, page, prov from doc_block \
             where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
               and ordinal >= $5 \
             order by ordinal limit $6",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(node.id)
        .bind(version)
        .bind(from.max(0))
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;
        let next_from = if rows.len() > limit as usize {
            rows.truncate(limit as usize);
            rows.last().map(|b| b.ordinal + 1)
        } else {
            None
        };
        Ok(DocBlocksPage {
            blocks: rows,
            next_from,
        })
    }
}
