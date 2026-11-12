//! StorageService: ゴミ箱の完全削除（GAP-108b・D-201）。
//!
//! 入口はここだけ（単一チョークポイント）。「1 件ずつ消す」（[`StorageService::purge_node`]）も「ゴミ箱を空にする」
//! （[`StorageService::empty_trash`]・`purge_empty.rs`）も、同じ [`StorageService::purge_locked`] で消す。
//!
//! 消せる人（D-201）: 削除した根の持ち主（`owner`・folder / file の owner は直接のタプルだけ）か、組織の管理者
//! （`organization#admin`）。editor は求めない（管理者は editor に入らない・PIT-65）。workflow が owner のファイルは
//! 利用者の owner の check を通らないので、管理者だけが消せる。フォルダの持ち主は、その下のほかの人のファイルも
//! まとめて消せる（範囲は論理削除と同じ）。
//!
//! 1 つの Tx の中で、ノード・closure・版・アップロードの宣言・共有リンク・Office のロックの行を消し、blob の参照数を
//! 消した版の数だけ減らす。DB の外にあるもの（OpenFGA のタプルと本体）は同じ Tx で片付け待ち
//! （`storage_purge_pending`・migration 0154）に積み、commit の後にその場で片付ける（タプル → 本体の順・PIT-38 の
//! 「データを先に閉じる」）。その場で片付けきれなかった分と失敗した分は、掃除（`purge_pending`）がやり直す。
//! 検索の索引（`rag_chunk`・`doc_block`・ベクタ・全文）は、ファイルごとの delete のイベントで索引の側が消す。
//!
//! ロックの順は **node（id の昇順・FOR UPDATE）→ blob の行（sha256 の昇順・FOR UPDATE）→ 監査のチェーン**
//! （書き込みと同じ順・`upload::bump_blob`）。blob は advisory lock ではなく行ロックで押さえる。行ロックは行に
//! 載り、共有ロック表を使わないので、異なる本体が何万ある根でも 1 つの Tx で消せる（advisory lock を本体ごとに
//! 取ると、表の大きさ `max_locks_per_transaction × 接続の数` を超えた根は `out of shared memory` で二度と消せない）。
//! 書き込みの `bump_blob` は advisory lock の後に行を upsert するので、その upsert がこの行ロックを待つ。片付け
//! （`cleanup_one`・`purge_pending`）は 1 行ずつ advisory lock（[`lock_blob`]）を取り、`bump_blob` と直列になる。
//!
//! 保持期間を過ぎたものの自動の完全削除（GAP-108c・D-202・`crate::trash_retention`）も、同じ本体
//! （[`StorageService::purge_for_retention`]）で消す。主体はシステム（`system:trash-retention`）で、権限の check は
//! 通さない。根を押さえた後に、まだ期限を過ぎているかを確かめ直す。

#[allow(clippy::wildcard_imports)]
use super::*;

use sqlx::{Postgres, Transaction};

use super::upload::lock_blob;
use crate::audit::AuditAction;
use crate::trash_retention::{due_on, RetentionPurge, SWEEP_ACTOR};

/// 完全削除の監査の書き方（利用者の操作か、保持期間の掃除か）。
struct PurgeAudit<'a> {
    action: AuditAction,
    /// metadata に足す項目（数の項目のほか）。
    extra: serde_json::Value,
    trace_id: Option<&'a str>,
}

/// commit の後に、1 回の呼び出しの中で片付ける片付け待ちの行の上限（残りは掃除が片付ける）。
///
/// 1 行は OpenFGA への 2 往復（Read と Write）か、オブジェクトストアへの 1 往復。根の行を先に片付けるので、
/// 根から継いでいた閲覧は上限に届いても先に閉じる。
pub(crate) const INLINE_CLEANUP_MAX: usize = 300;

