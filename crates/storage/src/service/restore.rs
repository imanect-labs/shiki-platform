//! StorageService: ゴミ箱/バージョンからの復元。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::AuditAction;

impl StorageService {
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
            &ctx.ns().file(&file_id.to_string()),
            AuditAction::FileRestore,
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        // 祖先に削除済みフォルダがあると、単体復元してもツリーから到達不能なまま直リンクだけで
        // 露出する（subtree 削除の隠蔽が破れる）。祖先が全て生存している時のみ復元を許す。
        let ancestor_deleted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM node_closure c JOIN node n ON n.id = c.ancestor AND n.tenant_id = c.tenant_id \
             WHERE c.tenant_id = $2 AND c.descendant = $1 AND c.ancestor <> $1 AND n.deleted_at IS NOT NULL)",
        )
        .bind(file_id)
        .bind(&ctx.tenant_id)
        .fetch_one(&mut *tx)
        .await?;
        if ancestor_deleted {
            return Err(StorageError::Invalid(
                "祖先フォルダが削除済みのため復元できません（先に親フォルダを復元してください）"
                    .into(),
            ));
        }
        // deleted_at=NULL に戻す。生存兄弟と同名なら部分ユニークが効き Conflict になる。
        // version も進めて書込イベントの冪等キー (node_id, version) を一意に保つ。
        let sql = format!(
            "UPDATE node SET deleted_at = NULL, updated_at = now(), version = version + 1, \
             updated_by = $4 \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NOT NULL \
             RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(file_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .bind(&ctx.principal.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StorageError::NotFound)?;
        // soft_delete で refcount を減らさないので、復元でも増やさない（対称・LbvQZ）。
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::FileRestore,
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({}),
            },
            Chain::Yes,
        )
        .await?;
        // 書込イベント（Task 1.8）。削除で除去した索引を購読側が再構築する。
        event::emit_on(
            &mut tx,
            ctx,
            WriteEvent {
                node_id: file_id,
                version: row.version,
                op: WriteOp::Restore,
                payload: json!({ "kind": "file", "blob_sha256": row.blob_sha256 }),
            },
            trace_id,
        )
        .await?;
        tx.commit().await?;
        row_to_node(row)
    }

    /// ゴミ箱からのフォルダ復元（editor 権限・同名衝突は Conflict）。
    ///
    /// `soft_delete_folder` はサブツリーを 1 txn で同一 `deleted_at` にする。復元は
    /// **その削除と同時に消えた配下**（＝同一 `deleted_at`）だけを戻す。独立に先に削除された
    /// 配下（別タイムスタンプ）は巻き込まない。祖先が削除済みなら（到達不能の露出を避けるため）拒否。
    pub async fn restore_folder(
        &self,
        ctx: &AuthContext,
        folder_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let node = self.load_node(ctx, folder_id, true).await?;
        if node.kind != NodeKind::Folder {
            return Err(StorageError::NotFound);
        }
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().folder(&folder_id.to_string()),
            AuditAction::FolderRestore,
            "folder",
            &folder_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        // 祖先（自身を除く）に削除済みフォルダがあれば、単体復元してもツリーから到達不能になる。
        let ancestor_deleted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM node_closure c JOIN node n ON n.id = c.ancestor AND n.tenant_id = c.tenant_id \
             WHERE c.tenant_id = $2 AND c.descendant = $1 AND c.ancestor <> $1 AND n.deleted_at IS NOT NULL)",
        )
        .bind(folder_id)
        .bind(&ctx.tenant_id)
        .fetch_one(&mut *tx)
        .await?;
        if ancestor_deleted {
            return Err(StorageError::Invalid(
                "祖先フォルダが削除済みのため復元できません（先に親フォルダを復元してください）"
                    .into(),
            ));
        }
        // 削除バッチの識別子＝フォルダ自身の deleted_at（同一 txn の now() で配下と一致）。
        let batch: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT deleted_at FROM node \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NOT NULL",
        )
        .bind(folder_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        let Some(batch) = batch else {
            return Err(StorageError::NotFound);
        };
        // 同一バッチ（同時削除）の配下を一括復元する。version も進めて書込イベントの冪等キーを保つ。
        // 生存兄弟と同名なら部分ユニークが効き Conflict になる。
        let affected: Vec<(Uuid, i64)> = sqlx::query_as(
            "UPDATE node SET deleted_at = NULL, updated_at = now(), version = version + 1, \
             updated_by = $5 \
             WHERE org = $1 AND tenant_id = $2 AND deleted_at = $3 \
               AND id IN (SELECT descendant FROM node_closure WHERE tenant_id = $2 AND ancestor = $4) \
             RETURNING id, version",
        )
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(batch)
        .bind(folder_id)
        .bind(&ctx.principal.id)
        .fetch_all(&mut *tx)
        .await?;
        let folder_version = affected
            .iter()
            .find(|(id, _)| *id == folder_id)
            .map(|(_, v)| *v)
            .ok_or(StorageError::NotFound)?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::FolderRestore,
                object_type: "folder",
                object_id: &folder_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "subtree_count": affected.len() }),
            },
            Chain::Yes,
        )
        .await?;
        event::emit_on(
            &mut tx,
            ctx,
            WriteEvent {
                node_id: folder_id,
                version: folder_version,
                op: WriteOp::Restore,
                payload: json!({ "kind": "folder", "subtree_count": affected.len() }),
            },
            trace_id,
        )
        .await?;
        tx.commit().await?;
        // 復元後のフォルダ自身を返す（最新メタ）。
        self.load_node(ctx, folder_id, false).await
    }

    /// 過去版を**新しい版として**復元する（editor 権限）。
    ///
    /// 復元は履歴を巻き戻さず、対象版の blob を指す新版（version+1）を追記する
    /// （AC: 復元が新しい版として記録される・履歴を壊さない）。content-addressing により
    /// 実体はコピーされず blob を共有する（refcount +1）。
    pub async fn restore_version(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().file(&file_id.to_string()),
            AuditAction::FileVersionRestore,
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        lock_file_row(&mut tx, ctx, file_id).await?;
        let restored = self
            .restore_version_in_tx(&mut tx, ctx, file_id, version, trace_id, json!({}))
            .await?;
        tx.commit().await?;
        Ok(restored)
    }

    /// AI 等による編集を**取り消す**（#1049・editor 権限）。
    ///
    /// `edit_versions`（取り消す編集が作った内容版の集合・空でないこと）のうち最も古い版の**直前の
    /// 内容版**を、[`restore_version`](Self::restore_version) と同じく**新しい版として**復元する（履歴は
    /// 残る＝取り消しの取り消しもできる）。同じ回答で同じファイルを複数回編集した場合は、その全版を渡すと
    /// まとめて取り消せる。
    ///
    /// - 現在の内容が既に直前の版と同じ → 何もしない（[`RevertOutcome::AlreadyReverted`]）。二度押し・
    ///   再読込後の再実行で版を無駄に積まない。
    /// - 直前の版より後に、`edit_versions` に含まれない**内容版**がある（他の人・後の編集）→ `force` で
    ///   なければ [`StorageError::VersionConflict`]（`current` は最新の内容版）。後から入った他の編集を
    ///   黙って消さない。判定は内容版（`node_version`）で行うので、名前変更・移動のような内容の変わらない
    ///   版上げでは競合にならない。
    /// - `edit_versions` のいずれかが無い／提案版／その前に内容版が無い（新規作成の版）→
    ///   [`StorageError::NotFound`]。
    pub async fn revert_edit(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        edit_versions: &[i64],
        force: bool,
        trace_id: Option<&str>,
    ) -> Result<RevertOutcome, StorageError> {
        let Some(&earliest) = edit_versions.iter().min() else {
            return Err(StorageError::Invalid("取り消す版を指定してください".into()));
        };
        let node = self.load_node(ctx, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require(
            ctx,
            Relation::Editor,
            &ctx.ns().file(&file_id.to_string()),
            AuditAction::FileVersionRestore,
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await?;
        // 行ロックの後に読む（load_node との間に入った編集を見落とさない）。
        let (_, current_sha) = lock_file_row(&mut tx, ctx, file_id).await?;
        // ブラウザの Office エディタで開かれている間は戻さない。WOPI の PutFile は版を検査しないので、
        // ここで差し替えてもエディタの次の自動保存がメモリ上の（AI の編集を含む）内容で書き戻し、
        // 取り消しが黙って消える。ロックは助言的（期限切れは無視）で、判定は office と同じ条件。
        let locked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM office_lock \
             WHERE file_id = $1 AND tenant_id = $2 AND expires_at > now())",
        )
        .bind(file_id)
        .bind(&ctx.tenant_id)
        .fetch_one(&mut *tx)
        .await?;
        if locked {
            return Err(StorageError::Locked);
        }
        // 取り消す編集の版がすべて実在する（提案でない内容版）こと。
        let found: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM node_version \
             WHERE node_id = $1 AND org = $2 AND tenant_id = $3 AND version = ANY($4) AND NOT is_proposal",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(edit_versions)
        .fetch_one(&mut *tx)
        .await?;
        let mut distinct = edit_versions.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        if usize::try_from(found).unwrap_or(0) != distinct.len() {
            return Err(StorageError::NotFound);
        }
        // 最も古い編集の直前の内容版（node_version_node_idx (node_id, version desc) で 1 行だけ引く）。
        let target: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, blob_sha256 FROM node_version \
             WHERE node_id = $1 AND org = $2 AND tenant_id = $3 AND version < $4 AND NOT is_proposal \
             ORDER BY version DESC LIMIT 1",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(earliest)
        .fetch_optional(&mut *tx)
        .await?;
        let (target_version, target_sha) = target.ok_or(StorageError::NotFound)?;
        if current_sha.as_deref() == Some(target_sha.as_str()) {
            // 既に編集前の内容（取り消し済み・手で戻した等）。版は積まない。
            tx.rollback().await?;
            let node = self.load_node(ctx, file_id, false).await?;
            return Ok(RevertOutcome::AlreadyReverted(node));
        }
        // 直前の版より後の内容版のうち、取り消す編集でないもの＝後から入った他の編集があるか。
        // 履歴の件数に比例する取得をしないよう、有無と最新の版番号だけを 1 行で集計する（行ロック中）。
        if !force {
            let (foreign, latest): (bool, Option<i64>) = sqlx::query_as(
                "SELECT COALESCE(bool_or(NOT (version = ANY($5))), false), max(version) \
                 FROM node_version \
                 WHERE node_id = $1 AND org = $2 AND tenant_id = $3 AND version > $4 AND NOT is_proposal",
            )
            .bind(file_id)
            .bind(&ctx.org)
            .bind(&ctx.tenant_id)
            .bind(target_version)
            .bind(&distinct)
            .fetch_one(&mut *tx)
            .await?;
            if foreign {
                return Err(StorageError::VersionConflict {
                    current: latest.unwrap_or(target_version),
                });
            }
        }
        let restored = self
            .restore_version_in_tx(
                &mut tx,
                ctx,
                file_id,
                target_version,
                trace_id,
                json!({ "reverted_edit_versions": distinct, "forced": force }),
            )
            .await?;
        tx.commit().await?;
        Ok(RevertOutcome::Reverted {
            node: restored,
            restored_from_version: target_version,
        })
    }

    /// 行ロック済みの tx 内で、`version` の内容を新しい版として復元する（restore / revert 共通）。
    /// `extra` は監査メタデータに足す項目（取り消しの由来など）。
    async fn restore_version_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ctx: &AuthContext,
        file_id: Uuid,
        version: i64,
        trace_id: Option<&str>,
        extra: serde_json::Value,
    ) -> Result<Node, StorageError> {
        // 復元元の版の内容（blob/size/content_type）を取得する。
        // 提案バージョン（is_proposal）は復元経路の対象外（採用は adopt_proposal_version 経由）。
        let src: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT blob_sha256, size_bytes, content_type FROM node_version \
             WHERE node_id = $1 AND org = $2 AND tenant_id = $3 AND version = $4 \
               AND NOT is_proposal",
        )
        .bind(file_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(version)
        .fetch_optional(&mut **tx)
        .await?;
        let (sha, size, content_type) = src.ok_or(StorageError::NotFound)?;
        // 復元先の blob を refcount +1（新版が参照するため）。実体はオブジェクトストアに既存。
        let final_key = blob_object_key(&ctx.tenant_id, &ctx.org, &sha);
        self.bump_blob(
            tx,
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
             SET blob_sha256 = $1, size_bytes = $2, content_type = $3, version = {NEXT_CONTENT_VERSION}, \
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
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(StorageError::NotFound)?;
        let restored = row_to_node(row)?;
        self.record_version(
            tx,
            ctx,
            restored.id,
            restored.version,
            &sha,
            size,
            &content_type,
        )
        .await?;
        audit::record_on(
            tx,
            ctx,
            AuditEntry {
                action: AuditAction::FileVersionRestore,
                object_type: "file",
                object_id: &file_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: {
                    let mut m = json!({ "restored_from_version": version, "new_version": restored.version });
                    if let (Some(m), serde_json::Value::Object(extra)) = (m.as_object_mut(), extra) {
                        m.extend(extra);
                    }
                    m
                },
            },
            Chain::Yes,
        )
        .await?;
        event::emit_on(
            tx,
            ctx,
            WriteEvent {
                node_id: file_id,
                version: restored.version,
                op: WriteOp::Restore,
                payload: json!({
                    "kind": "file",
                    "blob_sha256": sha,
                    "restored_from_version": version,
                }),
            },
            trace_id,
        )
        .await?;
        Ok(restored)
    }

    // --- 共有（ReBAC: Task 1.6） ---
}

/// [`StorageService::revert_edit`] の結果。
#[derive(Debug, Clone)]
pub enum RevertOutcome {
    /// 編集前の版を新しい版として復元した。
    Reverted {
        node: Node,
        /// 内容の復元元（取り消した編集の直前の版）。
        restored_from_version: i64,
    },
    /// 現在の内容が既に編集前と同じだった（版は積んでいない）。
    AlreadyReverted(Node),
}

/// 対象ファイルの行を `FOR UPDATE` でロックし、ロック後の（版, blob）を返す（並行更新と直列化）。
async fn lock_file_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    file_id: Uuid,
) -> Result<(i64, Option<String>), StorageError> {
    sqlx::query_as(
        "SELECT version, blob_sha256 FROM node \
         WHERE id = $1 AND org = $2 AND tenant_id = $3 AND kind = 'file' AND deleted_at IS NULL \
         FOR UPDATE",
    )
    .bind(file_id)
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StorageError::NotFound)
}
