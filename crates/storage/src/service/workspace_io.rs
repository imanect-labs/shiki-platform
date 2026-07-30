//! StorageService: ワークスペースのパス指定書込／追記（Task 5.4/5.8・#392）。
//!
//! 自律エージェントの**ワークスペース**（thread ごとの Drive フォルダ）に対する
//! **「パス指定で既存なら新版・無ければ作成」の内部 upsert**。バイト列を所持した内部書込で、
//! 認可・content-addressing・版管理・監査・書込イベント（→再索引）を単一チョークポイントで通す。
//! CRUD の read/list/delete は既存の `read_file_internal`/`list_children`/`soft_delete_file` を再利用し、
//! 名前→node の解決だけをここが担う（[`resolve_child_file`](StorageService::resolve_child_file)）。
//!
//! [`append_file_at`](StorageService::append_file_at) は**末尾追記**（#392）。証拠台帳のような
//! append-only のメモを全文再送なしに伸ばすための操作で、モデルが毎ターン全文を出し直す
//! （トークンが二次で増える）のを構造的に避ける。既存内容の読み出しは `(parent, name)` の
//! advisory lock を**取ってから**行い、並行追記でどちらかが消えないようにする。
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
        let size = self.checked_size(bytes.len())?;
        self.require_workspace_write(ctx, parent_id, trace_id)
            .await?;

        // content-addressing: 所持バイトをハッシュし、新規 blob のみオブジェクトストアへ。
        // 全文置換は既存内容を読む必要が無いので、txn の**外**で put まで済ませる（ロックを短く保つ）。
        let sha256 = sha256_hex(bytes);
        let final_key = blob_object_key(&ctx.tenant_id, &ctx.org, &sha256);
        // txn の外なのでプールから 1 本借りて確認する（占有を跨がない）。
        let mut probe = self.db.acquire().await?;
        let known = Self::blob_exists(&mut probe, &sha256, ctx).await?;
        drop(probe);
        if !known {
            self.store
                .put_object(&final_key, bytes.to_vec(), content_type)
                .await?;
        }

        let mut tx = self.db.begin().await?;
        let existing = Self::lock_child_file(&mut tx, ctx, parent_id, name).await?;
        let (node, created) = self
            .apply_write_at(
                &mut tx,
                ctx,
                parent_id,
                name,
                existing.map(|e| e.node_id),
                &BlobFacts {
                    sha256: &sha256,
                    size,
                    content_type,
                    object_key: &final_key,
                },
                trace_id,
            )
            .await?;
        self.commit_write_at(tx, ctx, parent_id, &node, created)
            .await
    }

    /// 親フォルダ配下の `name` の**末尾へ追記**する（無ければ作成・#392）。
    ///
    /// 既存内容 ＋ `suffix` を新しい内容として新版に記録する（版管理上は通常の新版なので、
    /// 過去の台帳もそのまま復元できる）。既存内容の読み出しは `(parent, name)` の advisory lock
    /// 取得**後**に行うため、並行追記で片方の追記が消えることがない。
    pub async fn append_file_at(
        &self,
        ctx: &AuthContext,
        parent_id: Uuid,
        name: &str,
        suffix: &[u8],
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<WriteAtOutcome, StorageError> {
        validate_name(name)?;
        self.require_workspace_write(ctx, parent_id, trace_id)
            .await?;

        let mut tx = self.db.begin().await?;
        let existing = Self::lock_child_file(&mut tx, ctx, parent_id, name).await?;
        // 既存内容を**ロック内で**読む。base が無ければ suffix がそのまま全体になる（新規作成）。
        let mut content = match &existing {
            Some(e) => match &e.blob_sha256 {
                Some(sha) => {
                    let key = blob_object_key(&ctx.tenant_id, &ctx.org, sha);
                    self.store.get_object(&key).await?
                }
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        content.extend_from_slice(suffix);
        let size = self.checked_size(content.len())?;

        let sha256 = sha256_hex(&content);
        let final_key = blob_object_key(&ctx.tenant_id, &ctx.org, &sha256);
        // 追記は「既存を読んでから」ハッシュが決まるため、put は txn 内になる（ロックを跨ぐ）。
        // rollback すると参照されない blob オブジェクトが残るが、content-addressed なので
        // 同一内容の再試行で再利用され、実害は容量のみ（blob 行は commit されない）。
        if !Self::blob_exists(&mut tx, &sha256, ctx).await? {
            self.store
                .put_object(&final_key, content, content_type)
                .await?;
        }

        let (node, created) = self
            .apply_write_at(
                &mut tx,
                ctx,
                parent_id,
                name,
                existing.map(|e| e.node_id),
                &BlobFacts {
                    sha256: &sha256,
                    size,
                    content_type,
                    object_key: &final_key,
                },
                trace_id,
            )
            .await?;
        self.commit_write_at(tx, ctx, parent_id, &node, created)
            .await
    }

    /// サイズ上限（容量ガード）を通した `i64` サイズ。
    fn checked_size(&self, len: usize) -> Result<i64, StorageError> {
        let size =
            i64::try_from(len).map_err(|_| StorageError::Invalid("size が大きすぎます".into()))?;
        if size > self.max_upload_size {
            return Err(StorageError::Invalid(format!(
                "size が上限を超えています（最大 {} バイト）",
                self.max_upload_size
            )));
        }
        Ok(size)
    }

    /// 配置先フォルダの editor 権限（内部書込の共通要求）＋フォルダ存在確認。
    async fn require_workspace_write(
        &self,
        ctx: &AuthContext,
        parent_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
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
        self.ensure_folder(ctx, parent_id).await
    }

    /// 同一内容の blob が既にあるか（あれば put を省く）。
    ///
    /// **接続は呼び出し側が渡す**。追記経路はトランザクション（＝1 コネクション占有）を
    /// 保持したまま呼ぶため、プールから別コネクションを取ると
    /// 「全コネクションが tx を持ったまま互いに待つ」枯渇/デッドロックになり得る
    /// （レビュー指摘・CodeRabbit Critical / Codex P1）。
    async fn blob_exists(
        conn: &mut PgConnection,
        sha256: &str,
        ctx: &AuthContext,
    ) -> Result<bool, StorageError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3)",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(sha256)
        .fetch_one(conn)
        .await?;
        Ok(exists)
    }

    /// `(parent, name)` を直列化し、既存の同名生存ファイルを**行ロックして**解決する。
    ///
    /// **(parent, name) に TX advisory lock** を掛け、resolve→create/update を直列化する。
    /// これが無いと、同名**新規**ファイルの並行書込で双方が existing=None を観測し、片方が
    /// node の (parent,name) unique 制約に衝突する（新規行は FOR UPDATE で待てないため）。
    /// 既存行は `FOR UPDATE` で lost-update を防ぐ（追記の base 読みもこのロック下で行う）。
    async fn lock_child_file(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ctx: &AuthContext,
        parent_id: Uuid,
        name: &str,
    ) -> Result<Option<ExistingFile>, StorageError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{}|{}|{parent_id}|{name}", ctx.tenant_id, ctx.org))
            .execute(&mut **tx)
            .await?;
        let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
            "SELECT id, blob_sha256 FROM node \
             WHERE parent_id = $1 AND org = $2 AND tenant_id = $3 AND name = $4 \
               AND kind = 'file' AND deleted_at IS NULL \
             FOR UPDATE",
        )
        .bind(parent_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(name)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row.map(|(node_id, blob_sha256)| ExistingFile {
            node_id,
            blob_sha256,
        }))
    }

    /// blob 参照・ノード（新版 or 新規）・版記録・監査・書込イベントを 1 txn で確定する。
    // txn/ctx/配置先/名前/既存/blob 事実/trace の 7 点＋self は upsert の確定に本質的。
    #[allow(clippy::too_many_arguments)]
    async fn apply_write_at(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ctx: &AuthContext,
        parent_id: Uuid,
        name: &str,
        existing: Option<Uuid>,
        blob: &BlobFacts<'_>,
        trace_id: Option<&str>,
    ) -> Result<(Node, bool), StorageError> {
        self.bump_blob(
            tx,
            &ctx.tenant_id,
            &ctx.org,
            blob.sha256,
            blob.size,
            blob.content_type,
            blob.object_key,
        )
        .await?;

        let (node, op, created) = if let Some(target) = existing {
            // 既存 → 新版へ差し替え（finalize_content_update と同一の UPDATE）。
            let sql = format!(
                "UPDATE node \
                 SET blob_sha256 = $1, size_bytes = $2, content_type = $3, version = {NEXT_CONTENT_VERSION}, \
                 updated_by = $7, updated_at = now() \
                 WHERE id = $4 AND org = $5 AND tenant_id = $6 AND kind = 'file' AND deleted_at IS NULL \
                 RETURNING {NODE_COLS}"
            );
            let row: NodeRow = sqlx::query_as(&sql)
                .bind(blob.sha256)
                .bind(blob.size)
                .bind(blob.content_type)
                .bind(target)
                .bind(&ctx.org)
                .bind(&ctx.tenant_id)
                .bind(&ctx.principal.id)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or(StorageError::NotFound)?;
            (row_to_node(row)?, WriteOp::Update, false)
        } else {
            // 新規作成（write_file_core の作成経路と同一）。
            let node = self
                .create_file_node(
                    tx,
                    ctx,
                    Some(parent_id),
                    name,
                    blob.sha256,
                    blob.size,
                    blob.content_type,
                )
                .await?;
            (node, WriteOp::Create, true)
        };

        self.record_version(
            tx,
            ctx,
            node.id,
            node.version,
            blob.sha256,
            blob.size,
            blob.content_type,
        )
        .await?;
        let action = if created {
            "file.write.workspace.create"
        } else {
            "file.write.workspace.update"
        };
        audit::record_on(
            tx,
            ctx,
            AuditEntry {
                action,
                object_type: "file",
                object_id: &node.id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "sha256": blob.sha256, "size": blob.size, "version": node.version }),
            },
            Chain::Yes,
        )
        .await?;
        event::emit_on(
            tx,
            ctx,
            WriteEvent {
                node_id: node.id,
                version: node.version,
                op,
                payload: json!({ "kind": "file", "blob_sha256": blob.sha256, "size": blob.size,
                    "parent_id": parent_id.to_string() }),
            },
            trace_id,
        )
        .await?;
        Ok((node, created))
    }

    /// FGA tuple（新規のみ）を書いてから commit する。commit 失敗時は書いた tuple を revoke する。
    ///
    /// 新規作成時のみ FGA tuple（owner＋parent）を書く（write_file_core と同じ順序・補償）。
    /// 更新は既存ノードの tuple を流用するため触らない。
    async fn commit_write_at(
        &self,
        tx: sqlx::Transaction<'_, sqlx::Postgres>,
        ctx: &AuthContext,
        parent_id: Uuid,
        node: &Node,
        created: bool,
    ) -> Result<WriteAtOutcome, StorageError> {
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

/// ロック下で解決した既存ファイル（追記は `blob_sha256` を base として読む）。
struct ExistingFile {
    node_id: Uuid,
    blob_sha256: Option<String>,
}

/// 確定する内容の blob 事実（引数の並びを間違えないよう構造体で束ねる）。
struct BlobFacts<'a> {
    sha256: &'a str,
    size: i64,
    content_type: &'a str,
    object_key: &'a str,
}