/// 完全削除の結果（[`StorageService::purge_node`]）。消さなかった理由は、何も消していないことを表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeOutcome {
    /// 完全に削除した。
    Purged(PurgeReport),
    /// 論理削除されていない（ゴミ箱に無い）。
    NotInTrash,
    /// 削除したフォルダの中にある（ゴミ箱の一覧に出ているのは、そのフォルダ）。
    InsideDeletedFolder,
    /// 中に、削除されていない（生きている）ファイルやフォルダがある（論理削除の競合の直し・7p-4 より前に崩れた行）。
    HasLiveDescendants,
}

/// 完全に削除したものの数（監査の metadata と同じ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeReport {
    /// 根の種類。
    pub kind: NodeKind,
    /// 消したノードの数（根を含む）。
    pub nodes: usize,
    /// そのうちのファイルの数。
    pub files: usize,
    /// 消した版の数。
    pub versions: usize,
    /// 参照が 0 になって片付け待ちに入れた本体の大きさの合計（ほかのファイルと共有している本体は数えない）。
    pub freed_bytes: i64,
}

/// 押さえた配下の 1 行。
#[derive(sqlx::FromRow)]
struct LockedNode {
    id: Uuid,
    kind: String,
    parent_id: Option<Uuid>,
    version: i64,
    deleted: bool,
}

/// 片付け待ちの 1 行（commit の後にその場で片付ける）。
struct PendingCleanup {
    id: Uuid,
    work: CleanupWork,
}

enum CleanupWork {
    Tuples { node_id: Uuid, kind: NodeKind },
    Object { sha256: String, object_key: String },
}

