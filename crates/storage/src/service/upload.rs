//! StorageService: アップロード declare/ダウンロードURL発行 と blob/version 補助。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

impl StorageService {
    /// declare: メタを申告し、staging への presigned PUT URL を得る。
    ///
    /// `target_node_id` が `Some` のときは**既存ファイルの内容更新（新版アップロード）**で、
    /// 認可は配置先ではなく対象ファイルの `editor@file` を要求し、親/名前は既存ノードを引き継ぐ。
    /// `None` のときは**新規ファイル作成**で、配置先（フォルダ or org ルート）の権限を確認する。
    ///
    /// 重複排除は **finalize 時**に行う（実バイトのアップロード＝所持証明の後）。declare で
    /// 宣言ハッシュだけを根拠に既存 blob へ短絡すると、内容を持たない同 org ユーザーが
    /// ハッシュを知るだけで他人のファイル内容を取得できてしまうため（所持証明前 dedup の禁止）。
    #[allow(clippy::too_many_arguments)] // 宣言メタ一式は凝集した 1 操作の引数。
    pub async fn begin_upload(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        content_type: &str,
        declared_sha256: &str,
        declared_size: i64,
        target_node_id: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<UploadTicket, StorageError> {
        if !is_valid_sha256_hex(declared_sha256) {
            return Err(StorageError::Invalid(
                "sha256 が 64 桁の hex ではありません".into(),
            ));
        }
        if declared_size < 0 {
            return Err(StorageError::Invalid("size が負です".into()));
        }
        if declared_size > self.max_upload_size {
            return Err(StorageError::Invalid(format!(
                "size が上限を超えています（最大 {} バイト）",
                self.max_upload_size
            )));
        }

        // 対象の解決と発行時認可。
        // - 内容更新: 対象ファイルの editor@file を要求し、親/名前は既存ノードから引く。
        // - 新規作成: 名前を検証し、ルート（parent なし）は member@org、フォルダ配下は editor@folder。
        let (effective_parent_id, effective_name) = if let Some(target) = target_node_id {
            let existing = self.load_node(ctx, target, false).await?;
            if existing.kind != NodeKind::File {
                return Err(StorageError::NotFound);
            }
            self.require(
                ctx,
                Relation::Editor,
                &ctx.ns().file(&target.to_string()),
                "file.upload_url.issue",
                "file",
                &target.to_string(),
                trace_id,
            )
            .await?;
            (existing.parent_id, existing.name)
        } else {
            validate_name(name)?;
            match parent_id {
                Some(p) => {
                    self.require(
                        ctx,
                        Relation::Editor,
                        &ctx.ns().folder(&p.to_string()),
                        "file.upload_url.issue",
                        "folder",
                        &p.to_string(),
                        trace_id,
                    )
                    .await?;
                    self.ensure_folder(ctx, p).await?;
                }
                None => {
                    self.require(
                        ctx,
                        Relation::Member,
                        &ctx.ns().organization(&ctx.org),
                        "file.upload_url.issue",
                        "organization",
                        &ctx.org,
                        trace_id,
                    )
                    .await?;
                }
            }
            (parent_id, name.to_string())
        };

        // staging への presigned PUT を発行し、pending_upload に控える。
        // 実体は finalize で content-addressed に昇格し、そこで dedup する。
        let upload_id = Uuid::new_v4();
        let staging_key = staging_object_key(&ctx.tenant_id, &ctx.org, &upload_id.to_string());
        sqlx::query(
            "INSERT INTO pending_upload \
             (upload_id, org, tenant_id, parent_id, name, content_type, declared_sha256, declared_size, staging_key, created_by, target_node_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(upload_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(effective_parent_id)
        .bind(&effective_name)
        .bind(content_type)
        .bind(declared_sha256)
        .bind(declared_size)
        .bind(&staging_key)
        .bind(&ctx.principal.id)
        .bind(target_node_id)
        .execute(&self.db)
        .await?;

        let upload_url = self
            .store
            .presign_put(&staging_key, self.presign_put_ttl, declared_size)
            .await?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "file.upload_url.issue",
                    object_type: "file",
                    object_id: &upload_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "staging_key": staging_key, "ttl_secs": self.presign_put_ttl.as_secs() }),
                },
            )
            .await?;
        Ok(UploadTicket {
            upload_id,
            upload_url,
        })
    }

    /// presigned GET URL を発行する（短 TTL）。
    pub async fn issue_download_url(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DownloadTicket, StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require_read(
            ctx,
            &ctx.ns().file(&file_id.to_string()),
            "file.download_url.issue",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        let sha = node.blob_sha256.as_ref().ok_or(StorageError::NotFound)?;
        let key = blob_object_key(&ctx.tenant_id, &ctx.org, sha);
        let url = self
            .store
            .presign_get(
                &key,
                self.presign_get_ttl,
                Some(&node.name),
                node.content_type.as_deref(),
            )
            .await?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "file.download_url.issue",
                    object_type: "file",
                    object_id: &file_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "ttl_secs": self.presign_get_ttl.as_secs() }),
                },
            )
            .await?;
        Ok(DownloadTicket {
            url,
            expires_in_secs: self.presign_get_ttl.as_secs(),
        })
    }

    /// blob の refcount を upsert で +1 する（新規は 1 で挿入）。tenant_id + org スコープ。
    #[allow(clippy::too_many_arguments)] // blob 行の identity + メタ一式。
    pub(crate) async fn bump_blob(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        org: &str,
        sha256: &str,
        size: i64,
        content_type: &str,
        object_key: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount) \
             VALUES ($1, $2, $3, $4, $5, $6, 1) \
             ON CONFLICT (tenant_id, org, sha256) DO UPDATE SET refcount = blob.refcount + 1",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(sha256)
        .bind(size)
        .bind(content_type)
        .bind(object_key)
        .execute(conn)
        .await?;
        Ok(())
    }

    /// ファイルノードを作成し、closure を整合させる（同一 txn 内で呼ぶ）。
    #[allow(clippy::too_many_arguments)] // ノード作成に必要なカラム一式。
    pub(crate) async fn create_file_node(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        sha256: &str,
        size: i64,
        content_type: &str,
    ) -> Result<Node, StorageError> {
        let sql = format!(
            "INSERT INTO node (org, tenant_id, kind, name, parent_id, blob_sha256, size_bytes, content_type, created_by, updated_by) \
             VALUES ($1, $2, 'file', $3, $4, $5, $6, $7, $8, $8) RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .bind(name)
            .bind(parent_id)
            .bind(sha256)
            .bind(size)
            .bind(content_type)
            .bind(&ctx.principal.id)
            .fetch_one(&mut *conn)
            .await?;
        let id = row.id;
        // 祖先リンク（親の closure を +1 で引き継ぐ）。
        if let Some(p) = parent_id {
            sqlx::query(
                "INSERT INTO node_closure (tenant_id, org, ancestor, descendant, depth) \
                 SELECT tenant_id, org, ancestor, $1, depth + 1 FROM node_closure \
                 WHERE tenant_id = $3 AND descendant = $2",
            )
            .bind(id)
            .bind(p)
            .bind(&ctx.tenant_id)
            .execute(&mut *conn)
            .await?;
        }
        // 自分自身（depth 0）。
        sqlx::query(
            "INSERT INTO node_closure (tenant_id, org, ancestor, descendant, depth) VALUES ($1, $2, $3, $3, 0)",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(id)
        .execute(&mut *conn)
        .await?;
        row_to_node(row)
    }

    /// 内容版を履歴（node_version）に 1 行記録する（同一 txn 内で呼ぶ・Task 1.7）。
    ///
    /// refcount = node_version 行数の規律のため、呼び出し側は版作成ごとに [`bump_blob`] を
    /// **1 回だけ**実行し、ここでは追加 bump しない（node.blob_sha256 は現在版への非正規化ポインタ）。
    ///
    /// [`bump_blob`]: Self::bump_blob
    #[allow(clippy::too_many_arguments)] // 版メタ一式は凝集した 1 操作の引数。
    pub(crate) async fn record_version(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        node_id: Uuid,
        version: i64,
        sha256: &str,
        size: i64,
        content_type: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO node_version \
             (node_id, version, org, tenant_id, blob_sha256, size_bytes, content_type, author) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(node_id)
        .bind(version)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(sha256)
        .bind(size)
        .bind(content_type)
        .bind(&ctx.principal.id)
        .execute(conn)
        .await?;
        Ok(())
    }
}
