//! StorageService: 複数ファイルのメタデータを表示用にまとめて読む（職員間メッセージの file_ref・Task 14.6）。
//!
//! 1 件ずつ [`StorageService::get_metadata`] を呼ぶと、添付 N 件ごとに DB・FGA check・監査が N 回走る。
//! ここでは期限切れ共有リンクの遅延失効（該当があるものだけ）→ viewer の batch_check →
//! 読めるものだけを 1 クエリで読み、監査も 1 行にまとめる。権限の判定は get_metadata と同じ
//! （viewer・共有リンクの期限・削除済みは返さない）。

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::{AuditAction, AuditEntry, Decision};
use authz::CheckTuple;

/// 1 回で扱う件数の上限（全件取得を防ぐ）。
const MAX_BATCH: usize = 2_000;

impl StorageService {
    /// `ids` のうち閲覧者が読めるファイルのメタデータ（読めない・無い・削除済みは含めない）。
    ///
    /// # Errors
    ///
    /// DB・認可基盤の失敗（fail-closed: 呼び出し側は「読めない」として扱う）。
    pub async fn readable_files_metadata(
        &self,
        ctx: &AuthContext,
        ids: &[Uuid],
        purpose: &str,
    ) -> Result<Vec<Node>, StorageError> {
        let mut ids: Vec<Uuid> = ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        ids.truncate(MAX_BATCH);
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // 期限切れの共有リンクが残るものだけ、先にタプルを剥奪する（get_metadata と同じ遅延失効）。
        let expiring: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT node_id FROM node_share_link WHERE node_id = ANY($1) AND tenant_id = $2 \
               AND revoked_at IS NULL AND expires_at IS NOT NULL AND expires_at <= now()",
        )
        .bind(&ids)
        .bind(ctx.tenant_id())
        .fetch_all(&self.db)
        .await?;
        for id in expiring {
            self.enforce_share_link_expiry(ctx, id, NodeKind::File).await?;
        }
        let items: Vec<CheckTuple> = ids
            .iter()
            .map(|id| CheckTuple {
                subject: ctx.subject(),
                relation: Relation::Viewer,
                object: ctx.ns().file(&id.to_string()),
            })
            .collect();
        let ok = self
            .authz
            .batch_check(&items, Consistency::HigherConsistency)
            .await?;
        let readable: Vec<Uuid> = ids
            .iter()
            .zip(ok)
            .filter_map(|(id, ok)| ok.then_some(*id))
            .collect();
        if readable.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<NodeRow> = sqlx::query_as(&format!(
            "SELECT {NODE_COLS} FROM node WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 \
               AND deleted_at IS NULL AND kind = 'file'"
        ))
        .bind(&readable)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .fetch_all(&self.db)
        .await?;
        let nodes: Vec<Node> = rows
            .into_iter()
            .map(row_to_node)
            .collect::<Result<_, _>>()?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::FileMetadataRead,
                    object_type: "file",
                    object_id: "",
                    decision: Decision::Allow,
                    trace_id: None,
                    metadata: serde_json::json!({
                        "purpose": purpose,
                        "node_ids": nodes.iter().map(|n| n.id).collect::<Vec<_>>(),
                    }),
                },
            )
            .await?;
        Ok(nodes)
    }
}
