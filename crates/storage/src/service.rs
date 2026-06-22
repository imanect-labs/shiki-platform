//! StorageService — 権限・監査・content-addressing の単一チョークポイント（Task 1.3/1.4/1.9）。
//!
//! 不変条件:
//! - 全 read/write メソッドは第 1 引数に `&AuthContext` を取り、OpenFGA `check` を必ず通す。
//! - ハンドラに `db`/`store` を直接触らせない（このサービス経由でのみアクセス）。
//! - 各操作は allow/deny を監査ログに残す（書込系は同一 txn で原子的に）。
//! - バイトは presigned URL でクライアント↔MinIO 直転送し、アプリはメタ操作のみ（PIT-6）。

use std::{sync::Arc, time::Duration};

use authz::{AuthContext, AuthzClient, FgaObject, Relation, Subject};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry, AuditRecorder, Decision},
    content_address::{blob_object_key, is_valid_sha256_hex, staging_object_key},
    error::StorageError,
    model::{DownloadTicket, Node, NodeKind, UploadOutcome},
    object_store::{ObjectStore, ObjectStoreError},
};

/// `node` テーブルの選択カラム（NodeRow と一致させる）。
const NODE_COLS: &str = "id, org, tenant_id, kind, name, parent_id, blob_sha256, size_bytes, \
                         content_type, version, deleted_at, created_by, created_at, updated_at";

/// 単一チョークポイントの StorageService。
pub struct StorageService {
    db: PgPool,
    store: Arc<dyn ObjectStore>,
    authz: Arc<dyn AuthzClient>,
    audit: AuditRecorder,
    presign_get_ttl: Duration,
    presign_put_ttl: Duration,
}

#[derive(sqlx::FromRow)]
struct NodeRow {
    id: Uuid,
    org: String,
    tenant_id: String,
    kind: String,
    name: String,
    parent_id: Option<Uuid>,
    blob_sha256: Option<String>,
    size_bytes: Option<i64>,
    content_type: Option<String>,
    version: i64,
    deleted_at: Option<DateTime<Utc>>,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct PendingRow {
    parent_id: Option<Uuid>,
    name: String,
    content_type: String,
    declared_sha256: String,
    declared_size: i64,
    staging_key: String,
}

#[derive(sqlx::FromRow)]
struct BlobMeta {
    size_bytes: i64,
}

impl StorageService {
    pub fn new(
        db: PgPool,
        store: Arc<dyn ObjectStore>,
        authz: Arc<dyn AuthzClient>,
        presign_get_ttl: Duration,
        presign_put_ttl: Duration,
    ) -> Self {
        let audit = AuditRecorder::new(db.clone());
        StorageService {
            db,
            store,
            authz,
            audit,
            presign_get_ttl,
            presign_put_ttl,
        }
    }

    // --- アップロード（二相: declare → presigned PUT → finalize） ---

