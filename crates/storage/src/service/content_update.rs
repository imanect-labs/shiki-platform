//! StorageService: ノード ID 指定の内容更新（Task 11P.2・サーバ内バイト直書き）。
//!
//! ノート（md）保存のための「既知ノードへの新バージョン書込」。`write_file_at` の
//! update 分岐（(parent,name) 解決）と対称だが、こちらは**ノード ID で対象を固定**し、
//! 認可は **`editor@file`**（WOPI PutFile と同じ・ファイル自体の編集権）で判定する。
//! content-addressing・バージョン履歴・監査・書込イベント（RAG 再索引）は同一不変条件。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

use crate::content_address::sha256_hex;

impl StorageService {
    /// 既存ファイルの内容を新バージョンへ差し替える（内部書込・`editor@file` 認可）。
    ///
    /// 対象行を `FOR UPDATE` でロックし、並行保存の lost-update を防ぐ。rename/move とは
    /// 独立（名前・親は触らない）。返り値は更新後のノードメタ。
    pub async fn update_file_content_internal(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        bytes: &[u8],
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let size = i64::try_from(bytes.len())
            .map_err(|_| StorageError::Invalid("size が大きすぎます".into()))?;
        if size > self.max_upload_size {
            return Err(StorageError::Invalid(format!(
                "size が上限を超えています（最大 {} バイト）",
                self.max_upload_size
            )));
        }

        // ファイル自体の編集権（共同編集セッションと同じ relation・実行主体で判定）。
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().file(&file_id.to_string()),
            "file.write.content",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        // content-addressing: 所持バイトをハッシュし、新規 blob のみオブジェクトストアへ。
        let sha256 = sha256_hex(bytes);
        let final_key = blob_object_key(&ctx.tenant_id, &ctx.org, &sha256);
        let blob_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3)",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&sha256)
        .fetch_one(&self.db)
        .await?;
        if !blob_exists {
            self.store
                .put_object(&final_key, bytes.to_vec(), content_type)
                .await?;
        }

        let mut tx = self.db.begin().await?;
        self.bump_blob(
            &mut tx,
            &ctx.tenant_id,
            &ctx.org,
            &sha256,
            size,
            content_type,
            &final_key,
        )
        .await?;
        // 行ロック込みの UPDATE（finalize_content_update / write_file_at の update と同一形）。
        let sql = format!(
            "UPDATE node \
             SET blob_sha256 = $1, size_bytes = $2, content_type = $3, version = version + 1, updated_at = now() \
             WHERE id = $4 AND org = $5 AND tenant_id = $6 AND kind = 'file' AND deleted_at IS NULL \
             RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(&sha256)
            .bind(size)
            .bind(content_type)
            .bind(file_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;
        let node = row_to_node(row)?;

        self.record_version(
            &mut tx,
            ctx,
            node.id,
            node.version,
            &sha256,
            size,
            content_type,
        )
        .await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.write.content",
                object_type: "file",
                object_id: &node.id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "sha256": sha256, "size": size, "version": node.version }),
            },
            Chain::Yes,
        )
        .await?;
        event::emit_on(
            &mut tx,
            ctx,
            WriteEvent {
                node_id: node.id,
                version: node.version,
                op: WriteOp::Update,
                payload: json!({ "kind": "file", "blob_sha256": sha256, "size": size }),
            },
            trace_id,
        )
        .await?;
        tx.commit().await?;
        Ok(node)
    }
}