impl StorageService {
    /// ゴミ箱の根を、配下ごと完全に削除する（元に戻せない・D-201）。
    ///
    /// - 無い（ほかのテナント・組織を含む）・システム領域（#392・ドライブとゴミ箱で隠したもの）は [`StorageError::NotFound`]。
    /// - 根の owner でも組織の管理者でもなければ、deny を監査に残して [`StorageError::Forbidden`]。
    /// - ゴミ箱に無い・削除したフォルダの中にある・中に生きているノードがあるときは、何も消さずに理由を返す。
    pub async fn purge_node(
        &self,
        ctx: &AuthContext,
        root_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<PurgeOutcome, StorageError> {
        // 論理削除と同じく、削除済みも含めて引いてから確かめる（system はドライブにもゴミ箱にも出さないので、
        // ここでも在ることを明かさない）。
        let head: Option<(String, bool)> = sqlx::query_as(
            "SELECT kind, system FROM node WHERE id = $1 AND org = $2 AND tenant_id = $3",
        )
        .bind(root_id)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .fetch_optional(&self.db)
        .await?;
        let Some((kind, system)) = head else {
            return Err(StorageError::NotFound);
        };
        if system {
            return Err(StorageError::NotFound);
        }
        let kind = NodeKind::parse(&kind)
            .ok_or_else(|| StorageError::Integrity(format!("未知のノード種別: {kind}")))?;
        // 元に戻せない操作なので、剥奪をすぐに効かせる（PIT-11）。
        if !self
            .can_purge(ctx, kind, root_id, Consistency::HigherConsistency)
            .await?
        {
            self.audit
                .record(
                    ctx,
                    AuditEntry {
                        action: purge_action(kind),
                        object_type: kind.as_str(),
                        object_id: &root_id.to_string(),
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": "owner|admin" }),
                    },
                )
                .await?;
            return Err(StorageError::Forbidden);
        }
        let mut budget = INLINE_CLEANUP_MAX;
        self.purge_locked(ctx, root_id, kind, trace_id, &mut budget)
            .await
    }

    /// 呼んだ人が `kind`/`id` を完全に削除できるか（根の owner か組織の管理者・D-201）。
    pub(crate) async fn can_purge(
        &self,
        ctx: &AuthContext,
        kind: NodeKind,
        id: Uuid,
        consistency: Consistency,
    ) -> Result<bool, StorageError> {
        let subject = ctx.subject();
        if self
            .authz
            .check(
                &subject,
                Relation::Owner,
                &node_fga_object(&ctx.ns(), kind, id),
                consistency,
            )
            .await?
        {
            return Ok(true);
        }
        self.is_org_admin(ctx, consistency).await
    }

    /// 呼んだ人が組織の管理者（`organization#admin`）か。
    pub(crate) async fn is_org_admin(
        &self,
        ctx: &AuthContext,
        consistency: Consistency,
    ) -> Result<bool, StorageError> {
        Ok(self
            .authz
            .check(
                &ctx.subject(),
                Relation::Admin,
                &ctx.ns().organization(ctx.org()),
                consistency,
            )
            .await?)
    }

    /// 認可を済ませた根を、配下ごと完全に削除する（`purge_node` と「空にする」の共通の本体）。
    ///
    /// `cleanup_budget` は commit の後にその場で片付ける片付け待ちの行の残りの数で、使った分を減らす。
    pub(crate) async fn purge_locked(
        &self,
        ctx: &AuthContext,
        root_id: Uuid,
        kind: NodeKind,
        trace_id: Option<&str>,
        cleanup_budget: &mut usize,
    ) -> Result<PurgeOutcome, StorageError> {
        let mut tx = self.db.begin().await?;
        // 1・2. 根と配下を押さえ、押さえた後の値で消してよいかを確かめ直す。
        let locked = match lock_subtree(&mut tx, ctx, root_id, kind).await? {
            Ok(locked) => locked,
            Err(refused) => return Ok(refused),
        };
        let audit = PurgeAudit {
            action: purge_action(kind),
            extra: json!({}),
            trace_id,
        };
        let report = self
            .purge_rows(tx, ctx, (root_id, kind), &locked, audit, cleanup_budget)
            .await?;
        Ok(PurgeOutcome::Purged(report))
    }

    /// 保持期間を過ぎた削除の根を、根の組織のシステムの主体（[`SWEEP_ACTOR`]）で配下ごと完全に削除する
    /// （GAP-108c・D-202・`crate::trash_retention` の掃除だけが呼ぶ）。
    ///
    /// 権限の check は通さない（人の操作ではない）。利用者の完全削除と同じく根と配下を押さえ、押さえた後の値で、
    /// 消してよいか（ゴミ箱の根で、中に生きているノードが無い）と、`now` でまだ期限を過ぎているか
    /// （[`due_on`]）を確かめ直す。監査は `storage.trash_retention.purge` をチェーンで残す。
    pub(crate) async fn purge_for_retention(
        &self,
        tenant_id: &str,
        org: &str,
        (root_id, kind): (Uuid, NodeKind),
        now: DateTime<Utc>,
        cleanup_budget: &mut usize,
    ) -> Result<RetentionPurge, StorageError> {
        let ctx = system_ctx(tenant_id, org, SWEEP_ACTOR)?;
        let mut tx = self.db.begin().await?;
        let locked = match lock_subtree(&mut tx, &ctx, root_id, kind).await? {
            Ok(locked) => locked,
            Err(refused) => return Ok(RetentionPurge::Refused(refused)),
        };
        // 候補を選んでから押さえるまでの間に、復元して削除し直した・保持期間を変えた、を拾う。
        let Some(due) = due_on(&mut tx, &ctx, root_id, now).await? else {
            return Ok(RetentionPurge::NotDue);
        };
        let audit = PurgeAudit {
            action: AuditAction::StorageTrashRetentionPurge,
            extra: json!({
                "retention_days": due.days,
                "deleted_at": due.deleted_at,
            }),
            trace_id: None,
        };
        let report = self
            .purge_rows(tx, &ctx, (root_id, kind), &locked, audit, cleanup_budget)
            .await?;
        Ok(RetentionPurge::Purged(report))
    }

    /// 接続プール（テナントを横断する保守の掃除 `crate::trash_retention` が、候補の選び出しとリーダーのロックに使う）。
    pub(crate) fn pool(&self) -> &PgPool {
        &self.db
    }

    /// 押さえた根と配下（`locked`）の行を消し、監査・イベント・片付け待ちを残して commit し、その場で片付ける。
    async fn purge_rows(
        &self,
        mut tx: Transaction<'static, Postgres>,
        ctx: &AuthContext,
        (root_id, kind): (Uuid, NodeKind),
        locked: &[LockedNode],
        audit: PurgeAudit<'_>,
        cleanup_budget: &mut usize,
    ) -> Result<PurgeReport, StorageError> {
        let trace_id = audit.trace_id;
        let ids: Vec<Uuid> = locked.iter().map(|n| n.id).collect();

        // 3. 消す版を blob ごとに数える（refcount = node_version の行数）。行を消す前に数える。
        let versions: Vec<(String, i64)> = sqlx::query_as(
            "SELECT blob_sha256, count(*) FROM node_version \
             WHERE tenant_id = $1 AND org = $2 AND node_id = ANY($3) \
             GROUP BY blob_sha256 ORDER BY blob_sha256",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;

        // 4. 行を消す（外部キーのある子の表から。collab_doc・collab_update は node から連鎖して消える）。
        self.delete_node_rows(&mut tx, ctx, &ids).await?;

        // 5. blob の参照数を減らす。blob の行は sha256 の昇順に押さえる（node の後・監査のチェーンの前）。
        let (mut objects, freed_bytes) = release_blobs(&mut tx, ctx, &versions).await?;

        // 6. ファイルごとに delete のイベントを出す。version は消した行の version + 1（論理削除のときの delete と、
        //    取り込みの重複除け (tenant, node_id, version, op) が重ならない・rag の job_state）。node が無いので、
        //    索引の側はフォルダとして展開せずに remove_node へ進む。
        let mut files = 0usize;
        for n in locked.iter().filter(|n| n.kind == "file") {
            files += 1;
            event::emit_on(
                &mut tx,
                ctx,
                WriteEvent {
                    node_id: n.id,
                    version: n.version + 1,
                    op: WriteOp::Delete,
                    payload: json!({ "kind": "file", "purge": true }),
                },
                trace_id,
            )
            .await?;
        }

        // 7. 元に戻せない操作として監査のチェーンに残す（名前は入れない）。
        let report = PurgeReport {
            kind,
            nodes: ids.len(),
            files,
            versions: versions
                .iter()
                .map(|(_, c)| usize::try_from(*c).unwrap_or(0))
                .sum(),
            freed_bytes,
        };
        let mut metadata = json!({
            "subtree_count": report.nodes,
            "file_count": report.files,
            "version_count": report.versions,
            "freed_bytes": report.freed_bytes,
        });
        if let (Some(m), serde_json::Value::Object(extra)) = (metadata.as_object_mut(), audit.extra)
        {
            m.extend(extra);
        }
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: audit.action,
                object_type: kind.as_str(),
                object_id: &root_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata,
            },
            Chain::Yes,
        )
        .await?;

        // 8. 消したノードのタプルを片付け待ちに入れる（根を先に片付ける）。
        let mut cleanup = enqueue_tuples(&mut tx, ctx, locked, root_id).await?;
        tx.commit().await?;

        // commit の後にその場で片付ける（タプル → 本体）。
        cleanup.append(&mut objects);
        self.drain_cleanup(ctx, &cleanup, cleanup_budget).await;
        Ok(report)
    }