    /// declare: メタを申告し、dedup 短絡 or presigned PUT URL を得る。
    #[allow(clippy::too_many_arguments)] // 宣言メタ一式は凝集した 1 操作の引数。
    pub async fn begin_upload(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        content_type: &str,
        declared_sha256: &str,
        declared_size: i64,
        trace_id: Option<&str>,
    ) -> Result<UploadOutcome, StorageError> {
        validate_name(name)?;
        if !is_valid_sha256_hex(declared_sha256) {
            return Err(StorageError::Invalid(
                "sha256 が 64 桁の hex ではありません".into(),
            ));
        }
        if declared_size < 0 {
            return Err(StorageError::Invalid("size が負です".into()));
        }

        // 発行時認可。ルート（parent なし）は org メンバー、フォルダ配下は editor@folder。
        let object_label = parent_id.map(|p| p.to_string());
        let label_ref = object_label.as_deref().unwrap_or("root");
        match parent_id {
            Some(p) => {
                self.require(
                    ctx,
                    Relation::Editor,
                    &FgaObject::folder(&p.to_string()),
                    "file.upload_url.issue",
                    "folder",
                    label_ref,
                    trace_id,
                )
                .await?;
                self.ensure_folder(ctx, p).await?;
            }
            None => {
                self.require(
                    ctx,
                    Relation::Member,
                    &FgaObject::organization(&ctx.org),
                    "file.upload_url.issue",
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }

        // dedup: 同一 org・同一内容の blob があればアップロード不要。
        let existing: Option<BlobMeta> =
            sqlx::query_as("SELECT size_bytes FROM blob WHERE org = $1 AND sha256 = $2")
                .bind(&ctx.org)
                .bind(declared_sha256)
                .fetch_optional(&self.db)
                .await?;

        if let Some(blob) = existing {
            let final_key = blob_object_key(&ctx.org, declared_sha256);
            let mut tx = self.db.begin().await?;
            self.bump_blob(
                &mut tx,
                &ctx.org,
                declared_sha256,
                blob.size_bytes,
                content_type,
                &final_key,
            )
            .await?;
            let node = self
                .create_file_node(
                    &mut tx,
                    ctx,
                    parent_id,
                    name,
                    declared_sha256,
                    blob.size_bytes,
                    content_type,
                )
                .await?;
            audit::record_on(
                &mut tx,
                ctx,
                AuditEntry {
                    action: "file.upload.dedup",
                    object_type: "file",
                    object_id: &node.id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "sha256": declared_sha256, "deduplicated": true }),
                },
            )
            .await?;
            tx.commit().await?;
            self.write_file_tuples_or_compensate(ctx, node.id, parent_id)
                .await?;
            return Ok(UploadOutcome::Deduplicated(node));
        }

        // 未存在 → staging への presigned PUT を発行し、pending_upload に控える。
        let upload_id = Uuid::new_v4();
        let staging_key = staging_object_key(&ctx.org, &upload_id.to_string());
        sqlx::query(
            "INSERT INTO pending_upload \
             (upload_id, org, tenant_id, parent_id, name, content_type, declared_sha256, declared_size, staging_key, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(upload_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(parent_id)
        .bind(name)
        .bind(content_type)
        .bind(declared_sha256)
        .bind(declared_size)
        .bind(&staging_key)
        .bind(&ctx.principal.id)
        .execute(&self.db)
        .await?;

        let upload_url = self
            .store
            .presign_put(&staging_key, self.presign_put_ttl)
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
        Ok(UploadOutcome::NeedsUpload {
            upload_id,
            upload_url,
        })
    }

    /// finalize: staging を読み戻して内容ハッシュを検証し、content-addressed に昇格してノード化する。
    pub async fn finalize_upload(
        &self,
        ctx: &AuthContext,
        upload_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let pending: PendingRow = sqlx::query_as(
            "SELECT parent_id, name, content_type, declared_sha256, declared_size, staging_key \
             FROM pending_upload WHERE upload_id = $1 AND org = $2",
        )
        .bind(upload_id)
        .bind(&ctx.org)
        .fetch_optional(&self.db)
        .await?
        .ok_or(StorageError::NotFound)?;

        // finalize も認可を再確認（capability を持つだけでなく実権限も要る）。
        let label = upload_id.to_string();
        match pending.parent_id {
            Some(p) => {
                self.require(
                    ctx,
                    Relation::Editor,
                    &FgaObject::folder(&p.to_string()),
                    "file.upload.finalize",
                    "folder",
                    &p.to_string(),
                    trace_id,
                )
                .await?;
            }
            None => {
                self.require(
                    ctx,
                    Relation::Member,
                    &FgaObject::organization(&ctx.org),
                    "file.upload.finalize",
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }

        // staging を server-side で再ハッシュし、宣言値と照合（client バイトを信頼しない）。
        let (actual_sha, actual_size) = self.hash_staging(&pending.staging_key).await?;
        if actual_sha != pending.declared_sha256 || actual_size as i64 != pending.declared_size {
            let _ = self.store.delete(&pending.staging_key).await;
            let _ = sqlx::query("DELETE FROM pending_upload WHERE upload_id = $1")
                .bind(upload_id)
                .execute(&self.db)
                .await;
            return Err(StorageError::Integrity(format!(
                "宣言ハッシュ/サイズと実体が一致しません (label={label})"
            )));
        }

        // content-addressed キーへ昇格（server-side copy・バイトはアプリを通らない）。
        let final_key = blob_object_key(&ctx.org, &actual_sha);
        self.store.copy(&pending.staging_key, &final_key).await?;

        let mut tx = self.db.begin().await?;
        self.bump_blob(
            &mut tx,
            &ctx.org,
            &actual_sha,
            actual_size as i64,
            &pending.content_type,
            &final_key,
        )
        .await?;
        let node = self
            .create_file_node(
                &mut tx,
                ctx,
                pending.parent_id,
                &pending.name,
                &actual_sha,
                actual_size as i64,
                &pending.content_type,
            )
            .await?;
        sqlx::query("DELETE FROM pending_upload WHERE upload_id = $1")
            .bind(upload_id)
            .execute(&mut *tx)
            .await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.upload.finalize",
                object_type: "file",
                object_id: &node.id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "sha256": actual_sha, "size": actual_size }),
            },
        )
        .await?;
        tx.commit().await?;

