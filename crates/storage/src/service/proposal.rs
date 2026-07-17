//! StorageService: 提案バージョン（Task 11.8・PIT-44）。
//!
//! WOPI 編集セッション中の AI 編集は current を進めず「提案」として履歴にだけ積む。
//! - `propose_file_content_internal`: node 行を触らず `node_version` に is_proposal 行を追加。
//!   書込イベント outbox を**発火しない**（＝RAG 再索引の対象外）。
//! - `adopt_proposal_version`: editor が提案を採用し、通常の新バージョンへ複製する
//!   （このとき初めて outbox が流れ、RAG 再索引に乗る）。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

use crate::content_address::sha256_hex;

impl StorageService {
    /// 編集セッション中の AI 編集結果を提案バージョンとして保存する（`editor@file` 認可）。
    ///
    /// current（`node.version`・`node.blob_sha256`）は進めない。版番号は
    /// [`NEXT_CONTENT_VERSION`] と同じ「current と履歴最大値の大きい方 + 1」で採番し、
    /// 以後の通常版・提案と衝突しない。返り値は作成した提案の [`FileVersion`]。
    pub async fn propose_file_content_internal(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        bytes: &[u8],
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<FileVersion, StorageError> {
        let size = i64::try_from(bytes.len())
            .map_err(|_| StorageError::Invalid("size が大きすぎます".into()))?;
        if size > self.max_upload_size {
            return Err(StorageError::Invalid(format!(
                "size が上限を超えています（最大 {} バイト）",
                self.max_upload_size
            )));
        }
        // 通常の内容更新（update_file_content_internal）と同じ編集権で判定する。
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().file(&file_id.to_string()),
            "file.write.propose",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        // content-addressing: 新規 blob のみオブジェクトストアへ（update と同一手順）。
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
        // 対象ファイルを行ロックし、採番（MAX(version)）を並行書込と直列化する。
        let node_version: Option<(i64,)> = sqlx::query_as(
            "SELECT version FROM node \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND kind = 'file' AND deleted_at IS NULL \
             FOR UPDATE",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (current,) = node_version.ok_or(StorageError::NotFound)?;
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
        let row: VersionRow = sqlx::query_as(
            "INSERT INTO node_version \
             (node_id, version, org, tenant_id, blob_sha256, size_bytes, content_type, author, \
              is_proposal, proposed_by) \
             SELECT $1, GREATEST($2, COALESCE(MAX(version), 0)) + 1, $3, $4, $5, $6, $7, $8, \
                    TRUE, $8 \
             FROM node_version WHERE node_id = $1 \
             RETURNING tenant_id, version, blob_sha256, size_bytes, content_type, author, \
                       created_at, is_proposal, proposed_by",
        )
        .bind(file_id)
        .bind(current)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(&sha256)
        .bind(size)
        .bind(content_type)
        .bind(&ctx.principal.id)
        .fetch_one(&mut *tx)
        .await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.write.propose",
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "sha256": sha256, "size": size, "version": row.version }),
            },
            Chain::Yes,
        )
        .await?;
        // 書込イベントは発火しない（提案は current でも RAG 索引対象でもない・PIT-44）。
        tx.commit().await?;
        Ok(row.into_model())
    }

    /// 提案バージョンを採用し、通常の新バージョンへ昇格する（`editor@file` 認可）。
    ///
    /// 提案の blob を参照する新しい通常版を作成して current を進める（提案行自体は
    /// 履歴として残す）。このとき初めて書込イベント outbox → RAG 再索引が流れる。
    pub async fn adopt_proposal_version(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().file(&file_id.to_string()),
            "file.version.adopt",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        // 対象ファイルを行ロックして並行更新と直列化する。
        sqlx::query(
            "SELECT id FROM node \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND kind = 'file' AND deleted_at IS NULL \
             FOR UPDATE",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StorageError::NotFound)?;
        // 採用対象は提案バージョンのみ（通常版の複製は restore_version の役割）。
        let src: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT blob_sha256, size_bytes, content_type FROM node_version \
             WHERE node_id = $1 AND org = $2 AND tenant_id = $3 AND version = $4 AND is_proposal",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(version)
        .fetch_optional(&mut *tx)
        .await?;
        let (sha, size, content_type) = src.ok_or(StorageError::NotFound)?;
        // 新しい通常版が同じ blob を参照する（refcount +1・実体コピーなし。restore と同型）。
        let final_key = blob_object_key(&ctx.tenant_id, &ctx.org, &sha);
        self.bump_blob(
            &mut tx,
            &ctx.tenant_id,
            &ctx.org,
            &sha,
            size,
            &content_type,
            &final_key,
        )
        .await?;
        let sql = format!(
            "UPDATE node \
             SET blob_sha256 = $1, size_bytes = $2, content_type = $3, \
             version = {NEXT_CONTENT_VERSION}, \
             updated_by = $7, updated_at = now() \
             WHERE id = $4 AND org = $5 AND tenant_id = $6 AND kind = 'file' AND deleted_at IS NULL \
             RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(&sha)
            .bind(size)
            .bind(&content_type)
            .bind(file_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .bind(&ctx.principal.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;
        let node = row_to_node(row)?;
        self.record_version(
            &mut tx,
            ctx,
            node.id,
            node.version,
            &sha,
            size,
            &content_type,
        )
        .await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.version.adopt",
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "adopted_from_version": version,
                    "sha256": sha,
                    "version": node.version,
                }),
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
                payload: json!({
                    "kind": "file",
                    "blob_sha256": sha,
                    "size": size,
                    "adopted_from_version": version,
                }),
            },
            trace_id,
        )
        .await?;
        tx.commit().await?;
        Ok(node)
    }
}
