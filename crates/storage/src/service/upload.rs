//! StorageService: アップロード declare/ダウンロードURL発行 と blob/version 補助。
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::AuditAction;

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
        self.begin_upload_with_base(
            ctx,
            parent_id,
            name,
            content_type,
            declared_sha256,
            declared_size,
            target_node_id,
            None,
            trace_id,
        )
        .await
    }

    /// [`Self::begin_upload`] に、内容更新の楽観ロック（`base_version`）を足したもの（#827）。
    ///
    /// `base_version` は `target_node_id` と組で使う（新規作成では無視）。申告した版が今の版と
    /// 違えば declare の時点で [`StorageError::VersionConflict`] にし、finalize でも版更新を
    /// `version = base_version` の条件付きにして、その間に入った保存を上書きしない。
    #[allow(clippy::too_many_arguments)] // 宣言メタ一式は凝集した 1 操作の引数。
    pub async fn begin_upload_with_base(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        content_type: &str,
        declared_sha256: &str,
        declared_size: i64,
        target_node_id: Option<Uuid>,
        base_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<UploadTicket, StorageError> {
        let base_version = target_node_id.and(base_version);
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
                AuditAction::FileUploadUrlIssue,
                "file",
                &target.to_string(),
                trace_id,
            )
            .await?;
            // 開いた後に他の保存が入っていれば、バイトを送らせる前に止める。
            if let Some(base) = base_version {
                if existing.version != base {
                    return Err(StorageError::VersionConflict {
                        current: existing.version,
                    });
                }
            }
            (existing.parent_id, existing.name)
        } else {
            validate_name(name)?;
            match parent_id {
                Some(p) => {
                    self.require(
                        ctx,
                        Relation::Editor,
                        &ctx.ns().folder(&p.to_string()),
                        AuditAction::FileUploadUrlIssue,
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
                        &ctx.ns().organization(ctx.org()),
                        AuditAction::FileUploadUrlIssue,
                        "organization",
                        ctx.org(),
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
        let staging_key = staging_object_key(ctx.tenant_id(), ctx.org(), &upload_id.to_string());
        sqlx::query(
            "INSERT INTO pending_upload \
             (upload_id, org, tenant_id, parent_id, name, content_type, declared_sha256, declared_size, staging_key, created_by, target_node_id, base_version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(upload_id)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .bind(effective_parent_id)
        .bind(&effective_name)
        .bind(content_type)
        .bind(declared_sha256)
        .bind(declared_size)
        .bind(&staging_key)
        .bind(&ctx.principal().id)
        .bind(target_node_id)
        .bind(base_version)
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
                    action: AuditAction::FileUploadUrlIssue,
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
            AuditAction::FileDownloadUrlIssue,
            "file",
            &file_id.to_string(),
            trace_id,
        )
        .await?;
        let sha = node.blob_sha256.as_ref().ok_or(StorageError::NotFound)?;
        let key = blob_object_key(ctx.tenant_id(), ctx.org(), sha);
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
                    action: AuditAction::FileDownloadUrlIssue,
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
    ///
    /// 先に [`lock_blob`] で `(tenant_id, org, sha256)` を Tx の終わりまで押さえる。完全削除の片付け
    /// （`purge_pending`）は同じロックの中で「blob の行が無い」を確かめてから本体を消すので、行を作ったこの Tx が
    /// 終わるまで本体は消されない。行を**新しく作った**（[`BlobBump::created`]）ときは、同じロックの中で本体が
    /// あるかを確かめ直し、無ければ `body` から置く（書き込みの put はロックの外で起きるので、掃除が行の無いのを
    /// 見てから消すまでの間に置いた本体も消えうる。ロックの中で確かめ直せば、最後には本体が残る・PIT-14）。
    ///
    /// ロックの順は **node → blob → 監査のチェーン**。node の行を押さえる書き込みは、この関数より前に押さえる
    /// （[`lock_parent_for_create`] / [`lock_file_for_content`]）。完全削除も同じ順で取るので、順が逆の書き込みと
    /// 重なってデッドロックにならない。完全削除は blob を advisory lock ではなく行ロック（sha256 の昇順・
    /// `FOR UPDATE`）で押さえるので、ここの upsert はその行ロックを待つ。完全削除が行を消して commit していれば、
    /// upsert は行を作り直し（`created`）、本体を確かめ直す。
    #[allow(clippy::too_many_arguments)] // blob 行の identity + メタ一式 + 本体の出どころ。
    pub(crate) async fn bump_blob(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        org: &str,
        sha256: &str,
        size: i64,
        content_type: &str,
        object_key: &str,
        body: BlobBody<'_>,
    ) -> Result<BlobBump, StorageError> {
        lock_blob(&mut *conn, tenant_id, org, sha256).await?;
        // xmax = 0 は、この文が行を挿入した（ON CONFLICT の更新ではない）ことを表す。
        let created: bool = sqlx::query_scalar(
            "INSERT INTO blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount) \
             VALUES ($1, $2, $3, $4, $5, $6, 1) \
             ON CONFLICT (tenant_id, org, sha256) DO UPDATE SET refcount = blob.refcount + 1 \
             RETURNING (xmax = 0)",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(sha256)
        .bind(size)
        .bind(content_type)
        .bind(object_key)
        .fetch_one(&mut *conn)
        .await?;
        if created {
            self.ensure_blob_body(object_key, content_type, body)
                .await?;
        }
        Ok(BlobBump { created })
    }

    /// blob の行を新しく作ったとき、本体があるかを確かめ、無ければ置く（[`Self::bump_blob`] のロックの中で呼ぶ）。
    async fn ensure_blob_body(
        &self,
        object_key: &str,
        content_type: &str,
        body: BlobBody<'_>,
    ) -> Result<(), StorageError> {
        if let BlobBody::Existing = body {
            // 既存の版の blob に参照を足すだけの経路（restore・adopt）。版の行があるのに blob の行が無いのは、
            // 参照数の決まり（refcount = node_version の行数）が崩れている。置き直す元も無いので止める。
            return Err(StorageError::Db(sqlx::Error::Protocol(format!(
                "既存の版の blob の行がありません（{object_key}）"
            ))));
        }
        if self.store.exists(object_key).await? {
            return Ok(());
        }
        match body {
            BlobBody::Bytes(bytes) => {
                self.store
                    .put_object(object_key, bytes.to_vec(), content_type)
                    .await?;
            }
            BlobBody::CopyFrom(src) => self.store.copy(src, object_key).await?,
            BlobBody::Existing => {}
        }
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
        // system は**親から継承**する（#392）。システム領域（エージェントのワークスペース等）に
        // 置かれたファイルは、ドライブ一覧・名前検索・RAG 索引から一律で外れる。
        let sql = format!(
            "INSERT INTO node (org, tenant_id, kind, name, parent_id, blob_sha256, size_bytes, content_type, created_by, updated_by, system) \
             VALUES ($1, $2, 'file', $3, $4, $5, $6, $7, $8, $8, \
                     coalesce((SELECT n.system FROM node n WHERE n.id = $4), false)) \
             RETURNING {NODE_COLS}"
        );
        let row: NodeRow = sqlx::query_as(&sql)
            .bind(ctx.org())
            .bind(ctx.tenant_id())
            .bind(name)
            .bind(parent_id)
            .bind(sha256)
            .bind(size)
            .bind(content_type)
            .bind(&ctx.principal().id)
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
            .bind(ctx.tenant_id())
            .execute(&mut *conn)
            .await?;
        }
        // 自分自身（depth 0）。
        sqlx::query(
            "INSERT INTO node_closure (tenant_id, org, ancestor, descendant, depth) VALUES ($1, $2, $3, $3, 0)",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
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
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .bind(sha256)
        .bind(size)
        .bind(content_type)
        .bind(&ctx.principal().id)
        .execute(conn)
        .await?;
        Ok(())
    }
}

/// [`StorageService::bump_blob`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlobBump {
    /// blob の行をこの呼び出しで新しく作った（それまで行が無かった）。
    pub(crate) created: bool,
}

/// blob の行を新しく作ったときに本体を置き直す元（[`StorageService::bump_blob`]）。
#[derive(Clone, Copy)]
pub(crate) enum BlobBody<'a> {
    /// 所持しているバイト（内部書込）。
    Bytes(&'a [u8]),
    /// 不変な写し（finalize の incoming）。サーバ側で写す。
    CopyFrom(&'a str),
    /// 既存の版の blob に参照を足すだけ（行は必ずある）。行を新しく作ったら内部の誤り。
    Existing,
}

/// blob `(tenant_id, org, sha256)` を Tx の終わりまで押さえる（advisory xact lock）。
///
/// キーは `hashtextextended('blob:' || '<tenant_id>|<org>|<sha256>', 0)`（64bit・`blob:` 接頭辞で
/// `audit_chain:`・`share_link:` のキーと分ける）。[`StorageService::bump_blob`] と完全削除の片付けが同じキーで
/// 直列化する。完全削除の本体（多くの blob を 1 つの Tx で押さえる）は、共有ロック表を使い切らないよう、これでは
/// なく blob の行ロックを使う（`purge::release_blobs`）。1 つの Tx で多くの本体にこれを取らない。
pub(crate) async fn lock_blob(
    conn: &mut PgConnection,
    tenant_id: &str,
    org: &str,
    sha256: &str,
) -> Result<(), StorageError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('blob:' || $1, 0))")
        .bind(format!("{tenant_id}|{org}|{sha256}"))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// 新規作成の親を Tx の中で確かめ直し、親とその祖先の行を id の昇順に押さえる（`bump_blob` より前に呼ぶ）。
///
/// 親の確かめは Tx の外の `ensure_folder` だけだと、その後に親が論理削除されると、削除したフォルダの下に生きた
/// ノードが残る。ここで `FOR SHARE` を取ると、論理削除（配下を `FOR NO KEY UPDATE` で押さえる）とは互いに待ち、
/// 削除が先なら確かめ直しで [`StorageError::NotFound`]、作成が先なら削除が作成の commit の後に新しいノードにも
/// 印を付ける。祖先まで id の昇順で押さえるのは、移動（PIT-16・移動先の祖先を id の昇順で `FOR UPDATE`）と
/// 順をそろえるため。親だけを押さえると、後の closure の挿入が外部キーで祖先に `FOR KEY SHARE` を取りに行き、
/// 祖先を押さえて親を待つ移動とデッドロックになる。作成どうしは `FOR SHARE` なので互いに待たない。
pub(crate) async fn lock_parent_for_create(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    parent_id: Uuid,
) -> Result<(), StorageError> {
    let mut ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT ancestor FROM node_closure WHERE tenant_id = $1 AND org = $2 AND descendant = $3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(parent_id)
    .fetch_all(&mut *conn)
    .await?;
    if !ids.contains(&parent_id) {
        ids.push(parent_id);
    }
    let live: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, kind FROM node \
         WHERE id = ANY($1) AND org = $2 AND tenant_id = $3 AND deleted_at IS NULL \
         ORDER BY id FOR SHARE",
    )
    .bind(&ids)
    .bind(ctx.org())
    .bind(ctx.tenant_id())
    .fetch_all(&mut *conn)
    .await?;
    match live.iter().find(|(id, _)| *id == parent_id) {
        Some((_, kind)) if kind == "folder" => Ok(()),
        Some(_) => Err(StorageError::Invalid("親がフォルダではありません".into())),
        None => Err(StorageError::NotFound),
    }
}

/// 内容を差し替える対象のファイルの行を、`bump_blob` より前に `FOR UPDATE` で押さえる（ロックの順 node → blob）。
///
/// `base_version` があれば押さえた後の版と比べ、違えば [`StorageError::VersionConflict`]（今の版つき）。
/// ファイルが無い・消えていれば [`StorageError::NotFound`]。
pub(crate) async fn lock_file_for_content(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    file_id: Uuid,
    base_version: Option<i64>,
) -> Result<(), StorageError> {
    let version: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM node \
         WHERE id = $1 AND org = $2 AND tenant_id = $3 AND kind = 'file' AND deleted_at IS NULL \
         FOR UPDATE",
    )
    .bind(file_id)
    .bind(ctx.org())
    .bind(ctx.tenant_id())
    .fetch_optional(&mut *conn)
    .await?;
    let current = version.ok_or(StorageError::NotFound)?;
    match base_version {
        Some(base) if base != current => Err(StorageError::VersionConflict { current }),
        _ => Ok(()),
    }
}
