//! StorageService: 資料のチャンネル共有（`folder` / `file` の viewer に `channel#member` を付与・design §4.14）。
//!
//! チャンネルに共有した資料は、その時点の参加者全員が読める。メッセージのエージェントの公開返信は、
//! チャンネル内の発言とこの共有資料だけを根拠にできる。
//!
//! - **閲覧だけ**を付与する（editor は付けない・モデルも viewer にしか `channel#member` を受理しない）。
//! - 共有の管理は既存の共有と同じく **owner 権限**。加えて、共有する人が**そのチャンネルの参加者**で
//!   あることを要求する（参加していないチャンネルへ資料を流し込めない）。解除は owner なら参加不要。
//! - ユーザー・ロールへの共有（[`ShareTarget`]）とは別の API にする。`ShareTarget` は表・アーティファクト
//!   の共有でも使う語彙で、それらの型は `channel#member` を受理しないため。

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::AuditAction;
use crate::session_lock::SessionLock;

impl StorageService {
    /// ノードをチャンネルの参加者へ閲覧で共有する（owner かつチャンネルの参加者・冪等）。
    ///
    /// # Errors
    ///
    /// ノードが無い（`NotFound`）・owner でない／チャンネルの参加者でない（`Forbidden`）・認可基盤や DB の失敗。
    pub async fn share_node_with_channel(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        channel_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, AuditAction::NodeShare, trace_id)
            .await?;
        let channel = ctx.ns().channel(&channel_id.to_string());
        let member = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Member,
                &channel,
                Consistency::HigherConsistency,
            )
            .await?;
        if !member {
            return Err(StorageError::Forbidden);
        }
        self.change_channel_share(ctx, node_id, &obj, channel_id, true, trace_id)
            .await
    }

    /// チャンネルへの共有を解除する（owner 権限・冪等・即時反映）。
    ///
    /// # Errors
    ///
    /// ノードが無い（`NotFound`）・owner でない（`Forbidden`）・認可基盤や DB の失敗。
    pub async fn unshare_node_from_channel(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        channel_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, AuditAction::NodeUnshare, trace_id)
            .await?;
        self.change_channel_share(ctx, node_id, &obj, channel_id, false, trace_id)
            .await
    }

    /// このノードを共有しているチャンネルの id（owner 権限・このノードに直接書いた共有だけ）。
    ///
    /// # Errors
    ///
    /// ノードが無い（`NotFound`）・owner でない（`Forbidden`）・認可基盤の失敗。
    pub async fn list_channel_shares(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<Uuid>, StorageError> {
        let obj = self
            .authorize_share_admin(ctx, node_id, AuditAction::NodeSharesList, trace_id)
            .await?;
        let tuples = self.authz.read_tuples(&obj, Some(Relation::Viewer)).await?;
        let ns = ctx.ns();
        let mut ids: Vec<Uuid> = tuples
            .iter()
            .filter_map(|t| ns.parse_channel_member_subject(&t.user))
            .filter_map(|id| Uuid::parse_str(id).ok())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// チャンネル共有の付与・解除と監査を、同じノード・チャンネルの操作と直列化して行う。
    ///
    /// セッションの advisory lock（[`SessionLock`]）を取り、同じ接続で「監査の確定 → 権限の変更」を行う。
    /// 巻き戻しを持たないので、セッションが切れてロックを失っても、古い判断で権限を戻すことはない。
    async fn change_channel_share(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        obj: &FgaObject,
        channel_id: Uuid,
        grant: bool,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let key = format!(
            "node_channel_share:{}:{node_id}:{channel_id}",
            ctx.tenant_id()
        );
        let mut lock = SessionLock::acquire(&self.db, key).await?;
        let result = match lock.conn() {
            Some(conn) => {
                self.change_channel_share_locked(
                    conn, ctx, node_id, obj, channel_id, grant, trace_id,
                )
                .await
            }
            None => Err(StorageError::Integrity("ロックの接続がありません".into())),
        };
        lock.release().await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn change_channel_share_locked(
        &self,
        conn: &mut sqlx::PgConnection,
        ctx: &AuthContext,
        node_id: Uuid,
        obj: &FgaObject,
        channel_id: Uuid,
        grant: bool,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        // 監査を先に確定させてから権限を変える（巻き戻しを持たない）。権限の変更に失敗したら要求は失敗し、
        // 失敗を監査に追記する。巻き戻しが無いので、並行する操作の後に古い判断で権限を戻すことがない。
        // 監査の確定後・権限の変更前にプロセスが止まると、監査は変更しなかった操作を記録する（多めに残る側）。
        self.record_channel_share(
            &mut *conn, ctx, node_id, obj, channel_id, grant, None, trace_id,
        )
        .await?;
        let subject = ctx.ns().channel_member(&channel_id.to_string());
        let applied = if grant {
            self.authz
                .write_tuple(&subject, Relation::Viewer, obj)
                .await
        } else {
            self.authz
                .delete_tuple(&subject, Relation::Viewer, obj)
                .await
        };
        if let Err(e) = applied {
            let failure = e.to_string();
            if let Err(err) = self
                .record_channel_share(
                    &mut *conn,
                    ctx,
                    node_id,
                    obj,
                    channel_id,
                    grant,
                    Some(&failure),
                    trace_id,
                )
                .await
            {
                tracing::error!(
                    error = %err,
                    %node_id,
                    %channel_id,
                    granted = grant,
                    "sharing: チャンネル共有の変更の失敗を監査に追記できない"
                );
            }
            return Err(e.into());
        }
        Ok(())
    }

    /// チャンネル共有の付与・解除を監査に残す（権限変更なのでハッシュチェーンに連結する）。
    /// `failure` があれば、先に記録した操作が適用できなかったことを `deny` で追記する。
    #[allow(clippy::too_many_arguments)]
    async fn record_channel_share(
        &self,
        conn: &mut sqlx::PgConnection,
        ctx: &AuthContext,
        node_id: Uuid,
        obj: &FgaObject,
        channel_id: Uuid,
        grant: bool,
        failure: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let action = if grant {
            AuditAction::NodeShare
        } else {
            AuditAction::NodeUnshare
        };
        let object_type = if obj.as_str().starts_with("folder:") {
            "folder"
        } else {
            "file"
        };
        let mut tx = sqlx::Connection::begin(conn).await?;
        audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action,
                object_type,
                object_id: &node_id.to_string(),
                decision: if failure.is_some() {
                    Decision::Deny
                } else {
                    Decision::Allow
                },
                trace_id,
                metadata: json!({
                    "target": { "type": "channel", "id": channel_id },
                    "role": ShareRole::Viewer,
                    "failed": failure,
                }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
