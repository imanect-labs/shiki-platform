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
        let subject = ctx.ns().channel_member(&channel_id.to_string());
        let granted = self
            .authz
            .write_tuple(&subject, Relation::Viewer, &obj)
            .await?;
        if let Err(e) = self
            .record_channel_share_audit(
                ctx,
                node_id,
                &obj,
                AuditAction::NodeShare,
                channel_id,
                trace_id,
            )
            .await
        {
            // 実際に付与した時だけ巻き戻す（既にあった共有を剥がさない）。
            if granted {
                let _ = self
                    .authz
                    .delete_tuple(&subject, Relation::Viewer, &obj)
                    .await;
            }
            return Err(e);
        }
        Ok(())
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
        let subject = ctx.ns().channel_member(&channel_id.to_string());
        let revoked = self
            .authz
            .delete_tuple(&subject, Relation::Viewer, &obj)
            .await?;
        if let Err(e) = self
            .record_channel_share_audit(
                ctx,
                node_id,
                &obj,
                AuditAction::NodeUnshare,
                channel_id,
                trace_id,
            )
            .await
        {
            // 実際に剥奪した時だけ巻き戻す（無かった共有を付けない）。
            if revoked {
                let _ = self
                    .authz
                    .write_tuple(&subject, Relation::Viewer, &obj)
                    .await;
            }
            return Err(e);
        }
        Ok(())
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

    /// チャンネル共有の付与・解除を監査に残す（権限変更なのでハッシュチェーンに連結する）。
    async fn record_channel_share_audit(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        obj: &FgaObject,
        action: AuditAction,
        channel_id: Uuid,
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
                    "target": { "type": "channel", "id": channel_id },
                    "role": ShareRole::Viewer,
                }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
