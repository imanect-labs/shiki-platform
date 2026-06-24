//! StorageService — 権限・監査・content-addressing の単一チョークポイント（Task 1.3/1.4/1.9）。
//!
//! 不変条件:
//! - 全 read/write メソッドは第 1 引数に `&AuthContext` を取り、OpenFGA `check` を必ず通す。
//! - ハンドラに `db`/`store` を直接触らせない（このサービス経由でのみアクセス）。
//! - 各操作は allow/deny を監査ログに残す（書込系は同一 txn で原子的に）。
//! - バイトは presigned URL でクライアント↔MinIO 直転送し、アプリはメタ操作のみ（PIT-6）。

use std::{sync::Arc, time::Duration};

use authz::{AuthContext, AuthzClient, FgaObject, ObjectType, Relation, Subject};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry, AuditRecorder, Chain, Decision},
    content_address::{
        blob_object_key, incoming_object_key, is_valid_sha256_hex, staging_object_key,
    },
    error::StorageError,
    model::{
        ChildPage, Crumb, DownloadTicket, Node, NodeKind, ShareEntry, ShareRole, ShareTarget,
        UploadTicket,
    },
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
    /// 1 ファイルの最大アップロードサイズ（バイト）。declare の宣言サイズがこれを超えたら拒否し、
    /// 認証ユーザーによる無制限アップロードでのストレージ枯渇を防ぐ（容量ガード）。
    max_upload_size: i64,
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
        max_upload_size: i64,
    ) -> Self {
        let audit = AuditRecorder::new(db.clone());
        StorageService {
            db,
            store,
            authz,
            audit,
            presign_get_ttl,
            presign_put_ttl,
            max_upload_size,
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
        if declared_size > self.max_upload_size {
            return Err(StorageError::Invalid(format!(
                "size が上限を超えています（最大 {} バイト）",
                self.max_upload_size
            )));
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
                Chain::Yes,
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
                // 共有 content-addressed の `final_key` は失敗時に**削除しない**。並行 finalize が
                // 同 hash を commit 済みなら参照中の本体を壊し得るため（Lb76C のレース）。判定も
                // commit 直前のレース窓が残るので、削除はせず GC に委ねる。参照ゼロの孤児本体
                // （新規 hash の finalize が DB 失敗した稀ケースのみ）は **オブジェクトストアの
                // 孤児スイープ GC**（blob 行を持たないキーを掃除・後続）で回収する（refcount GC は
                // blob 行が無いと検知できないため）。upload 固有の incoming/staging だけ掃除する。
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
        let node = self.load_node(ctx, file_id, false).await?;
        self.require_read(
            ctx,
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

    /// ノード（ファイル/フォルダ）のリネーム・移動を **1 トランザクションで原子的に**適用する。
    ///
    /// `expect` でファイル/フォルダ種別を強制し（種別違いは存在秘匿の `NotFound`）、
    /// `new_name`: 指定でリネーム。`new_parent`: `Some(Some(p))` で `p` 配下へ、
    /// `Some(None)` でルートへ移動、`None` で移動しない。move と rename を一度に指定しても
    /// 部分適用にならない。
    ///
    /// 移動はサブツリー全体の closure を張り替え、**循環（自身の配下への移動）を拒否**する。
    /// PIT-16: 移動サブツリー ∪ 移動先の祖先列を id 昇順ロックした単一 txn で更新する。
    async fn update_node(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        expect: NodeKind,
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
        // 早期の存在＋種別＋tenant ゲート（最新状態はロック後に再読込する・Lb76B）。
        let existing = self.load_node(ctx, node_id, false).await?;
        if existing.kind != expect {
            return Err(StorageError::NotFound);
        }
        let self_obj = node_fga_object(expect, node_id);
        let action = update_action(expect);
        let id_str = node_id.to_string();
        // 対象ノードの editor 権限。
        self.require(
            ctx,
            Relation::Editor,
            &self_obj,
            action,
            expect.as_str(),
            &id_str,
            trace_id,
        )
        .await?;
        // 移動する場合は移動先の権限＋実在を確認。
        if let Some(target) = new_parent {
            match target {
                Some(p) => {
                    if p == node_id {
                        return Err(StorageError::Invalid("自分自身へは移動できません".into()));
                    }
                    self.require(
                        ctx,
                        Relation::Editor,
                        &FgaObject::folder(&p.to_string()),
                        action,
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
                        action,
                        "organization",
                        &ctx.org,
                        trace_id,
                    )
                    .await?;
                }
            }
        }

        let mut tx = self.db.begin().await?;
        // PIT-16: 移動時は「移動サブツリー ∪ 移動先の祖先列」を id 昇順ロック（祖先ロック下の単一 txn）。
        // rename だけなら対象 1 行で足りる。
        let lock_ids: Vec<Uuid> = if new_parent.is_some() {
            let mut ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT descendant FROM node_closure WHERE org = $1 AND ancestor = $2",
            )
            .bind(&ctx.org)
            .bind(node_id)
            .fetch_all(&mut *tx)
            .await?;
            if let Some(Some(p)) = new_parent {
                let anc: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT ancestor FROM node_closure WHERE org = $1 AND descendant = $2",
                )
                .bind(&ctx.org)
                .bind(p)
                .fetch_all(&mut *tx)
                .await?;
                ids.extend(anc);
            }
            ids
        } else {
            vec![node_id]
        };
        sqlx::query(
            "SELECT id FROM node \
             WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 ORDER BY id FOR UPDATE",
        )
        .bind(&lock_ids)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_all(&mut *tx)
        .await?;

        // Lb76B: ロック取得後に**最新状態を再読込**する。並行 move とオーバーラップした際、
        // ロック前に読んだ stale な親/名前を使うと、FGA の parent タプルが DB とずれる
        // （旧フォルダの継承アクセスが残る）ため、ロック下の最新行から計算する。
        let fresh_sql = format!(
            "SELECT {NODE_COLS} FROM node \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL"
        );
        let fresh: NodeRow = sqlx::query_as(&fresh_sql)
            .bind(node_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;
        let old_parent = fresh.parent_id;
        let final_parent = match new_parent {
            Some(target) => target,
            None => fresh.parent_id,
        };
        let final_name = new_name.unwrap_or(fresh.name.as_str());
        let parent_changed = new_parent.is_some() && final_parent != old_parent;

        // 循環拒否: 移動先が自身の配下（closure で ancestor=self に含まれる）なら拒否する。
        // ロック下で判定し、並行移動でサブツリーが入れ替わっても閉路を作らせない。
        if parent_changed {
            if let Some(p) = final_parent {
                let is_descendant: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM node_closure WHERE ancestor = $1 AND descendant = $2)",
                )
                .bind(node_id)
                .bind(p)
                .fetch_one(&mut *tx)
                .await?;
                if is_descendant {
                    return Err(StorageError::Invalid(
                        "フォルダを自身の配下へは移動できません".into(),
                    ));
                }
            }
        }

        // version をインクリメント（メタ変更を後段の write-event/索引が検知できるように）。
        let sql = format!(
            "UPDATE node SET name = $1, parent_id = $2, version = version + 1, updated_at = now() \
             WHERE id = $3 AND org = $4 AND tenant_id = $5 AND deleted_at IS NULL RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(final_name)
            .bind(final_parent)
            .bind(node_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;

        // closure 書換（親が変わった時のみ・サブツリー全体）:
        //   1. 移動サブツリーの各ノードから、移動ノードの旧祖先へのリンクを切る（サブツリー内部は保つ）。
        //   2. 新親（とその祖先）から移動サブツリー各ノードへ depth を足して張り直す（cross join）。
        // 葉（ファイル）はサブツリー＝自身のみなので既存挙動と一致する。
        if parent_changed {
            sqlx::query(
                "DELETE FROM node_closure \
                 WHERE descendant IN (SELECT descendant FROM node_closure WHERE ancestor = $1) \
                   AND ancestor IN (SELECT ancestor FROM node_closure WHERE descendant = $1 AND ancestor <> $1)",
            )
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
            if let Some(p) = final_parent {
                sqlx::query(
                    "INSERT INTO node_closure (org, ancestor, descendant, depth) \
                     SELECT sup.org, sup.ancestor, sub.descendant, sup.depth + sub.depth + 1 \
                     FROM node_closure sup CROSS JOIN node_closure sub \
                     WHERE sup.descendant = $1 AND sub.ancestor = $2",
                )
                .bind(p)
                .bind(node_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action,
                object_type: expect.as_str(),
                object_id: &id_str,
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "renamed": new_name.is_some(),
                    "moved": parent_changed,
                    "old_parent": old_parent.map(|p| p.to_string()),
                    "new_parent": final_parent.map(|p| p.to_string()),
                }),
            },
            Chain::Yes,
        )
        .await?;
        // FGA parent タプルは過剰権限を生まない順序で更新する（DB と FGA は 2PC できないため、
        // どの失敗点でも fail-safe へ倒し、かつ**冪等な再試行で収束**できるようにする）:
        //   1. 旧親の剥奪は **コミット前**（剥奪失敗ならロールバック＝旧親経由の継続アクセスを残さない）。
        //   2. 新親の付与は **コミット後**かつ **final_parent がある限り常に**実行する。
        //      `write_tuple` は冪等（既存は成功扱い）なので、付与失敗時に同じ move を再試行すれば
        //      `parent_changed=false` でも新親タプルを張り直して修復できる（Lb76A）。
        //      コミット前付与は移動未確定時の先行アクセスを生むため避ける（Lb76C/LbiSj）。
        // 移動するのは**自ノードの parent タプルのみ**（子は OpenFGA の `from parent` 継承で追従）。
        // 完全失敗時の残留（新親未付与＝過小権限）は再試行 or 書込イベント/outbox（Task 1.8）で収束。
        if parent_changed {
            if let Some(op) = old_parent {
                self.authz
                    .delete_tuple(
                        &Subject::object(&FgaObject::folder(&op.to_string())),
                        Relation::Parent,
                        &self_obj,
                    )
                    .await?; // 失敗 → tx は drop でロールバック（移動なし＝整合）
            }
        }
        if let Err(e) = tx.commit().await {
            // 移動が確定しなかったので、剥奪済みの旧親タプルを復元（冪等・best-effort）。
            if parent_changed {
                if let Some(op) = old_parent {
                    let _ = self
                        .authz
                        .write_tuple(
                            &Subject::object(&FgaObject::folder(&op.to_string())),
                            Relation::Parent,
                            &self_obj,
                        )
                        .await;
                }
            }
            return Err(StorageError::from(e));
        }
        // 現在の親（folder）への parent タプルを冪等に保証する（再試行で修復可能）。
        if let Some(np) = final_parent {
            self.authz
                .write_tuple(
                    &Subject::object(&FgaObject::folder(&np.to_string())),
                    Relation::Parent,
                    &self_obj,
                )
                .await?;
        }
        row_to_node(row)
    }

    /// ファイルのリネーム・移動（[`update_node`](Self::update_node) の種別固定ラッパ）。
    pub async fn update_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_node(ctx, file_id, NodeKind::File, new_name, new_parent, trace_id)
            .await
    }

    /// フォルダのリネーム・移動（[`update_node`](Self::update_node) の種別固定ラッパ）。
    pub async fn update_folder(
        &self,
        ctx: &AuthContext,
        folder_id: Uuid,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_node(
            ctx,
            folder_id,
            NodeKind::Folder,
            new_name,
            new_parent,
            trace_id,
        )
        .await
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

    /// フォルダのリネーム（[`update_folder`](Self::update_folder) の薄いラッパ）。
    pub async fn rename_folder(
        &self,
        ctx: &AuthContext,
        folder_id: Uuid,
        new_name: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_folder(ctx, folder_id, Some(new_name), None, trace_id)
            .await
    }

    /// フォルダの移動（[`update_folder`](Self::update_folder) の薄いラッパ）。
    pub async fn move_folder(
        &self,
        ctx: &AuthContext,
        folder_id: Uuid,
        new_parent: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.update_folder(ctx, folder_id, None, Some(new_parent), trace_id)
            .await
    }

    /// フォルダを作成する（親フォルダ配下 or org ルート直下）。
    ///
    /// 認可は upload と対称: フォルダ配下は `editor@parent`、ルートは `member@org`。
    /// closure（親継承 ＋ self depth0）を張り、FGA に owner（＋folder 配下なら parent）
    /// タプルを書く。DB と FGA は 2PC できないため、tuple は **commit 前**に書き、
    /// parent 失敗・commit 失敗のどちらでも書けた tuple を revoke して不整合を残さない。
    pub async fn create_folder(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        validate_name(name)?;
        match parent_id {
            Some(p) => {
                self.require(
                    ctx,
                    Relation::Editor,
                    &FgaObject::folder(&p.to_string()),
                    "folder.create",
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
                    "folder.create",
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }

        let tx_result: Result<Node, StorageError> = async {
            let mut tx = self.db.begin().await?;
            let sql = format!(
                "INSERT INTO node (org, tenant_id, kind, name, parent_id, created_by) \
                 VALUES ($1, $2, 'folder', $3, $4, $5) RETURNING {NODE_COLS}"
            );
            let row: NodeRow = sqlx::query_as(&sql)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .bind(name)
                .bind(parent_id)
                .bind(&ctx.principal.id)
                .fetch_one(&mut *tx)
                .await?;
            let folder_id = row.id;
            // 祖先リンク（親の closure を +1 で引き継ぐ）。
            if let Some(p) = parent_id {
                sqlx::query(
                    "INSERT INTO node_closure (org, ancestor, descendant, depth) \
                     SELECT org, ancestor, $1, depth + 1 FROM node_closure WHERE descendant = $2",
                )
                .bind(folder_id)
                .bind(p)
                .execute(&mut *tx)
                .await?;
            }
            // 自分自身（depth 0）。
            sqlx::query(
                "INSERT INTO node_closure (org, ancestor, descendant, depth) VALUES ($1, $2, $2, 0)",
            )
            .bind(&ctx.org)
            .bind(folder_id)
            .execute(&mut *tx)
            .await?;

            let folder_obj = FgaObject::folder(&folder_id.to_string());
            self.authz
                .write_tuple(&ctx.subject(), Relation::Owner, &folder_obj)
                .await
                .map_err(StorageError::Authz)?;
            if let Some(p) = parent_id {
                if let Err(e) = self
                    .authz
                    .write_tuple(
                        &Subject::object(&FgaObject::folder(&p.to_string())),
                        Relation::Parent,
                        &folder_obj,
                    )
                    .await
                {
                    let _ = self
                        .authz
                        .delete_tuple(&ctx.subject(), Relation::Owner, &folder_obj)
                        .await;
                    return Err(StorageError::Authz(e));
                }
            }
            audit::record_on(
                &mut tx,
                ctx,
                AuditEntry {
                    action: "folder.create",
                    object_type: "folder",
                    object_id: &folder_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "parent_id": parent_id.map(|p| p.to_string()) }),
                },
                Chain::Yes,
            )
            .await?;
            if let Err(e) = tx.commit().await {
                let _ = self
                    .authz
                    .delete_tuple(&ctx.subject(), Relation::Owner, &folder_obj)
                    .await;
                if let Some(p) = parent_id {
                    let _ = self
                        .authz
                        .delete_tuple(
                            &Subject::object(&FgaObject::folder(&p.to_string())),
                            Relation::Parent,
                            &folder_obj,
                        )
                        .await;
                }
                return Err(StorageError::from(e));
            }
            row_to_node(row)
        }
        .await;
        tx_result
    }

    /// フォルダの子を**権限フィルタ済み**で 1 ページ返す（PIT-13）。
    ///
    /// `parent_id` が `None` なら org ルート直下。`limit` は 1..=100 にクランプ。
    /// `(name, id)` 昇順の keyset カーソルでページングする。`next_cursor` が `Some` なら続きがある
    /// （末尾ちょうどで空ページが 1 回返ることはあるが、欠落や重複は起きない）。
    ///
    /// 権限フィルタは親の種別で 2 段構えにする（pre-filter＋post-filter）:
    /// - **フォルダ配下**: 親の viewer を確認できれば、union-only の継承モデル上**全子が viewer**
    ///   （`viewer from parent`）。親チェックを pre-filter とし、子ごとの check は不要。
    /// - **ルート直下**: 共通の親が無いため、子ごとに viewer を post-filter する
    ///   （読めない子はオーバーフェッチで読み飛ばす）。
    pub async fn list_children(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        cursor: Option<&str>,
        limit: usize,
        trace_id: Option<&str>,
    ) -> Result<ChildPage, StorageError> {
        // 親の閲覧可否を先に確認（ルートは org メンバー）。読めない親は存在秘匿で空扱い。
        match parent_id {
            Some(p) => {
                self.ensure_folder(ctx, p).await?;
                self.require_read(
                    ctx,
                    &FgaObject::folder(&p.to_string()),
                    "folder.children.list",
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
                    "folder.children.list",
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }

        let limit = limit.clamp(1, 100);
        // フォルダ配下は親 viewer が全子に継承するため子ごとの post-filter は不要。
        // ルート直下のみ子ごとに viewer を確認する。
        let post_filter = parent_id.is_none();
        // 1 ラウンドのフェッチ歩幅。post-filter 時はフィルタ落ちを見越して多めに引く。
        let batch: i64 = if post_filter {
            (limit as i64 * 2).clamp(16, 200)
        } else {
            limit as i64
        };
        let (mut after_name, mut after_id) = match cursor {
            Some(c) => {
                let (name, id) = decode_child_cursor(c)?;
                (Some(name), Some(id))
            }
            None => (None, None),
        };

        let mut items: Vec<Node> = Vec::with_capacity(limit);
        let mut exhausted = false;
        while items.len() < limit && !exhausted {
            // keyset: (name, id) > (after_name, after_id)。parent_id は IS NOT DISTINCT FROM で
            // NULL（ルート）も同値比較する。
            let sql = format!(
                "SELECT {NODE_COLS} FROM node \
                 WHERE org = $1 AND tenant_id = $2 AND deleted_at IS NULL \
                   AND parent_id IS NOT DISTINCT FROM $3 \
                   AND ($4::text IS NULL OR (name, id) > ($4, $5)) \
                 ORDER BY name, id LIMIT $6"
            );
            let rows: Vec<NodeRow> = sqlx::query_as(&sql)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .bind(parent_id)
                .bind(after_name.as_deref())
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
                after_name = Some(row.name.clone());
                after_id = Some(row.id);
                // フォルダ配下は親 viewer の継承で全子可視。ルート直下のみ子ごとに確認。
                if post_filter {
                    let kind = NodeKind::parse(&row.kind).unwrap_or(NodeKind::File);
                    let allowed = self
                        .authz
                        .check(
                            &ctx.subject(),
                            Relation::Viewer,
                            &node_fga_object(kind, row.id),
                        )
                        .await?;
                    if !allowed {
                        continue;
                    }
                }
                items.push(row_to_node(row)?);
                if items.len() == limit {
                    break;
                }
            }
        }
        // limit 充足で止めたなら続きがあり得る → カーソルを返す。尽きたなら None。
        let next_cursor = if items.len() == limit {
            match (after_name, after_id) {
                (Some(n), Some(i)) => Some(encode_child_cursor(&n, i)),
                _ => None,
            }
        } else {
            None
        };
        Ok(ChildPage { items, next_cursor })
    }

    /// ノードのパンくず（祖先列）を root→自身の順で返す（**読める接尾のみ**）。
    ///
    /// 自身の viewer を確認後、closure の祖先を**自身→上**（depth 昇順）に辿り、読めない祖先に
    /// 当たった時点で打ち切る。これにより返すのは「自身から上方向に連続して読める範囲」＝
    /// 読める接尾（contiguous suffix ending at self）であり、読めない祖先名は一切漏れない。
    /// 直接共有でルート祖先が読めない場合は、読める範囲（最小で自身のみ）だけを返す。
    pub async fn breadcrumb(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<Crumb>, StorageError> {
        let node = self.load_node(ctx, node_id, false).await?;
        self.require_read(
            ctx,
            &node_fga_object(node.kind, node_id),
            "node.breadcrumb.read",
            node.kind.as_str(),
            &node_id.to_string(),
            trace_id,
        )
        .await?;
        // 祖先（自身含む）を 自身→root の順（depth 昇順）で取得する。
        let rows: Vec<(Uuid, String, String, i32)> = sqlx::query_as(
            "SELECT n.id, n.name, n.kind, c.depth \
             FROM node_closure c JOIN node n ON n.id = c.ancestor \
             WHERE c.descendant = $1 AND n.org = $2 AND n.tenant_id = $3 AND n.deleted_at IS NULL \
             ORDER BY c.depth ASC",
        )
        .bind(node_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await?;

        // 自身（depth 0）から上へ。読めない祖先に当たったら打ち切る（読める接尾のみ）。
        let mut crumbs = Vec::with_capacity(rows.len());
        for (id, name, kind, _depth) in rows {
            let kind = NodeKind::parse(&kind)
                .ok_or_else(|| StorageError::Integrity(format!("未知のノード種別: {kind}")))?;
            if id != node_id {
                let allowed = self
                    .authz
                    .check(&ctx.subject(), Relation::Viewer, &node_fga_object(kind, id))
                    .await?;
                if !allowed {
                    break;
                }
            }
            crumbs.push(Crumb { id, name, kind });
        }
        // 自身→root で積んだので、表示順（root→自身）へ反転する。
        crumbs.reverse();
        Ok(crumbs)
    }

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
            &FgaObject::folder(&folder_id.to_string()),
            "folder.delete",
            "folder",
            &folder_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        // サブツリー（自身含む）の生存ノードをまとめて論理削除する。
        let affected = sqlx::query(
            "UPDATE node SET deleted_at = now(), updated_at = now() \
             WHERE org = $1 AND tenant_id = $2 AND deleted_at IS NULL \
               AND id IN (SELECT descendant FROM node_closure WHERE ancestor = $3)",
        )
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(folder_id)
        .execute(&mut *tx)
        .await?;
        if affected.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "folder.delete",
                object_type: "folder",
                object_id: &folder_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "subtree_count": affected.rows_affected() }),
            },
            Chain::Yes,
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
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
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
        // soft_delete で refcount を減らさないので、復元でも増やさない（対称・LbvQZ）。
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
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        row_to_node(row)
    }

    // --- 共有（ReBAC: Task 1.6） ---

    /// ファイル/フォルダを user / role へ viewer/editor で共有する。
    ///
    /// 共有の管理（ACL 付与）は **owner 権限**を要求する（editor が再共有して権限を
    /// 横展開する confused-deputy を防ぐ）。OpenFGA の tuple 付与として実装し、
    /// ロール共有は `role:<id>#member` 1 タプルでロールメンバー（配下ロール含む）へ継承する
    /// （`role#member` は org 継承を含まないため org 全体共有にならない・#72）。
    ///
    /// FGA と監査 DB は別 durability 境界のため、**監査失敗時は付与した tuple を補償剥奪**して
    /// 「ACL は変わったが監査が無い」状態を残さない（呼び出し側の再試行で収束する）。
    pub async fn share_node(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        target: &ShareTarget,
        role: ShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, "node.share", trace_id)
            .await?;
        // 付与は冪等（既存タプルは成功扱い）。
        self.authz
            .write_tuple(&target.subject(), role.relation(), &obj)
            .await?;
        if let Err(e) = self
            .record_share_audit(ctx, node_id, &obj, "node.share", target, role, trace_id)
            .await
        {
            // 監査が残らないので付与を巻き戻す（冪等剥奪・best-effort）。
            let _ = self
                .authz
                .delete_tuple(&target.subject(), role.relation(), &obj)
                .await;
            return Err(e);
        }
        Ok(())
    }

    /// 共有を解除する（owner 権限・冪等）。
    ///
    /// PIT-11: check は HIGHER_CONSISTENCY で問い合わせるため、剥奪は次リクエストから即時に効く。
    /// 監査失敗時は剥奪を補償付与して「ACL は剥奪されたが監査が無い」状態を残さない。
    pub async fn unshare_node(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        target: &ShareTarget,
        role: ShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, "node.unshare", trace_id)
            .await?;
        // 剥奪も冪等（存在しないタプルの削除は成功扱い）。
        self.authz
            .delete_tuple(&target.subject(), role.relation(), &obj)
            .await?;
        if let Err(e) = self
            .record_share_audit(ctx, node_id, &obj, "node.unshare", target, role, trace_id)
            .await
        {
            // 監査が残らないので剥奪を巻き戻す（冪等付与・best-effort）。
            let _ = self
                .authz
                .write_tuple(&target.subject(), role.relation(), &obj)
                .await;
            return Err(e);
        }
        Ok(())
    }

    /// このノードの共有相手一覧を返す（owner 権限）。
    ///
    /// オブジェクトに**直接**書かれた viewer/editor タプルのみを返す（owner/parent や
    /// 親フォルダからの継承は含めない＝「このノードで誰に共有したか」の管理ビュー）。
    pub async fn list_shares(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<ShareEntry>, StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, "node.shares.list", trace_id)
            .await?;
        let tuples = self.authz.read_tuples(&obj, None).await?;
        let mut entries = Vec::new();
        for t in tuples {
            // viewer/editor のみ共有として扱う（owner/parent は管理対象外）。
            let Some(role) = Relation::parse(&t.relation).and_then(ShareRole::from_relation) else {
                continue;
            };
            let Some(target) = ShareTarget::parse_subject(&t.user) else {
                continue;
            };
            entries.push(ShareEntry { target, role });
        }
        Ok(entries)
    }

    /// 自分に共有されたノード一覧（自分が作成したものを除く・org+tenant スコープ）。
    ///
    /// OpenFGA の `list-objects`（viewer 実効集合・継承込み）で id を引き、DB で生存ノードの
    /// メタへ解決する。作成者本人のノード（≒owner）は「共有された」一覧から除く。
    pub async fn list_shared_with_me(
        &self,
        ctx: &AuthContext,
        trace_id: Option<&str>,
    ) -> Result<Vec<Node>, StorageError> {
        let subject = ctx.subject();
        let mut ids: Vec<Uuid> = Vec::new();
        for object_type in [ObjectType::File, ObjectType::Folder] {
            let objs = self
                .authz
                .list_objects(&subject, Relation::Viewer, object_type)
                .await?;
            for o in objs {
                // "file:<uuid>" / "folder:<uuid>" の id 部を取り出す。
                if let Some((_, id)) = o.split_once(':') {
                    if let Ok(uuid) = Uuid::parse_str(id) {
                        ids.push(uuid);
                    }
                }
            }
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {NODE_COLS} FROM node \
             WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 \
               AND deleted_at IS NULL AND created_by <> $4 \
             ORDER BY updated_at DESC"
        );
        let rows: Vec<NodeRow> = sqlx::query_as(&sql)
            .bind(&ids)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .bind(&ctx.principal.id)
            .fetch_all(&self.db)
            .await?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "node.shared_with_me.list",
                    object_type: "organization",
                    object_id: &ctx.org,
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "count": rows.len() }),
                },
            )
            .await?;
        rows.into_iter().map(row_to_node).collect()
    }

    /// 共有管理（share/unshare/list）の前段: ノードの存在確認＋owner 認可。FGA object を返す。
    async fn authorize_share_admin(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<FgaObject, StorageError> {
        let node = self.load_node(ctx, node_id, false).await?;
        let obj = node_fga_object(node.kind, node_id);
        self.require(
            ctx,
            Relation::Owner,
            &obj,
            action,
            node.kind.as_str(),
            &node_id.to_string(),
            trace_id,
        )
        .await?;
        Ok(obj)
    }

    /// 共有/解除の監査を**ハッシュチェーンに連結**して記録する（権限変更は改竄検知対象）。
    #[allow(clippy::too_many_arguments)] // 監査記録に必要なフィールド一式。
    async fn record_share_audit(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        obj: &FgaObject,
        action: &str,
        target: &ShareTarget,
        role: ShareRole,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let object_type = if obj.as_str().starts_with("folder:") {
            "folder"
        } else {
            "file"
        };
        let mut tx = self.db.begin().await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action,
                object_type,
                object_id: &node_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "target": target,
                    "role": role,
                }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
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
        object_type: &str,
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
                        object_type,
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

/// ノード種別に対応する OpenFGA オブジェクト識別子（`file:<id>` / `folder:<id>`）。
fn node_fga_object(kind: NodeKind, id: Uuid) -> FgaObject {
    match kind {
        NodeKind::File => FgaObject::file(&id.to_string()),
        NodeKind::Folder => FgaObject::folder(&id.to_string()),
    }
}

/// リネーム/移動の監査アクション名（種別ごと）。
fn update_action(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::File => "file.update",
        NodeKind::Folder => "folder.update",
    }
}

