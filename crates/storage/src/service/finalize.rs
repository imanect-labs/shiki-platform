//! StorageService: アップロード finalize（内容ハッシュ検証→content-addressed 昇格）。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::AuditAction;
use crate::tuple_commit::{commit_tx_with_tuples, node_creation_tuples};

use super::upload::{lock_file_for_content, lock_parent_for_create, BlobBody};

impl StorageService {
    /// finalize: staging を読み戻して内容ハッシュを検証し、content-addressed に昇格してノード化する。
    // staging 読み戻し → ハッシュ検証 → content-addressed 昇格 → ノード化 ＋ FGA タプルを
    // 単一 txn で原子的に行うため長め。段階の不変条件を一望できるよう一体に保つ。
    #[allow(clippy::too_many_lines)]
    pub async fn finalize_upload(
        &self,
        ctx: &AuthContext,
        upload_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        // 所有者束縛: アップロードを宣言した本人のみ finalize できる（upload_id 漏洩での横取り防止）。
        // tenant_id も条件に含め、同一 org 内でも tenant 跨ぎを遮断する。
        let pending: PendingRow = sqlx::query_as(
            "SELECT parent_id, name, content_type, declared_sha256, declared_size, staging_key, target_node_id, base_version \
             FROM pending_upload \
             WHERE upload_id = $1 AND org = $2 AND tenant_id = $3 AND created_by = $4",
        )
        .bind(upload_id)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .bind(&ctx.principal().id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(StorageError::NotFound)?;

        // finalize も認可を再確認（capability を持つだけでなく実権限も要る）。
        let label = upload_id.to_string();
        match pending.target_node_id {
            // 内容更新: 対象ファイルの editor@file を再確認し、対象が生存していることを保証する。
            Some(target) => {
                let existing = self.load_node(ctx, target, false).await?;
                if existing.kind != NodeKind::File {
                    return Err(StorageError::NotFound);
                }
                self.require(
                    ctx,
                    Relation::Editor,
                    &ctx.ns().file(&target.to_string()),
                    AuditAction::FileContentUpdate,
                    "file",
                    &target.to_string(),
                    trace_id,
                )
                .await?;
            }
            // 新規作成: 配置先（フォルダ or org ルート）の権限を再確認する。
            None => match pending.parent_id {
                Some(p) => {
                    self.require(
                        ctx,
                        Relation::Editor,
                        &ctx.ns().folder(&p.to_string()),
                        AuditAction::FileUploadFinalize,
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
                        &ctx.ns().organization(ctx.org()),
                        AuditAction::FileUploadFinalize,
                        "organization",
                        ctx.org(),
                        trace_id,
                    )
                    .await?;
                }
            },
        }

        // TOCTOU 回避: staging はクライアントが presigned PUT で上書きでき得るため、
        // 不変な incoming へ server-side copy し、以降の検証・昇格は incoming 基準で行う。
        if !self.store.exists(&pending.staging_key).await? {
            return Err(StorageError::Integrity(format!(
                "staging オブジェクトが存在しません（アップロード未完了 label={label}）"
            )));
        }
        let incoming_key = incoming_object_key(ctx.tenant_id(), ctx.org(), &label);
        self.store.copy(&pending.staging_key, &incoming_key).await?;

        // 不変スナップショットを再ハッシュし、宣言値と照合（client バイトを信頼しない）。
        let (actual_sha, actual_size) = self.store.read_and_hash(&incoming_key).await?;
        if actual_sha != pending.declared_sha256 || actual_size as i64 != pending.declared_size {
            let _ = self.store.delete(&incoming_key).await;
            let _ = self.store.delete(&pending.staging_key).await;
            let _ =
                sqlx::query("DELETE FROM pending_upload WHERE upload_id = $1 AND tenant_id = $2")
                    .bind(upload_id)
                    .bind(ctx.tenant_id())
                    .execute(&self.db)
                    .await;
            return Err(StorageError::Integrity(format!(
                "宣言ハッシュ/サイズと実体が一致しません (label={label})"
            )));
        }

        // 既存の有効 blob を上書きしない（content-addressed への昇格は新規 blob の時だけ）。
        // 既存 blob があるなら finalize は実バイトを所持した上での正当な dedup。
        let final_key = blob_object_key(ctx.tenant_id(), ctx.org(), &actual_sha);
        let blob_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3)",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
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

        // メタ確定を 1 txn 境界で行う。内容更新（target あり）と新規作成（target なし）で分岐する。
        let tx_result: Result<Node, StorageError> = match pending.target_node_id {
            Some(target) => {
                self.finalize_content_update(
                    ctx,
                    target,
                    upload_id,
                    &actual_sha,
                    actual_size as i64,
                    &pending.content_type,
                    &final_key,
                    &incoming_key,
                    pending.base_version,
                    trace_id,
                )
                .await
            }
            // 新規作成: blob upsert + node + FGA tuple + 版記録 + イベント + pending 削除 + 監査。
            // FGA tuple は **commit 前**に書き、parent 失敗・commit 失敗のどちらでも書けた tuple を
            // revoke して DB/FGA の不整合（auth tuple 欠落・owner 残留）を残さない（tuple_commit）。
            None => {
                async {
                    let mut tx = self.db.begin().await?;
                    // 親を Tx の中で確かめ直して押さえる（ロックの順 node → blob → 監査のチェーン・bump_blob より前）。
                    // declare・上の ensure_folder の後に親が論理削除されても、削除したフォルダの下に生きたファイルを残さない。
                    if let Some(p) = pending.parent_id {
                        lock_parent_for_create(&mut tx, ctx, p).await?;
                    }
                    // 行を新しく作ったら、ロックの中で本体を確かめ直し、無ければ incoming（commit まで残す）から写す。
                    self.bump_blob(
                        &mut tx,
                        ctx.tenant_id(),
                        ctx.org(),
                        &actual_sha,
                        actual_size as i64,
                        &pending.content_type,
                        &final_key,
                        BlobBody::CopyFrom(&incoming_key),
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
                    // 初版（version 1）を履歴に記録する（content-addressing で同一内容は blob 共有）。
                    self.record_version(
                        &mut tx,
                        ctx,
                        node.id,
                        node.version,
                        &actual_sha,
                        actual_size as i64,
                        &pending.content_type,
                    )
                    .await?;
                    // pending を**この txn の先頭処理として claim**する（rows_affected=0 は
                    // 二重 finalize＝既に確定済みなので NotFound）。並行/再試行の finalize が
                    // 同一ノードを二重に作るのを防ぐ。
                    let claimed = sqlx::query(
                        "DELETE FROM pending_upload WHERE upload_id = $1 AND tenant_id = $2",
                    )
                    .bind(upload_id)
                    .bind(ctx.tenant_id())
                    .execute(&mut *tx)
                    .await?;
                    if claimed.rows_affected() == 0 {
                        return Err(StorageError::NotFound);
                    }
                    // 監査・書込イベントは **FGA tuple を書く前**に済ませる。post-tuple の fallible
                    // call で失敗すると DB はロールバックされても FGA tuple だけ残り孤立するため、
                    // 外部副作用（FGA 書込）の手前で DB 側の操作を全て確定させる。
                    audit::record_on(
                        &mut tx,
                        ctx,
                        AuditEntry {
                            action: AuditAction::FileUploadFinalize,
                            object_type: "file",
                            object_id: &node.id.to_string(),
                            decision: Decision::Allow,
                            trace_id,
                            metadata: json!({ "sha256": actual_sha, "size": actual_size }),
                        },
                        Chain::Yes,
                    )
                    .await?;
                    // 書込イベント（後段 RAG 増分索引のトリガ）を同一 txn で発行する（Task 1.8）。
                    event::emit_on(
                        &mut tx,
                        ctx,
                        WriteEvent {
                            node_id: node.id,
                            version: node.version,
                            op: WriteOp::Create,
                            payload: json!({
                                "kind": "file",
                                "blob_sha256": actual_sha,
                                "size": actual_size,
                                "parent_id": pending.parent_id.map(|p| p.to_string()),
                            }),
                        },
                        trace_id,
                    )
                    .await?;
                    // DB 側が確定したので owner・parent の FGA tuple を書いてから commit する。区間は別の
                    // タスクで最後まで走る（補償の決まりは tuple_commit・BUG-134）。
                    let file_obj = ctx.ns().file(&node.id.to_string());
                    commit_tx_with_tuples(
                        self.authz.clone(),
                        tx,
                        node_creation_tuples(ctx, pending.parent_id, &file_obj),
                    )
                    .await?;
                    Ok(node)
                }
                .await
            }
        };

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
                let _ = sqlx::query(
                    "DELETE FROM pending_upload WHERE upload_id = $1 AND tenant_id = $2",
                )
                .bind(upload_id)
                .bind(ctx.tenant_id())
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

    /// 既存ファイルの内容を新版へ差し替える（finalize の内容更新経路・Task 1.7）。
    ///
    /// 対象ファイルを行ロック → blob を bump（refcount +1）→ blob/size/content_type と version を更新 →
    /// 新版を履歴に記録 → 書込イベント（op=update）→ 監査 を 1 txn で原子的に確定する（ロックの順
    /// node → blob → 監査のチェーン）。owner/parent タプルは既存ファイルのものを流用するため触らない。古い版の
    /// blob は減算しない（履歴＝安全網のため download/restore 可能に保つ・LbvQZ と対称）。
    #[allow(clippy::too_many_arguments)] // finalize から確定済みのメタ一式を受け取る。
    async fn finalize_content_update(
        &self,
        ctx: &AuthContext,
        target: Uuid,
        upload_id: Uuid,
        sha256: &str,
        size: i64,
        content_type: &str,
        final_key: &str,
        incoming_key: &str,
        base_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let mut tx = self.db.begin().await?;
        // pending を**この txn の先頭で claim**する（rows_affected=0 は二重 finalize＝既に確定済み
        // なので NotFound）。並行/再試行の finalize が 1 アップロードを 2 版に増やすのを防ぐ。
        let claimed =
            sqlx::query("DELETE FROM pending_upload WHERE upload_id = $1 AND tenant_id = $2")
                .bind(upload_id)
                .bind(ctx.tenant_id())
                .execute(&mut *tx)
                .await?;
        if claimed.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        // 対象の行を bump_blob より前に押さえる（ロックの順 node → blob → 監査のチェーン）。並行内容更新の
        // lost-update もここで防ぐ。base_version があれば押さえた後の版と比べる（間に入った保存を上書きしない）。
        lock_file_for_content(&mut tx, ctx, target, base_version).await?;
        self.bump_blob(
            &mut tx,
            ctx.tenant_id(),
            ctx.org(),
            sha256,
            size,
            content_type,
            final_key,
            BlobBody::CopyFrom(incoming_key),
        )
        .await?;
        // 行は押さえてあるが、条件は前と同じに保つ（base_version があれば「その版のままなら」）。
        let sql = format!(
            "UPDATE node \
             SET blob_sha256 = $1, size_bytes = $2, content_type = $3, version = {NEXT_CONTENT_VERSION}, \
             updated_by = $7, updated_at = now() \
             WHERE id = $4 AND org = $5 AND tenant_id = $6 AND kind = 'file' AND deleted_at IS NULL \
             AND ($8::bigint IS NULL OR version = $8) \
             RETURNING {NODE_COLS}"
        );
        let row: Option<NodeRow> = sqlx::query_as(&sql)
            .bind(sha256)
            .bind(size)
            .bind(content_type)
            .bind(target)
            .bind(ctx.org())
            .bind(ctx.tenant_id())
            .bind(&ctx.principal().id)
            .bind(base_version)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            // 版が変わっていたのか、ファイルが消えたのかを分ける（txn は破棄＝pending は残る）。
            if base_version.is_some() {
                let current: Option<i64> = sqlx::query_scalar(
                    "SELECT version FROM node \
                     WHERE id = $1 AND org = $2 AND tenant_id = $3 AND kind = 'file' AND deleted_at IS NULL",
                )
                .bind(target)
                .bind(ctx.org())
                .bind(ctx.tenant_id())
                .fetch_optional(&mut *tx)
                .await?;
                if let Some(current) = current {
                    return Err(StorageError::VersionConflict { current });
                }
            }
            return Err(StorageError::NotFound);
        };
        let node = row_to_node(row)?;
        self.record_version(
            &mut tx,
            ctx,
            node.id,
            node.version,
            sha256,
            size,
            content_type,
        )
        .await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::FileContentUpdate,
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
                // 親フォルダはファイル保存トリガ・wait(event) の祖先束縛の照合に使う（作成と同じ・BUG-28）。
                payload: json!({ "kind": "file", "blob_sha256": sha256, "size": size,
                    "parent_id": node.parent_id.map(|p| p.to_string()) }),
            },
            trace_id,
        )
        .await?;
        tx.commit().await?;
        Ok(node)
    }
}
