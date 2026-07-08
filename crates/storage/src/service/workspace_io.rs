//! StorageService: ワークスペースのパス指定書込（Task 5.4/5.8）。
//!
//! 自律エージェントの**ワークスペース**（thread ごとの Drive フォルダ）に対する
//! **「パス指定で既存なら新版・無ければ作成」の内部 upsert**。バイト列を所持した内部書込で、
//! 認可・content-addressing・版管理・監査・書込イベント（→再索引）を単一チョークポイントで通す。
//! CRUD の read/list/delete は既存の `read_file_internal`/`list_children`/`soft_delete_file` を再利用し、
//! 名前→node の解決だけをここが担う（[`resolve_child_file`](StorageService::resolve_child_file)）。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::content_address::sha256_hex;

/// [`write_file_at`](StorageService::write_file_at) の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteAtOutcome {
    pub node_id: Uuid,
    pub version: i64,
    /// 新規作成なら true、既存への新版追加なら false。
    pub created: bool,
}

impl StorageService {
    /// 親フォルダ配下の `name` を **create-or-new-version** で書き込む（バイト列所持の内部書込）。
    ///
    /// 既存の同名生存ファイルがあれば内容を新版へ差し替え（`WriteOp::Update`）、無ければ新規作成
    /// （`WriteOp::Create`）する。いずれも content-addressing・版記録・監査・書込イベントを 1 txn で
    /// 原子的に確定する（finalize の create/update 経路と対称）。認可は配置先フォルダの `editor`。
    // create/update の両分岐を content-addressing→メタ確定→イベントまで一体で追えるよう長め。
    #[allow(clippy::too_many_lines)]
    pub async fn write_file_at(
        &self,
        ctx: &AuthContext,
        parent_id: Uuid,
        name: &str,
        bytes: &[u8],
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<WriteAtOutcome, StorageError> {
        validate_name(name)?;
        let size = i64::try_from(bytes.len())
            .map_err(|_| StorageError::Invalid("size が大きすぎます".into()))?;
        if size > self.max_upload_size {
            return Err(StorageError::Invalid(format!(
                "size が上限を超えています（最大 {} バイト）",
                self.max_upload_size
            )));
        }

        // 配置先フォルダの editor 権限（内部書込の共通要求）。
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().folder(&parent_id.to_string()),
            "file.write.workspace",
            "folder",
            &parent_id.to_string(),
            trace_id,
        )
        .await?;
        self.ensure_folder(ctx, parent_id).await?;

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
        // **(parent, name) に TX advisory lock** を掛け、resolve→create/update を直列化する。
        // これが無いと、同名**新規**ファイルの並行書込で双方が existing=None を観測し、片方が
        // node の (parent,name) unique 制約に衝突する（新規行は FOR UPDATE で待てないため）。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{}|{}|{parent_id}|{name}", ctx.tenant_id, ctx.org))
            .execute(&mut *tx)
            .await?;
        // 既存の同名生存ファイルを **行ロックして** 解決する（並行書込の lost-update を防ぐ）。
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM node \
             WHERE parent_id = $1 AND org = $2 AND tenant_id = $3 AND name = $4 \
               AND kind = 'file' AND deleted_at IS NULL \
             FOR UPDATE",
        )
        .bind(parent_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;

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

        let (node, op, created) = if let Some(target) = existing {
            // 既存 → 新版へ差し替え（finalize_content_update と同一の UPDATE）。
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
                .bind(target)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StorageError::NotFound)?;
            (row_to_node(row)?, WriteOp::Update, false)
        } else {
            // 新規作成（write_file_core の作成経路と同一）。
            let node = self
                .create_file_node(
                    &mut tx,
                    ctx,
                    Some(parent_id),
                    name,
                    &sha256,
                    size,
                    content_type,
                )
                .await?;
            (node, WriteOp::Create, true)
        };

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
        let action = if created {
            "file.write.workspace.create"
        } else {
            "file.write.workspace.update"
        };
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action,
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
                op,
                payload: json!({ "kind": "file", "blob_sha256": sha256, "size": size,
                    "parent_id": parent_id.to_string() }),
            },
            trace_id,
        )
        .await?;

        // 新規作成時のみ FGA tuple（owner＋parent）を書く（write_file_core と同じ順序・補償）。
        // 更新は既存ノードの tuple を流用するため触らない。
        let file_obj = ctx.ns().file(&node.id.to_string());
        if created {
            self.authz
                .write_tuple(&ctx.subject(), Relation::Owner, &file_obj)
                .await
                .map_err(StorageError::Authz)?;
            if let Err(e) = self
                .authz
                .write_tuple(
                    &Subject::object(&ctx.ns().folder(&parent_id.to_string())),
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
        // commit 失敗時は（新規で）書いた tuple を revoke して FGA を作成前へ戻す。
        if let Err(e) = tx.commit().await {
            if created {
                let _ = self
                    .authz
                    .delete_tuple(&ctx.subject(), Relation::Owner, &file_obj)
                    .await;
                let _ = self
                    .authz
                    .delete_tuple(
                        &Subject::object(&ctx.ns().folder(&parent_id.to_string())),
                        Relation::Parent,
                        &file_obj,
                    )
                    .await;
            }
            return Err(StorageError::from(e));
        }

        Ok(WriteAtOutcome {
            node_id: node.id,
            version: node.version,
            created,
        })
    }

    /// 親フォルダ配下の生存ファイルを名前で解決する（無ければ `None`）。read/delete の名前解決に使う。
    ///
    /// 読み取り認可は親フォルダ（`viewer`）。存在秘匿のため、読めない親は上流で空扱いになる。
    pub async fn resolve_child_file(
        &self,
        ctx: &AuthContext,
        parent_id: Uuid,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Option<Uuid>, StorageError> {
        self.require_read(
            ctx,
            &ctx.ns().folder(&parent_id.to_string()),
            "file.resolve.workspace",
            "folder",
            &parent_id.to_string(),
            trace_id,
        )
        .await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM node \
             WHERE parent_id = $1 AND org = $2 AND tenant_id = $3 AND name = $4 \
               AND kind = 'file' AND deleted_at IS NULL",
        )
        .bind(parent_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(name)
        .fetch_optional(&self.db)
        .await?;
        Ok(id)
    }
}
