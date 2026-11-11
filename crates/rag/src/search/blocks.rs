//! 版のブロック列（`doc_block`）の読み出し（出典パネル・#508）。
//!
//! **認可は呼び出し側が StorageService で済ませた証拠（[`storage::Node`]）を受け取る形にする。**
//! ここで独自の権限判定はしない（単一チョークポイント）。`Node` は `get_metadata` などの
//! viewer 判定を通った結果としてしか手に入らないので、未認可のまま読む経路を型で塞ぐ。
//! 加えて tenant_id と org で必ず絞る（node の所属と同じ境界）。

use authz::AuthContext;
use storage::audit::AuditAction;
use storage::audit::{AuditEntry, Decision};

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
        trace_id: Option<&str>,
    ) -> Result<DocBlocksPage, RagError> {
        self.blocks_on_page(ctx, node, version, from, limit, None, trace_id)
            .await
    }

    /// PDF のページで絞った窓。跨ページブロックは provenance のページも照合する。
    #[allow(clippy::too_many_arguments)]
    pub async fn blocks_on_page(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        version: i64,
        from: i32,
        limit: u32,
        page: Option<i32>,
        trace_id: Option<&str>,
    ) -> Result<DocBlocksPage, RagError> {
        self.blocks_in_page_range(ctx, node, version, from, limit, page, page, trace_id)
            .await
    }

    /// ページ範囲は最大16ページ。検索の一括取得でも認可済み Node と本文監査を維持する。
    #[allow(clippy::too_many_arguments)]
    pub async fn blocks_in_page_range(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        version: i64,
        from: i32,
        limit: u32,
        page: Option<i32>,
        page_end: Option<i32>,
        trace_id: Option<&str>,
    ) -> Result<DocBlocksPage, RagError> {
        let page_end = page.map(|start| {
            page_end
                .unwrap_or(start)
                .clamp(start, start.saturating_add(15))
        });
        let limit = limit.clamp(1, MAX_BLOCKS_PER_PAGE);
        // 1 件多く取り、続きがあるかを判定する。
        // ページ指定と未指定は別の prepared statement にする。nullable parameter の
        // OR で包むと generic plan がページ索引を使えなくなる。
        let page_filter = if page.is_some() {
            "and (page between $7 and $8 or jsonb_path_query_array(prov, '$[*].page') @> any($9::jsonb[]))"
        } else {
            ""
        };
        let statement = format!(
            "select ordinal, type, level, text, list_marker, page, prov from doc_block \
             where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
               and ordinal >= $5 {page_filter} order by ordinal limit $6"
        );
        let query = sqlx::query_as(&statement)
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(node.id)
            .bind(version)
            .bind(from.max(0))
            .bind(i64::from(limit) + 1);
        let query = if let Some(page) = page {
            let end = page_end.unwrap_or(page);
            let pages: Vec<serde_json::Value> = (page..=end)
                .map(|value| serde_json::json!([value]))
                .collect();
            query.bind(page).bind(end).bind(pages)
        } else {
            query
        };
        let mut rows: Vec<DocBlock> = query.fetch_all(&self.pool).await?;
        let next_from = if rows.len() > limit as usize {
            rows.truncate(limit as usize);
            rows.last().map(|b| b.ordinal + 1)
        } else {
            None
        };
        // メタデータの参照（get_metadata）とは別に、本文を読み出したことを残す（design §4.9）。
        let first = rows.first().map(|b| b.ordinal);
        let last = rows.last().map(|b| b.ordinal);
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::FileBlocksRead,
                    object_type: "file",
                    object_id: &node.id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: serde_json::json!({
                        "version": version,
                        "page": page,
                        "page_end": page_end,
                        "ordinals": [first, last],
                    }),
                },
            )
            .await?;
        Ok(DocBlocksPage {
            blocks: rows,
            next_from,
        })
    }
}
