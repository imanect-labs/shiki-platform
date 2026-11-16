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
    /// セッションの advisory lock（[`SessionLock`]）を取り、同じ接続のトランザクションで FGA の変更・監査・
    /// commit を行う。監査か commit に失敗したら、**ロックを持ったまま**今回変えた分だけを巻き戻す
    /// （並行する共有・解除が割り込んで、古い判断で権限を戻すことがない）。ロックは巻き戻しの後に外す。
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
        let subject = ctx.ns().channel_member(&channel_id.to_string());
        let mut tx = sqlx::Connection::begin(conn).await?;
        // 付与・剥奪は冪等。changed=true なら今回実際に変えた（巻き戻すのはその分だけ）。
        let changed = if grant {
            self.authz
                .write_tuple(&subject, Relation::Viewer, obj)
                .await?
        } else {
            self.authz
                .delete_tuple(&subject, Relation::Viewer, obj)
                .await?
        };
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
        let audited = audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action,
                object_type,
                object_id: &node_id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({
                    "target": { "type": "channel", "id": channel_id },
                    "role": ShareRole::Viewer,
                }),
            },
            Chain::Yes,
        )
        .await;
        let committed = match audited {
            Ok(()) => tx.commit().await.map_err(StorageError::from),
            Err(e) => Err(e),
        };
        if let Err(e) = committed {
            if changed {
                self.undo_channel_share(&subject, obj, grant, node_id, channel_id)
                    .await;
            }
            return Err(e);
        }
        Ok(())
    }

    /// 監査が残せなかった変更を戻す。戻せなければタプルの食い違いをログに残す（運用者が検知できるように）。
    async fn undo_channel_share(
        &self,
        subject: &Subject,
        obj: &FgaObject,
        granted: bool,
        node_id: Uuid,
        channel_id: Uuid,
    ) {
        let undo = if granted {
            self.authz
                .delete_tuple(subject, Relation::Viewer, obj)
                .await
        } else {
            self.authz.write_tuple(subject, Relation::Viewer, obj).await
        };
        if let Err(err) = undo {
            tracing::error!(
                error = %err,
                %node_id,
                %channel_id,
                granted,
                "sharing: 監査失敗後のチャンネル共有の巻き戻しに失敗（閲覧タプルが監査と食い違う）"
            );
        }
    }
}