        self.write_file_tuples_or_compensate(ctx, node.id, pending.parent_id)
            .await?;
        let _ = self.store.delete(&pending.staging_key).await; // best-effort 後始末
        Ok(node)
    }

    // --- ダウンロード / メタ ---

    /// presigned GET URL を発行する（短 TTL）。
    pub async fn issue_download_url(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DownloadTicket, StorageError> {
        let node = self.load_node(&ctx.org, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require(
            ctx,
            Relation::Viewer,
            &FgaObject::file(&file_id.to_string()),
            "file.download_url.issue",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        let sha = node.blob_sha256.as_ref().ok_or(StorageError::NotFound)?;
        let key = blob_object_key(&ctx.org, sha);
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

    /// ファイルメタを取得する（viewer 権限が要る）。
    pub async fn get_metadata(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let node = self.load_node(&ctx.org, file_id, false).await?;
        self.require(
            ctx,
            Relation::Viewer,
            &FgaObject::file(&file_id.to_string()),
            "file.metadata.read",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "file.metadata.read",
                    object_type: "file",
                    object_id: &file_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({}),
                },
            )
            .await?;
        Ok(node)
    }

    // --- 変更系 ---

    /// リネーム（editor 権限・同名衝突は Conflict）。
    pub async fn rename_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_name: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        validate_name(new_name)?;
        let _ = self.load_node(&ctx.org, file_id, false).await?;
        self.require(
            ctx,
            Relation::Editor,
            &FgaObject::file(&file_id.to_string()),
            "file.rename",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        let sql = format!(
            "UPDATE node SET name = $1, updated_at = now() \
             WHERE id = $2 AND org = $3 AND deleted_at IS NULL RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(new_name)
            .bind(file_id)
            .bind(&ctx.org)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.rename",
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "new_name": new_name }),
            },
        )
        .await?;
        tx.commit().await?;
        row_to_node(row)
    }

    /// 移動（editor@file かつ 移動先 editor/member・PIT-16: 単一 txn ＋ 祖先ロック）。
    pub async fn move_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_parent: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let node = self.load_node(&ctx.org, file_id, false).await?;
        let file_obj = FgaObject::file(&file_id.to_string());
        self.require(
            ctx,
            Relation::Editor,
            &file_obj,
            "file.move",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        // 移動先の権限。
        match new_parent {
            Some(p) => {
                self.require(
                    ctx,
                    Relation::Editor,
                    &FgaObject::folder(&p.to_string()),
                    "file.move",
                    "folder",
                    &p.to_string(),
                    trace_id,
                )
                .await?;
            }
            None => {
                self.require(
                    ctx,
                    Relation::Member,
                    &FgaObject::organization(&ctx.org),
                    "file.move",
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }
        if let Some(p) = new_parent {
            if p == file_id {
                return Err(StorageError::Invalid("自分自身へは移動できません".into()));
            }
            self.ensure_folder(ctx, p).await?;
        }

        let old_parent = node.parent_id;
        let mut tx = self.db.begin().await?;
        // PIT-16: 関係ノードを id 昇順でロック（デッドロック回避）。
        let mut lock_ids = vec![file_id];
        if let Some(p) = new_parent {
            lock_ids.push(p);
        }
        sqlx::query("SELECT id FROM node WHERE id = ANY($1) ORDER BY id FOR UPDATE")
            .bind(&lock_ids)
            .fetch_all(&mut *tx)
            .await?;

        let sql = format!(
            "UPDATE node SET parent_id = $1, updated_at = now() \
             WHERE id = $2 AND org = $3 AND deleted_at IS NULL RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(new_parent)
            .bind(file_id)
            .bind(&ctx.org)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;

        // closure 書換: 祖先リンクを消し（自分自身は残す）、新親から張り直す。
        sqlx::query("DELETE FROM node_closure WHERE descendant = $1 AND ancestor <> $1")
            .bind(file_id)
            .execute(&mut *tx)
            .await?;
        if let Some(p) = new_parent {
            sqlx::query(
                "INSERT INTO node_closure (org, ancestor, descendant, depth) \
                 SELECT org, ancestor, $1, depth + 1 FROM node_closure WHERE descendant = $2",
            )
            .bind(file_id)
            .bind(p)
            .execute(&mut *tx)
            .await?;
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.move",
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "old_parent": old_parent.map(|p| p.to_string()),
                    "new_parent": new_parent.map(|p| p.to_string()),
                }),
            },
        )
        .await?;
        tx.commit().await?;

        // OpenFGA の parent タプルを更新（commit 後）。
        if let Some(op) = old_parent {
            let _ = self
                .authz
                .delete_tuple(
                    &Subject::object(&FgaObject::folder(&op.to_string())),
                    Relation::Parent,
                    &file_obj,
                )
                .await;
        }
        if let Some(np) = new_parent {
            self.authz
                .write_tuple(
                    &Subject::object(&FgaObject::folder(&np.to_string())),
                    Relation::Parent,
                    &file_obj,
                )
                .await?;
        }
        row_to_node(row)
    }