    /// 消すノードの行と、それを指す行（外部キーのある子・外部キーの無い台帳）を消す（完全削除の Tx の中で呼ぶ）。
    async fn delete_node_rows(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        ids: &[Uuid],
    ) -> Result<(), StorageError> {
        // 共有リンクの台帳（外部キーは無い・0055）。redeem で書いた per-user のタプルは、ノードのタプルの片付けで外れる。
        // redeem はノードの行を FOR KEY SHARE で押さえるので（`share_link_redeem`）、押さえた後にここで消す間に新しい
        // 台帳の行は入らない。
        sqlx::query("DELETE FROM node_share_link_grant WHERE tenant_id = $1 AND node_id = ANY($2)")
            .bind(ctx.tenant_id())
            .bind(ids)
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "DELETE FROM node_share_link WHERE tenant_id = $1 AND org = $2 AND node_id = ANY($3)",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(ids)
        .execute(&mut *conn)
        .await?;
        // Office の編集のロック（外部キーは無い・0048・期限で消える助言のロック）。
        sqlx::query("DELETE FROM office_lock WHERE tenant_id = $1 AND file_id = ANY($2)")
            .bind(ctx.tenant_id())
            .bind(ids)
            .execute(&mut *conn)
            .await?;
        // 消すフォルダへのアップロードの宣言・消すファイルへの新版の宣言（staging の本体は孤児の掃除が回収する）。
        sqlx::query(
            "DELETE FROM pending_upload \
             WHERE tenant_id = $1 AND org = $2 AND (parent_id = ANY($3) OR target_node_id = ANY($3))",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(ids)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "DELETE FROM node_version WHERE tenant_id = $1 AND org = $2 AND node_id = ANY($3)",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(ids)
        .execute(&mut *conn)
        .await?;
        // 祖先（ゴミ箱の外の生きたフォルダ）から根へのリンクも消える（descendant が消すノード）。
        sqlx::query(
            "DELETE FROM node_closure \
             WHERE tenant_id = $1 AND (descendant = ANY($2) OR ancestor = ANY($2))",
        )
        .bind(ctx.tenant_id())
        .bind(ids)
        .execute(&mut *conn)
        .await?;
        let deleted =
            sqlx::query("DELETE FROM node WHERE tenant_id = $1 AND org = $2 AND id = ANY($3)")
                .bind(ctx.tenant_id())
                .bind(ctx.org())
                .bind(ids)
                .execute(&mut *conn)
                .await?;
        if usize::try_from(deleted.rows_affected()).ok() != Some(ids.len()) {
            // 押さえた行が消せないのは、ロックの前提が崩れている。何も消さずに止める（Tx は落とす）。
            return Err(StorageError::Db(sqlx::Error::Protocol(format!(
                "押さえたノード {} 件のうち {} 件しか消せませんでした",
                ids.len(),
                deleted.rows_affected()
            ))));
        }
        Ok(())
    }

    /// commit の後に、片付け待ちの行をその場で片付ける（タプル → 本体の順・`budget` 件まで）。
    ///
    /// 失敗はログに残して行をそのままにする（掃除 `sweep_purge_pending` がやり直す）。掃除が同じ行を処理中なら
    /// （`SKIP LOCKED` で取れなければ）掃除に任せる。
    async fn drain_cleanup(&self, ctx: &AuthContext, rows: &[PendingCleanup], budget: &mut usize) {
        for row in rows {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            if let Err(e) = self.cleanup_one(ctx, row).await {
                tracing::warn!(
                    error = %e,
                    tenant_id = %ctx.tenant_id(),
                    id = %row.id,
                    "完全削除の片付けに失敗しました（掃除がやり直します）"
                );
            }
        }
    }

    /// 片付け待ちの 1 行を片付けて消す（掃除 `purge_pending::purge_one` と同じ決まり）。
    async fn cleanup_one(
        &self,
        ctx: &AuthContext,
        row: &PendingCleanup,
    ) -> Result<(), StorageError> {
        let mut tx = self.db.begin().await?;
        let claimed: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM storage_purge_pending WHERE tenant_id = $1 AND id = $2 \
             FOR UPDATE SKIP LOCKED",
        )
        .bind(ctx.tenant_id())
        .bind(row.id)
        .fetch_optional(&mut *tx)
        .await?;
        if claimed.is_none() {
            // 掃除が処理中か、もう片付いた。
            return Ok(());
        }
        match &row.work {
            CleanupWork::Tuples { node_id, kind } => {
                self.authz
                    .delete_object_tuples(&node_fga_object(&ctx.ns(), *kind, *node_id))
                    .await?;
            }
            CleanupWork::Object { sha256, object_key } => {
                // 同じ内容が保存し直されて行が作り直されていれば、本体はその参照のもの。消さない（PIT-14）。
                lock_blob(&mut tx, ctx.tenant_id(), ctx.org(), sha256).await?;
                let live: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3)",
                )
                .bind(ctx.tenant_id())
                .bind(ctx.org())
                .bind(sha256)
                .fetch_one(&mut *tx)
                .await?;
                if !live {
                    self.store.delete(object_key).await?;
                }
            }
        }
        sqlx::query("DELETE FROM storage_purge_pending WHERE tenant_id = $1 AND id = $2")
            .bind(ctx.tenant_id())
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// 根と配下（closure の descendant）を id の昇順に `FOR UPDATE` で押さえ、押さえた後の値で消してよいかを確かめ直す
/// （論理削除・移動・作成と同じ順）。消さないときは理由（`Err` の側の [`PurgeOutcome`]）を返す。
///
/// 復元（deleted_at を外す UPDATE）と重なっても、押さえた後の値で確かめ直すので、勝つのはどちらか一方だけ。並行の
/// 完全削除が先に消していれば根が無い（[`StorageError::NotFound`]）。
async fn lock_subtree(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    root_id: Uuid,
    kind: NodeKind,
) -> Result<Result<Vec<LockedNode>, PurgeOutcome>, StorageError> {
    let locked: Vec<LockedNode> = sqlx::query_as(
        "SELECT n.id, n.kind, n.parent_id, n.version, (n.deleted_at IS NOT NULL) AS deleted \
         FROM node n \
         WHERE n.org = $1 AND n.tenant_id = $2 \
           AND n.id IN (SELECT descendant FROM node_closure WHERE tenant_id = $2 AND ancestor = $3) \
         ORDER BY n.id FOR UPDATE OF n",
    )
    .bind(ctx.org())
    .bind(ctx.tenant_id())
    .bind(root_id)
    .fetch_all(&mut *conn)
    .await?;
    let Some(root) = locked.iter().find(|n| n.id == root_id) else {
        return Err(StorageError::NotFound);
    };
    if NodeKind::parse(&root.kind) != Some(kind) {
        return Err(StorageError::NotFound);
    }
    if !root.deleted {
        return Ok(Err(PurgeOutcome::NotInTrash));
    }
    if let Some(parent) = root.parent_id {
        let parent_deleted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM node \
             WHERE id = $1 AND org = $2 AND tenant_id = $3 AND deleted_at IS NOT NULL)",
        )
        .bind(parent)
        .bind(ctx.org())
        .bind(ctx.tenant_id())
        .fetch_one(&mut *conn)
        .await?;
        if parent_deleted {
            return Ok(Err(PurgeOutcome::InsideDeletedFolder));
        }
    }
    // 削除したフォルダの下に生きているノードがあれば、元に戻せない形で消さない（crit の P1）。
    if locked.iter().any(|n| !n.deleted) {
        return Ok(Err(PurgeOutcome::HasLiveDescendants));
    }
    Ok(Ok(locked))
}

