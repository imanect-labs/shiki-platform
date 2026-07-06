//! StorageService: サーバ内バイト直書き/読み戻し（Task 4.12 Stage A・HTTP 非公開）。
//!
//! サンドボックス成果物の保存など「サーバが既にバイトを所持している」経路のための内部 API。
//! 呼び出しは shiki-server 内のツール（発話ユーザーの `AuthContext`）に限り、HTTP には公開しない。
//! presigned 経路（declare→finalize）と同一の不変条件を保つ:
//! - 認可（配置先 editor@folder / member@org、読み取りは viewer@file）を必ず通す。
//! - content-addressing（sha256 キー・blob dedup・refcount＝node_version 行数）。
//! - 監査・書込イベント（RAG 増分索引トリガ）を同一 txn で原子的に記録する。
//! - FGA tuple は commit 前に書き、失敗時は revoke して DB/FGA の不整合を残さない。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;

use crate::content_address::sha256_hex;

impl StorageService {
    /// バイト列を新規ファイルとして保存する（内部書き込み・presigned 経路と同一不変条件）。
    ///
    /// finalize の新規作成経路（`finalize.rs`）を「バイトを既に所持している」前提で写した実装。
    /// staging/incoming を経ず、sha256 をメモリ上で計算して content-addressed に直接昇格する
    /// （バイト所持＝所持証明そのものなので、宣言ハッシュ照合は不要）。
    // blob 昇格→ノード化→FGA tuple を finalize と同じ順序・同じ補償で行うため長め。
    // 段階の不変条件を一望できるよう一体に保つ（finalize.rs と対称）。
    #[allow(clippy::too_many_lines)]
    pub async fn write_file_internal(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        bytes: &[u8],
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        validate_name(name)?;
        let size = i64::try_from(bytes.len())
            .map_err(|_| StorageError::Invalid("size が大きすぎます".into()))?;
        // presigned 経路（declare）と同じ容量ガード（ストレージ枯渇防止）。
        if size > self.max_upload_size {
            return Err(StorageError::Invalid(format!(
                "size が上限を超えています（最大 {} バイト）",
                self.max_upload_size
            )));
        }

        // 配置先の認可（finalize の新規作成経路と同一の要求）。
        match parent_id {
            Some(p) => {
                self.require(
                    ctx,
                    Relation::Editor,
                    &ctx.ns().folder(&p.to_string()),
                    "file.write.internal",
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
                    "file.write.internal",
                    "organization",
                    &ctx.org,
                    trace_id,
                )
                .await?;
            }
        }

        // content-addressing: バイトを所持しているのでメモリ上でハッシュし、新規 blob のみ書き込む。
        // 既存 blob があれば上書きしない（並行 finalize が参照する共有本体を壊さない・dedup）。
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

        // メタ確定（finalize の新規作成 txn と同一の順序・補償）。失敗時、新規 hash の孤児本体は
        // GC（blob 行を持たないキーの掃除）に委ねる（finalize と同じ方針・Lb76C 対称）。
        let mut tx = self.db.begin().await?;
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
        let node = self
            .create_file_node(&mut tx, ctx, parent_id, name, &sha256, size, content_type)
            .await?;
        // 初版（version 1）を履歴に記録する（content-addressing で同一内容は blob 共有）。
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
        // 監査・書込イベントは FGA tuple を書く前に済ませる（finalize と同じ理由:
        // post-tuple の失敗で FGA tuple だけ残る不整合を避ける）。
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: "file.write.internal",
                object_type: "file",
                object_id: &node.id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "sha256": sha256, "size": size }),
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
                    "blob_sha256": sha256,
                    "size": size,
                    "parent_id": parent_id.map(|p| p.to_string()),
                }),
            },
            trace_id,
        )
        .await?;
        // DB 側が確定したので FGA tuple を書く（commit 前）。
        let file_obj = ctx.ns().file(&node.id.to_string());
        // owner tuple（失敗時は tx を drop でロールバック＝何も残らない）。
        self.authz
            .write_tuple(&ctx.subject(), Relation::Owner, &file_obj)
            .await
            .map_err(StorageError::Authz)?;
        // parent tuple（folder 配下のみ）。失敗時は owner を revoke してロールバック。
        if let Some(p) = parent_id {
            if let Err(e) = self
                .authz
                .write_tuple(
                    &Subject::object(&ctx.ns().folder(&p.to_string())),
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
        // commit 失敗時は書いた owner/parent tuple を revoke して FGA を作成前へ戻す。
        if let Err(e) = tx.commit().await {
            let _ = self
                .authz
                .delete_tuple(&ctx.subject(), Relation::Owner, &file_obj)
                .await;
            if let Some(p) = parent_id {
                let _ = self
                    .authz
                    .delete_tuple(
                        &Subject::object(&ctx.ns().folder(&p.to_string())),
                        Relation::Parent,
                        &file_obj,
                    )
                    .await;
            }
            return Err(StorageError::from(e));
        }
        Ok(node)
    }

    /// ファイル内容をサーバ内で読み戻す（内部読み取り・viewer 認可＋監査つき）。
    ///
    /// サイズは書き込み時に `max_upload_size` で有界化済みだが、（旧データ・別経路の巨大 blob に
    /// 備えて）読み戻し前にもノードの `size_bytes` で再確認し、無制限のメモリ載せを防ぐ。
    pub async fn read_file_internal(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(Node, Vec<u8>), StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        self.require_read(
            ctx,
            &ctx.ns().file(&file_id.to_string()),
            "file.read.internal",
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        if node.size_bytes.unwrap_or(0) > self.max_upload_size {
            return Err(StorageError::Invalid(format!(
                "size が内部読み取りの上限を超えています（最大 {} バイト）",
                self.max_upload_size
            )));
        }
        let sha = node.blob_sha256.as_ref().ok_or(StorageError::NotFound)?;
        let key = blob_object_key(&ctx.tenant_id, &ctx.org, sha);
        let bytes = self.store.get_object(&key).await?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "file.read.internal",
                    object_type: "file",
                    object_id: &file_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "size": bytes.len() }),
                },
            )
            .await?;
        Ok((node, bytes))
    }
}