    /// 論理削除（ゴミ箱）。blob refcount を減らす。
    pub async fn soft_delete_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let node = self.load_node(&ctx.org, file_id, false).await?;
        self.require(
            ctx,
            Relation::Editor,
            &FgaObject::file(&file_id.to_string()),
            "file.delete",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        let res = sqlx::query(
            "UPDATE node SET deleted_at = now(), updated_at = now() \
             WHERE id = $1 AND org = $2 AND deleted_at IS NULL",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        if let Some(sha) = &node.blob_sha256 {
            sqlx::query("UPDATE blob SET refcount = refcount - 1 WHERE org = $1 AND sha256 = $2 AND refcount > 0")
                .bind(&ctx.org)
                .bind(sha)
                .execute(&mut *tx)
                .await?;
        }
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
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// ゴミ箱からの復元（editor 権限・同名衝突は Conflict）。
    pub async fn restore_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let node = self.load_node(&ctx.org, file_id, true).await?;
        self.require(
            ctx,
            Relation::Editor,
            &FgaObject::file(&file_id.to_string()),
            "file.restore",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        // deleted_at=NULL に戻す。生存兄弟と同名なら部分ユニークが効き Conflict になる。
        let sql = format!(
            "UPDATE node SET deleted_at = NULL, updated_at = now() \
             WHERE id = $1 AND org = $2 AND deleted_at IS NOT NULL RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(file_id)
            .bind(&ctx.org)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;
        if let Some(sha) = &node.blob_sha256 {
            sqlx::query("UPDATE blob SET refcount = refcount + 1 WHERE org = $1 AND sha256 = $2")
                .bind(&ctx.org)
                .bind(sha)
                .execute(&mut *tx)
                .await?;
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.restore",
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({}),
            },
        )
        .await?;
        tx.commit().await?;
        row_to_node(row)
    }

    // --- 内部ヘルパ ---

    /// 認可 check（deny は監査して Forbidden）。
    #[allow(clippy::too_many_arguments)] // check + 監査記録に必要なフィールド一式。
    async fn require(
        &self,
        ctx: &AuthContext,
        relation: Relation,
        object: &FgaObject,
        action: &str,
        object_type: &str,
        object_id: &str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let allowed = self.authz.check(&ctx.subject(), relation, object).await?;
        if !allowed {
            self.audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type,
                        object_id,
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": relation.as_str() }),
                    },
                )
                .await?;
            return Err(StorageError::Forbidden);
        }
        Ok(())
    }

