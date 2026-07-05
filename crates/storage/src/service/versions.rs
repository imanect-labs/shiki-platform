//! StorageService: バージョン一覧・バージョンDLURL発行。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

impl StorageService {
    /// ファイルの版履歴を新しい順に返す（viewer 権限）。
    ///
    /// 内容を持つ版（初版アップロード / 内容更新 / 版復元）だけが並ぶ。同一内容の版は
    /// `blob_sha256` を共有する（content-addressing）。
    pub async fn list_versions(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        cursor: Option<&str>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<(Vec<FileVersion>, Option<String>), StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require_read(
            ctx,
            &ctx.ns().file(&file_id.to_string()),
            "file.versions.list",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        let limit = limit.clamp(1, 100);
        // 版番号は単調増加。カーソルは「この版より前（小さい）」を引く keyset（新しい順）。
        let before: Option<i64> = match cursor {
            Some(c) => Some(
                c.parse::<i64>()
                    .map_err(|_| StorageError::Invalid("カーソルが不正です".into()))?,
            ),
            None => None,
        };
        let rows: Vec<VersionRow> = sqlx::query_as(
            "SELECT tenant_id, version, blob_sha256, size_bytes, content_type, author, created_at \
             FROM node_version \
             WHERE node_id = $1 AND org = $2 AND tenant_id = $3 \
               AND ($4::bigint IS NULL OR version < $4) \
             ORDER BY version DESC LIMIT $5",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(before)
        .bind(limit as i64)
        .fetch_all(&self.db)
        .await?;
        let next_cursor = if rows.len() == limit {
            rows.last().map(|r| r.version.to_string())
        } else {
            None
        };
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "file.versions.list",
                    object_type: "file",
                    object_id: &file_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "count": rows.len() }),
                },
            )
            .await?;
        let items = rows.into_iter().map(VersionRow::into_model).collect();
        Ok((items, next_cursor))
    }

    /// 特定版の presigned ダウンロード URL を発行する（viewer 権限・短 TTL）。
    pub async fn issue_version_download_url(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<DownloadTicket, StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require_read(
            ctx,
            &ctx.ns().file(&file_id.to_string()),
            "file.version.download_url.issue",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        // 対象版の blob を解決する（無い版番号は存在秘匿の NotFound）。
        let version_row: Option<(String, String)> = sqlx::query_as(
            "SELECT blob_sha256, content_type FROM node_version \
             WHERE node_id = $1 AND org = $2 AND tenant_id = $3 AND version = $4",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(version)
        .fetch_optional(&self.db)
        .await?;
        let (sha, content_type) = version_row.ok_or(StorageError::NotFound)?;
        let key = blob_object_key(&ctx.tenant_id, &ctx.org, &sha);
        let url = self
            .store
            .presign_get(
                &key,
                self.presign_get_ttl,
                Some(&node.name),
                Some(&content_type),
            )
            .await?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "file.version.download_url.issue",
                    object_type: "file",
                    object_id: &file_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "version": version, "ttl_secs": self.presign_get_ttl.as_secs() }),
                },
            )
            .await?;
        Ok(DownloadTicket {
            url,
            expires_in_secs: self.presign_get_ttl.as_secs(),
        })
    }
}