/// 消した版の数だけ blob の参照数を減らし、参照が 0 になった行を消して本体を片付け待ちに入れる。
/// 片付け待ちの行と、空いたバイト数の合計を返す。
///
/// 先に blob の行をすべて 1 文で、sha256 の昇順に `FOR UPDATE` で押さえる（node の後・監査のチェーンの前）。
/// advisory lock（[`lock_blob`]）は使わない（本体の数だけ共有ロック表を使い、大きな根で表を使い切る）。
/// 書き込みの `bump_blob` の upsert はこの行ロックを待ち、ここが行を消して commit すれば行を作り直して
/// （`created`）本体を確かめ直す。押さえた後の減算・削除・片付け待ちへの追加も、それぞれ 1 文でまとめて行う
/// （大きなフォルダで往復を増やさない）。
/// 参照数が消す版の数より少ないのは、決まり（refcount = node_version の行数）が崩れている。ほかの参照の本体を
/// 消しうるので、何も消さずに止める（Tx は落とす）。
async fn release_blobs(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    versions: &[(String, i64)],
) -> Result<(Vec<PendingCleanup>, i64), StorageError> {
    if versions.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let shas: Vec<&str> = versions.iter().map(|(s, _)| s.as_str()).collect();
    // ORDER BY の後で行を押さえる（LockRows は Sort の上）ので、押さえる順は sha256 の昇順にそろう。
    sqlx::query(
        "SELECT sha256 FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = ANY($3) \
         ORDER BY sha256 FOR UPDATE",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(&shas)
    .execute(&mut *conn)
    .await?;
    let counts: Vec<i64> = versions.iter().map(|(_, c)| *c).collect();
    let left: Vec<(String, i64, i64)> = sqlx::query_as(
        "UPDATE blob b SET refcount = b.refcount - v.n \
         FROM unnest($3::text[], $4::bigint[]) AS v(sha, n) \
         WHERE b.tenant_id = $1 AND b.org = $2 AND b.sha256 = v.sha AND b.refcount >= v.n \
         RETURNING b.sha256, b.refcount, b.size_bytes",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(&shas)
    .bind(&counts)
    .fetch_all(&mut *conn)
    .await?;
    if left.len() != versions.len() {
        return Err(StorageError::Db(sqlx::Error::Protocol(format!(
            "blob の参照数が消す版の数より少ないか、行がありません（{} 件のうち {} 件だけ減らせました）",
            versions.len(),
            left.len()
        ))));
    }
    let gone: Vec<(String, i64)> = left
        .into_iter()
        .filter(|(_, refcount, _)| *refcount == 0)
        .map(|(sha, _, size)| (sha, size))
        .collect();
    if gone.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let gone_shas: Vec<&str> = gone.iter().map(|(s, _)| s.as_str()).collect();
    sqlx::query(
        "DELETE FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = ANY($3) AND refcount = 0",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(&gone_shas)
    .execute(&mut *conn)
    .await?;
    let keys: Vec<String> = gone_shas
        .iter()
        .map(|sha| blob_object_key(ctx.tenant_id(), ctx.org(), sha))
        .collect();
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "INSERT INTO storage_purge_pending (tenant_id, org, kind, sha256, object_key) \
         SELECT $1, $2, 'object', t.sha, t.object_key \
         FROM unnest($3::text[], $4::text[]) AS t(sha, object_key) \
         RETURNING id, sha256, object_key",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(&gone_shas)
    .bind(&keys)
    .fetch_all(&mut *conn)
    .await?;
    let pending = rows
        .into_iter()
        .map(|(id, sha256, object_key)| PendingCleanup {
            id,
            work: CleanupWork::Object { sha256, object_key },
        })
        .collect();
    Ok((pending, gone.iter().map(|(_, size)| size).sum()))
}

/// 消したノードのタプルを外す仕事を片付け待ちに入れる（1 文）。根の行を先頭にして返す（根から継いでいた閲覧を
/// 先に閉じる）。
async fn enqueue_tuples(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    locked: &[LockedNode],
    root_id: Uuid,
) -> Result<Vec<PendingCleanup>, StorageError> {
    let ids: Vec<Uuid> = locked.iter().map(|n| n.id).collect();
    let kinds: Vec<&str> = locked.iter().map(|n| n.kind.as_str()).collect();
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "INSERT INTO storage_purge_pending (tenant_id, org, kind, node_id, node_kind) \
         SELECT $1, $2, 'node_tuples', t.node_id, t.node_kind \
         FROM unnest($3::uuid[], $4::text[]) AS t(node_id, node_kind) \
         RETURNING id, node_id, node_kind",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(&ids)
    .bind(&kinds)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: Vec<PendingCleanup> = rows
        .into_iter()
        .filter_map(|(id, node_id, node_kind)| {
            NodeKind::parse(&node_kind).map(|kind| PendingCleanup {
                id,
                work: CleanupWork::Tuples { node_id, kind },
            })
        })
        .collect();
    out.sort_by_key(
        |p| !matches!(p.work, CleanupWork::Tuples { node_id, .. } if node_id == root_id),
    );
    Ok(out)
}

/// 完全削除の監査の操作（種類ごと）。
fn purge_action(kind: NodeKind) -> AuditAction {
    match kind {
        NodeKind::File => AuditAction::FilePurge,
        NodeKind::Folder => AuditAction::FolderPurge,
    }
}