    /// 親が存在する生存フォルダであることを確認する。
    async fn ensure_folder(&self, ctx: &AuthContext, id: Uuid) -> Result<(), StorageError> {
        let kind: Option<String> = sqlx::query_scalar(
            "SELECT kind FROM node WHERE id = $1 AND org = $2 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(&ctx.org)
        .fetch_optional(&self.db)
        .await?;
        match kind.as_deref() {
            Some("folder") => Ok(()),
            Some(_) => Err(StorageError::Invalid("親がフォルダではありません".into())),
            None => Err(StorageError::NotFound),
        }
    }

    /// blob の refcount を upsert で +1 する（新規は 1 で挿入）。
    async fn bump_blob(
        &self,
        conn: &mut PgConnection,
        org: &str,
        sha256: &str,
        size: i64,
        content_type: &str,
        object_key: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO blob (org, sha256, size_bytes, content_type, object_key, refcount) \
             VALUES ($1, $2, $3, $4, $5, 1) \
             ON CONFLICT (org, sha256) DO UPDATE SET refcount = blob.refcount + 1",
        )
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
    async fn create_file_node(
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
            "INSERT INTO node (org, tenant_id, kind, name, parent_id, blob_sha256, size_bytes, content_type, created_by) \
             VALUES ($1, $2, 'file', $3, $4, $5, $6, $7, $8) RETURNING {NODE_COLS}"
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
                "INSERT INTO node_closure (org, ancestor, descendant, depth) \
                 SELECT org, ancestor, $1, depth + 1 FROM node_closure WHERE descendant = $2",
            )
            .bind(id)
            .bind(p)
            .execute(&mut *conn)
            .await?;
        }
        // 自分自身（depth 0）。
        sqlx::query(
            "INSERT INTO node_closure (org, ancestor, descendant, depth) VALUES ($1, $2, $2, 0)",
        )
        .bind(&ctx.org)
        .bind(id)
        .execute(&mut *conn)
        .await?;
        row_to_node(row)
    }

    /// owner/parent タプルを書き込む。失敗時は補償的にノードを論理削除する（R2）。
    async fn write_file_tuples_or_compensate(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        parent_id: Option<Uuid>,
    ) -> Result<(), StorageError> {
        let file_obj = FgaObject::file(&file_id.to_string());
        let result: Result<(), authz::AuthzError> = async {
            self.authz
                .write_tuple(&ctx.subject(), Relation::Owner, &file_obj)
                .await?;
            if let Some(p) = parent_id {
                self.authz
                    .write_tuple(
                        &Subject::object(&FgaObject::folder(&p.to_string())),
                        Relation::Parent,
                        &file_obj,
                    )
                    .await?;
            }
            Ok(())
        }
        .await;

        if let Err(e) = result {
            tracing::error!(error = %e, %file_id, "owner/parent タプル書込に失敗。ノードを補償削除");
            let _ = sqlx::query("UPDATE node SET deleted_at = now() WHERE id = $1")
                .bind(file_id)
                .execute(&self.db)
                .await;
            return Err(StorageError::Authz(e));
        }
        Ok(())
    }

    /// staging オブジェクトを server-side で再ハッシュして `(sha256, size)` を返す。
    async fn hash_staging(&self, staging_key: &str) -> Result<(String, u64), StorageError> {
        match self.store.read_and_hash(staging_key).await {
            Ok(digest) => Ok(digest),
            Err(ObjectStoreError::NotFound(_)) => Err(StorageError::Integrity(
                "staging オブジェクトが存在しません（アップロード未完了）".into(),
            )),
            Err(e) => Err(e.into()),
        }
    }

    /// org スコープでノードを 1 件読む。
    async fn load_node(
        &self,
        org: &str,
        id: Uuid,
        include_deleted: bool,
    ) -> Result<Node, StorageError> {
        let sql = if include_deleted {
            format!("SELECT {NODE_COLS} FROM node WHERE id = $1 AND org = $2")
        } else {
            format!(
                "SELECT {NODE_COLS} FROM node WHERE id = $1 AND org = $2 AND deleted_at IS NULL"
            )
        };
        let row: Option<NodeRow> = sqlx::query_as(&sql)
            .bind(id)
            .bind(org)
            .fetch_optional(&self.db)
            .await?;
        row.map(row_to_node)
            .transpose()?
            .ok_or(StorageError::NotFound)
    }
}

fn row_to_node(row: NodeRow) -> Result<Node, StorageError> {
    let kind = NodeKind::parse(&row.kind)
        .ok_or_else(|| StorageError::Integrity(format!("未知のノード種別: {}", row.kind)))?;
    Ok(Node {
        id: row.id,
        org: row.org,
        tenant_id: row.tenant_id,
        kind,
        name: row.name,
        parent_id: row.parent_id,
        blob_sha256: row.blob_sha256,
        size_bytes: row.size_bytes,
        content_type: row.content_type,
        version: row.version,
        deleted_at: row.deleted_at,
        created_by: row.created_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// ノード名の検証（空・長すぎ・パス区切り/NUL を拒否）。
fn validate_name(name: &str) -> Result<(), StorageError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(StorageError::Invalid("名前が空です".into()));
    }
    if name.chars().count() > 255 {
        return Err(StorageError::Invalid(
            "名前が長すぎます（255 文字以内）".into(),
        ));
    }
    if name.contains('/') || name.contains('\0') {
        return Err(StorageError::Invalid("名前に / や NUL は使えません".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_rejects_bad_inputs() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(&"x".repeat(256)).is_err());
        assert!(validate_name("report.pdf").is_ok());
        assert!(validate_name("日本語.txt").is_ok());
    }
}
