//! StorageService: ゴミ箱: soft delete と一覧。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

impl StorageService {
    /// フォルダをサブツリーごと論理削除する（ゴミ箱）。
    ///
    /// closure の子孫（自身含む・生存分）を 1 txn でまとめて `deleted_at` する。refcount は
    /// soft-delete では減らさない（復元可能な間は本体を参照し続ける・LbvQZ と対称）。
    pub async fn soft_delete_folder(
        &self,
        ctx: &AuthContext,
        folder_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let node = self.load_node(ctx, folder_id, false).await?;
        if node.kind != NodeKind::Folder {
            return Err(StorageError::NotFound);
        }
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().folder(&folder_id.to_string()),
            "folder.delete",
            "folder",
            &folder_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        // サブツリー（自身含む）の生存ノードをまとめて論理削除する。version も進めて、各書込
        // イベントの冪等キー (node_id, version) が move/delete/restore 間で衝突しないようにする。
        let affected: Vec<(Uuid, i64)> = sqlx::query_as(
            "UPDATE node SET deleted_at = now(), updated_at = now(), version = version + 1 \
             WHERE org = $1 AND tenant_id = $2 AND deleted_at IS NULL \
               AND id IN (SELECT descendant FROM node_closure WHERE tenant_id = $2 AND ancestor = $3) \
             RETURNING id, version",
        )
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(folder_id)
        .fetch_all(&mut *tx)
        .await?;
        if affected.is_empty() {
            return Err(StorageError::NotFound);
        }
        // 進めた後のフォルダ自身の version（イベントに載せる）。
        let folder_version = affected
            .iter()
            .find(|(id, _)| *id == folder_id)
            .map(|(_, v)| *v)
            .ok_or(StorageError::NotFound)?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "folder.delete",
                object_type: "folder",
                object_id: &folder_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "subtree_count": affected.len() }),
            },
            Chain::Yes,
        )
        .await?;
        // 書込イベント（Task 1.8）。サブツリーは 1 操作 1 イベントに留め、購読側が node_closure
        // （soft-delete でも残る）で配下ファイルを展開して索引を除去する。
        event::emit_on(
            &mut tx,
            ctx,
            WriteEvent {
                node_id: folder_id,
                version: folder_version,
                op: WriteOp::Delete,
                payload: json!({
                    "kind": "folder",
                    "subtree_count": affected.len(),
                }),
            },
            trace_id,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 論理削除（ゴミ箱）。blob refcount を減らす。
    pub async fn soft_delete_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().file(&file_id.to_string()),
            "file.delete",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        // version も進めて書込イベントの冪等キー (node_id, version) を一意に保つ。
        let new_version: Option<i64> = sqlx::query_scalar(
            "UPDATE node SET deleted_at = now(), updated_at = now(), version = version + 1 \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL \
             RETURNING version",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(new_version) = new_version else {
            return Err(StorageError::NotFound);
        };
        // 論理削除（ゴミ箱）では blob refcount を**減らさない**。復元可能な間は実体を参照し続ける
        // ため、ここで減らすと将来の refcount GC が復元可能ファイルの本体を消し得る（LbvQZ）。
        // 減算は永続削除（ゴミ箱の完全削除・将来実装）でのみ行う。
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.delete",
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({}),
            },
            Chain::Yes,
        )
        .await?;
        // 書込イベント（Task 1.8）。購読側はベクタ/全文/メタを除去する。
        event::emit_on(
            &mut tx,
            ctx,
            WriteEvent {
                node_id: file_id,
                version: new_version,
                op: WriteOp::Delete,
                payload: json!({ "kind": "file" }),
            },
            trace_id,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// ゴミ箱一覧（削除の根ノードのみ）を新しい順に 1 ページ返す。
    ///
    /// 「削除の根」＝ `deleted_at` があり、かつ親が生存（または無い）ノード。フォルダ削除では
    /// サブツリーが丸ごと消えるが、ゴミ箱にはその根（フォルダ）だけを 1 件として見せる。
    /// 復元できる（editor）ものだけを post-filter で返す。keyset `(deleted_at, id)` 降順で
    /// ページングし、全件取得はしない。
    pub async fn list_trash(
        &self,
        ctx: &AuthContext,
        cursor: Option<&str>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<ChildPage, StorageError> {
        // ゴミ箱閲覧は org メンバーであること（root 列挙と同格）。
        self.require(
            ctx,
            Relation::Member,
            &ctx.ns().organization(&ctx.org),
            "trash.list",
            "organization",
            &ctx.org,
            trace_id,
        )
        .await?;

        let limit = limit.clamp(1, 100);
        let batch: i64 = (limit as i64 * 2).clamp(16, 200);
        let (mut after_ts, mut after_id) = match cursor {
            Some(c) => {
                let (ts, id) = decode_ts_cursor(c)?;
                (Some(ts), Some(id))
            }
            None => (None, None),
        };

        let mut items: Vec<Node> = Vec::with_capacity(limit);
        let mut exhausted = false;
        while items.len() < limit && !exhausted {
            // 削除の根: deleted_at あり ＆ 親が生存/無し。keyset は (deleted_at, id) 降順。
            let sql = format!(
                "SELECT {NODE_COLS} FROM node n \
                 WHERE n.org = $1 AND n.tenant_id = $2 AND n.deleted_at IS NOT NULL \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM node p WHERE p.id = n.parent_id AND p.deleted_at IS NOT NULL) \
                   AND ($3::text IS NULL OR (n.deleted_at, n.id) < ($3::timestamptz, $4)) \
                 ORDER BY n.deleted_at DESC, n.id DESC LIMIT $5"
            );
            let rows: Vec<NodeRow> = sqlx::query_as(&sql)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .bind(after_ts.as_deref())
                .bind(after_id)
                .bind(batch)
                .fetch_all(&self.db)
                .await?;
            if (rows.len() as i64) < batch {
                exhausted = true;
            }
            if rows.is_empty() {
                break;
            }
            for row in rows {
                after_ts = Some(row.deleted_at.map(|d| d.to_rfc3339()).unwrap_or_default());
                after_id = Some(row.id);
                // 復元可能（editor）なものだけ見せる。即時剥奪反映のため強整合。
                let kind = NodeKind::parse(&row.kind).unwrap_or(NodeKind::File);
                let allowed = self
                    .authz
                    .check(
                        &ctx.subject(),
                        Relation::Editor,
                        &node_fga_object(&ctx.ns(), kind, row.id),
                        Consistency::HigherConsistency,
                    )
                    .await?;
                if !allowed {
                    continue;
                }
                items.push(row_to_node(row)?);
                if items.len() == limit {
                    break;
                }
            }
        }
        let next_cursor = if items.len() == limit {
            match (after_ts, after_id) {
                (Some(ts), Some(i)) => Some(encode_ts_cursor(&ts, i)),
                _ => None,
            }
        } else {
            None
        };
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "trash.list",
                    object_type: "organization",
                    object_id: &ctx.org,
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "returned": items.len() }),
                },
            )
            .await?;
        Ok(ChildPage { items, next_cursor })
    }

    // --- バージョニング（Task 1.7） ---
}
