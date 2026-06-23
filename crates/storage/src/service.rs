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
    content_address::{
        blob_object_key, incoming_object_key, is_valid_sha256_hex, staging_object_key,
    },
    error::StorageError,
    model::{DownloadTicket, Node, NodeKind, UploadTicket},
    object_store::ObjectStore,
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

    /// declare: メタを申告し、staging への presigned PUT URL を得る。
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
        trace_id: Option<&str>,
    ) -> Result<UploadTicket, StorageError> {
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

        // staging への presigned PUT を発行し、pending_upload に控える。
        // 実体は finalize で content-addressed に昇格し、そこで dedup する。
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

    /// finalize: staging を読み戻して内容ハッシュを検証し、content-addressed に昇格してノード化する。
    pub async fn finalize_upload(
        &self,
        ctx: &AuthContext,
        upload_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        // 所有者束縛: アップロードを宣言した本人のみ finalize できる（upload_id 漏洩での横取り防止）。
        // tenant_id も条件に含め、同一 org 内でも tenant 跨ぎを遮断する。
        let pending: PendingRow = sqlx::query_as(
            "SELECT parent_id, name, content_type, declared_sha256, declared_size, staging_key \
             FROM pending_upload \
             WHERE upload_id = $1 AND org = $2 AND tenant_id = $3 AND created_by = $4",
        )
        .bind(upload_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
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
                // declare 後に親が削除/変更され得るため、生存フォルダであることを再確認する。
                self.ensure_folder(ctx, p).await?;
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

        // TOCTOU 回避: staging はクライアントが presigned PUT で上書きでき得るため、
        // 不変な incoming へ server-side copy し、以降の検証・昇格は incoming 基準で行う。
        if !self.store.exists(&pending.staging_key).await? {
            return Err(StorageError::Integrity(format!(
                "staging オブジェクトが存在しません（アップロード未完了 label={label}）"
            )));
        }
        let incoming_key = incoming_object_key(&ctx.org, &label);
        self.store.copy(&pending.staging_key, &incoming_key).await?;

        // 不変スナップショットを再ハッシュし、宣言値と照合（client バイトを信頼しない）。
        let (actual_sha, actual_size) = self.store.read_and_hash(&incoming_key).await?;
        if actual_sha != pending.declared_sha256 || actual_size as i64 != pending.declared_size {
            let _ = self.store.delete(&incoming_key).await;
            let _ = self.store.delete(&pending.staging_key).await;
            let _ = sqlx::query("DELETE FROM pending_upload WHERE upload_id = $1")
                .bind(upload_id)
                .execute(&self.db)
                .await;
            return Err(StorageError::Integrity(format!(
                "宣言ハッシュ/サイズと実体が一致しません (label={label})"
            )));
        }

        // 既存の有効 blob を上書きしない（content-addressed への昇格は新規 blob の時だけ）。
        // 既存 blob があるなら finalize は実バイトを所持した上での正当な dedup。
        let final_key = blob_object_key(&ctx.org, &actual_sha);
        let blob_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM blob WHERE org = $1 AND sha256 = $2)")
                .bind(&ctx.org)
                .bind(&actual_sha)
                .fetch_one(&self.db)
                .await?;
        if !blob_exists {
            // incoming は不変なので、final へのコピーは宣言ハッシュと必ず一致する。
            // 既存 blob があるなら上書きしない（並行 finalize が参照する共有本体を壊さない）。
            if let Err(e) = self.store.copy(&incoming_key, &final_key).await {
                let _ = self.store.delete(&incoming_key).await;
                return Err(e.into());
            }
        }

        // メタ確定（blob upsert + node + FGA tuple + pending 削除 + 監査）を 1 txn 境界で行う。
        // FGA tuple は **commit 前**に書き、parent 失敗・commit 失敗のどちらでも書けた tuple を
        // revoke して DB/FGA の不整合（auth tuple 欠落・owner 残留）を残さない。
        let tx_result: Result<Node, StorageError> = async {
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
            let file_obj = FgaObject::file(&node.id.to_string());
            // owner tuple（失敗時は tx を drop でロールバック＝何も残らない）。
            self.authz
                .write_tuple(&ctx.subject(), Relation::Owner, &file_obj)
                .await
                .map_err(StorageError::Authz)?;
            // parent tuple（folder 配下のみ）。失敗時は owner を revoke してロールバック。
            if let Some(p) = pending.parent_id {
                if let Err(e) = self
                    .authz
                    .write_tuple(
                        &Subject::object(&FgaObject::folder(&p.to_string())),
                        Relation::Parent,
                        &file_obj,
                    )
                    .await
                {
                    let _ = self
                        .authz
                        .delete_tuple(&ctx.subject(), Relation::Owner, &file_obj)
                        .await;
                    return Err(StorageError::Authz(e));
                }
            }
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
            // commit 失敗時は書いた owner/parent tuple を revoke して FGA を作成前へ戻す。
            if let Err(e) = tx.commit().await {
                let _ = self
                    .authz
                    .delete_tuple(&ctx.subject(), Relation::Owner, &file_obj)
                    .await;
                if let Some(p) = pending.parent_id {
                    let _ = self
                        .authz
                        .delete_tuple(
                            &Subject::object(&FgaObject::folder(&p.to_string())),
                            Relation::Parent,
                            &file_obj,
                        )
                        .await;
                }
                return Err(StorageError::from(e));
            }
            Ok(node)
        }
        .await;

        let node = match tx_result {
            Ok(node) => node,
            Err(e) => {
                // upload 固有のオブジェクトのみ掃除する。共有の content-addressed `final_key` は
                // **消さない**（並行 finalize が commit 済みの blob で参照し得るため）。参照ゼロの
                // 孤児本体は refcount ベース GC（後続）に委ねる。
                let _ = self.store.delete(&incoming_key).await;
                let _ = self.store.delete(&pending.staging_key).await;
                let _ = sqlx::query("DELETE FROM pending_upload WHERE upload_id = $1")
                    .bind(upload_id)
                    .execute(&self.db)
                    .await;
                return Err(e);
            }
        };

        let _ = self.store.delete(&incoming_key).await; // best-effort 後始末（final は残す）
        let _ = self.store.delete(&pending.staging_key).await;
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
        let node = self.load_node(ctx, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require_read(
            ctx,
            &FgaObject::file(&file_id.to_string()),
            "file.download_url.issue",
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
        let node = self.load_node(ctx, file_id, false).await?;
        self.require_read(
            ctx,
            &FgaObject::file(&file_id.to_string()),
            "file.metadata.read",
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

    /// リネーム・移動を **1 トランザクションで原子的に**適用する。
    ///
    /// `new_name`: 指定でリネーム。`new_parent`: `Some(Some(p))` で `p` 配下へ、
    /// `Some(None)` でルートへ移動、`None` で移動しない。move と rename を一度に指定しても
    /// 部分適用にならない（PIT-16: 関係ノードを祖先ロック下の単一 txn で更新）。
    pub async fn update_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        if new_name.is_none() && new_parent.is_none() {
            return Err(StorageError::Invalid("変更内容がありません".into()));
        }
        if let Some(name) = new_name {
            validate_name(name)?;
        }
        let node = self.load_node(ctx, file_id, false).await?;
        let file_obj = FgaObject::file(&file_id.to_string());
        // 対象ファイルの editor 権限。
        self.require(
            ctx,
            Relation::Editor,
            &file_obj,
            "file.update",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        // 移動する場合は移動先の権限＋実在を確認。
        if let Some(target) = new_parent {
            match target {
                Some(p) => {
                    if p == file_id {
                        return Err(StorageError::Invalid("自分自身へは移動できません".into()));
                    }
                    self.require(
                        ctx,
                        Relation::Editor,
                        &FgaObject::folder(&p.to_string()),
                        "file.update",
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
                        &FgaObject::organization(&ctx.org),
                        "file.update",
                        "organization",
                        &ctx.org,
                        trace_id,
                    )
                    .await?;
                }
            }
        }

        let old_parent = node.parent_id;
        let final_parent = match new_parent {
            Some(target) => target,
            None => node.parent_id,
        };
        let final_name = new_name.unwrap_or(node.name.as_str());
        let parent_changed = new_parent.is_some() && final_parent != old_parent;

        let mut tx = self.db.begin().await?;
        // PIT-16: 関係ノードを id 昇順でロック（デッドロック回避）。
        let mut lock_ids = vec![file_id];
        if let Some(Some(p)) = new_parent {
            lock_ids.push(p);
        }
        sqlx::query(
            "SELECT id FROM node \
             WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 ORDER BY id FOR UPDATE",
        )
        .bind(&lock_ids)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_all(&mut *tx)
        .await?;

        let sql = format!(
            "UPDATE node SET name = $1, parent_id = $2, updated_at = now() \
             WHERE id = $3 AND org = $4 AND tenant_id = $5 AND deleted_at IS NULL RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(final_name)
            .bind(final_parent)
            .bind(file_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;

        // closure 書換（親が変わった時のみ）: 祖先リンクを消し（自分自身は残す）、新親から張り直す。
        if parent_changed {
            sqlx::query("DELETE FROM node_closure WHERE descendant = $1 AND ancestor <> $1")
                .bind(file_id)
                .execute(&mut *tx)
                .await?;
            if let Some(p) = final_parent {
                sqlx::query(
                    "INSERT INTO node_closure (org, ancestor, descendant, depth) \
                     SELECT org, ancestor, $1, depth + 1 FROM node_closure WHERE descendant = $2",
                )
                .bind(file_id)
                .bind(p)
                .execute(&mut *tx)
                .await?;
            }
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.update",
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "renamed": new_name.is_some(),
                    "moved": parent_changed,
                    "old_parent": old_parent.map(|p| p.to_string()),
                    "new_parent": final_parent.map(|p| p.to_string()),
                }),
            },
        )
        .await?;
        // OpenFGA の parent タプルは **DB コミット前**に更新し、失敗時は txn をロールバックして
        // DB とタプルの不整合（旧親経由の漏れ／新親で到達不能）を防ぐ。ロックは保持したまま。
        if parent_changed {
            // 旧親を先に剥奪 → 新親を付与（途中失敗でも over-permissive にしない）。
            if let Some(op) = old_parent {
                self.authz
                    .delete_tuple(
                        &Subject::object(&FgaObject::folder(&op.to_string())),
                        Relation::Parent,
                        &file_obj,
                    )
                    .await?; // 失敗 → tx は drop でロールバック（移動なし＝整合）
            }
            if let Some(np) = final_parent {
                if let Err(e) = self
                    .authz
                    .write_tuple(
                        &Subject::object(&FgaObject::folder(&np.to_string())),
                        Relation::Parent,
                        &file_obj,
                    )
                    .await
                {
                    // 補償: 先に剥奪した旧親タプルを復元して FGA を移動前に戻し、ロールバックする。
                    if let Some(op) = old_parent {
                        let _ = self
                            .authz
                            .write_tuple(
                                &Subject::object(&FgaObject::folder(&op.to_string())),
                                Relation::Parent,
                                &file_obj,
                            )
                            .await;
                    }
                    return Err(StorageError::Authz(e));
                }
            }
        }
        // commit 失敗時は FGA を移動前へ戻す（DB は旧親のまま・FGA だけ新親＝漏れを防ぐ）。
        if let Err(e) = tx.commit().await {
            if parent_changed {
                if let Some(np) = final_parent {
                    let _ = self
                        .authz
                        .delete_tuple(
                            &Subject::object(&FgaObject::folder(&np.to_string())),
                            Relation::Parent,
                            &file_obj,
                        )
                        .await;
                }
                if let Some(op) = old_parent {
                    let _ = self
                        .authz
                        .write_tuple(
                            &Subject::object(&FgaObject::folder(&op.to_string())),
                            Relation::Parent,
                            &file_obj,
                        )
                        .await;
                }
            }
            return Err(StorageError::from(e));
        }
        row_to_node(row)
    }

    /// リネーム（[`update_file`](Self::update_file) の薄いラッパ）。
    pub async fn rename_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_name: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_file(ctx, file_id, Some(new_name), None, trace_id)
            .await
    }

    /// 移動（[`update_file`](Self::update_file) の薄いラッパ）。
    pub async fn move_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_parent: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_file(ctx, file_id, None, Some(new_parent), trace_id)
            .await
    }

    /// 論理削除（ゴミ箱）。blob refcount を減らす。
    pub async fn soft_delete_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
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
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
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
        let node = self.load_node(ctx, file_id, true).await?;
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
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NOT NULL \
             RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(file_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
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

    /// 読取系の viewer 認可。deny は**存在を秘匿**するため `NotFound` を返す（403/404 で
    /// 私有ファイルの存在が漏れないようにする・P2-6）。deny の監査は残す。
    async fn require_read(
        &self,
        ctx: &AuthContext,
        object: &FgaObject,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let allowed = self
            .authz
            .check(&ctx.subject(), Relation::Viewer, object)
            .await?;
        if !allowed {
            self.audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type: "file",
                        object_id,
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": Relation::Viewer.as_str() }),
                    },
                )
                .await?;
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    /// 親が存在する生存フォルダであることを確認する（org + tenant スコープ）。
    async fn ensure_folder(&self, ctx: &AuthContext, id: Uuid) -> Result<(), StorageError> {
        let kind: Option<String> = sqlx::query_scalar(
            "SELECT kind FROM node \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
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

    /// org + tenant スコープでノードを 1 件読む。
    async fn load_node(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        include_deleted: bool,
    ) -> Result<Node, StorageError> {
        let sql = if include_deleted {
            format!("SELECT {NODE_COLS} FROM node WHERE id = $1 AND org = $2 AND tenant_id = $3")
        } else {
            format!(
                "SELECT {NODE_COLS} FROM node \
                 WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL"
            )
        };
        let row: Option<NodeRow> = sqlx::query_as(&sql)
            .bind(id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
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