/// 子一覧 keyset カーソルの不透明エンコード。`(name, id)` を `id`(36桁)＋`name` の順で
/// 連結し hex 化する（区切り不要: uuid は固定長で衝突しない）。クライアントには不透明。
fn encode_child_cursor(name: &str, id: Uuid) -> String {
    hex::encode(format!("{id}{name}").as_bytes())
}

/// [`encode_child_cursor`] の逆。壊れたカーソルは `Invalid`。
fn decode_child_cursor(cursor: &str) -> Result<(String, Uuid), StorageError> {
    let bytes =
        hex::decode(cursor).map_err(|_| StorageError::Invalid("カーソルが不正です".into()))?;
    let s =
        String::from_utf8(bytes).map_err(|_| StorageError::Invalid("カーソルが不正です".into()))?;
    // 先頭 36 文字が uuid、残りが name。
    if s.len() < 36 {
        return Err(StorageError::Invalid("カーソルが不正です".into()));
    }
    let (id_part, name) = s.split_at(36);
    let id =
        Uuid::parse_str(id_part).map_err(|_| StorageError::Invalid("カーソルが不正です".into()))?;
    Ok((name.to_string(), id))
}

/// ノード名の検証。空/長すぎ/前後空白/`.`・`..`/パス区切り/制御文字を拒否する。
///
/// 名前は download の `Content-Disposition` ヘッダにも流れるため、`\r`/`\n` 等の制御文字を
/// 弾いてヘッダインジェクションの素地を断つ。前後空白は黙って trim せず拒否する（往復で
/// 名前が変わる混乱を避ける）。`.`/`..` は UI/同期での予約名衝突を避けるため拒否する。
fn validate_name(name: &str) -> Result<(), StorageError> {
    if name.is_empty() {
        return Err(StorageError::Invalid("名前が空です".into()));
    }
    if name.chars().count() > 255 {
        return Err(StorageError::Invalid(
            "名前が長すぎます（255 文字以内）".into(),
        ));
    }
    if name != name.trim() {
        return Err(StorageError::Invalid("名前の前後に空白は使えません".into()));
    }
    if name == "." || name == ".." {
        return Err(StorageError::Invalid("名前に . / .. は使えません".into()));
    }
    if name.contains('/') {
        return Err(StorageError::Invalid("名前に / は使えません".into()));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(StorageError::Invalid("名前に制御文字は使えません".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_rejects_bad_inputs() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err()); // 前後空白（trim で空）
        assert!(validate_name(" leading").is_err());
        assert!(validate_name("trailing ").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("bad\nname").is_err()); // 制御文字（改行）
        assert!(validate_name("bad\rname").is_err());
        assert!(validate_name("bad\u{0}name").is_err()); // NUL
        assert!(validate_name(&"x".repeat(256)).is_err());
        assert!(validate_name("report.pdf").is_ok());
        assert!(validate_name("日本語.txt").is_ok());
        assert!(validate_name("a.b.c").is_ok()); // ドットを含む通常名は可
    }

    #[test]
    fn child_cursor_roundtrips() {
        let id = Uuid::new_v4();
        for name in ["report.pdf", "日本語フォルダ", "a", &"x".repeat(255)] {
            let c = encode_child_cursor(name, id);
            let (got_name, got_id) = decode_child_cursor(&c).expect("decode");
            assert_eq!(got_name, name);
            assert_eq!(got_id, id);
        }
    }

    #[test]
    fn child_cursor_rejects_garbage() {
        assert!(decode_child_cursor("zzz").is_err()); // 非 hex
        assert!(decode_child_cursor(&hex::encode("short")).is_err()); // 36 文字未満
    }

    #[test]
    fn node_fga_object_maps_kind() {
        let id = Uuid::nil();
        assert_eq!(
            node_fga_object(NodeKind::File, id).as_str(),
            format!("file:{id}")
        );
        assert_eq!(
            node_fga_object(NodeKind::Folder, id).as_str(),
            format!("folder:{id}")
        );
    }
}
