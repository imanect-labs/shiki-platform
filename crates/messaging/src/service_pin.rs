//! 発言のピン留め（チャンネルの参加者に共通の「大事な発言」の一覧・design §4.14）。
//!
//! - 付け外しできるのは、そのチャンネルに投稿できる人（アーカイブ中は不可）。読めない発言
//!   （参加前の発言・削除・期限切れ）はピン留めできない。
//! - 一覧は通常の読取と同じ関数（`fetch_messages`）を通り、参加前の境界が掛かる。
//! - 付け外しは監査に残し、`updated_at` を進めて配信の差分で参加者の画面に届ける。

use authz::AuthContext;
use storage::audit::AuditAction;
use storage::db_tenant::begin_tenant_tx;
use uuid::Uuid;

use crate::pubsub::Signal;
use crate::service_message::audit_message;
use crate::store_message::{self as sm, MessageFilter};
use crate::{Error, MessageView, MessagingService};

impl MessagingService {
    /// 発言をピン留めする・外す（冪等）。
    ///
    /// # Errors
    ///
    /// 発言が無い・読めない（`NotFound`）、投稿できない・削除済み（`Forbidden`）、DB の失敗。
    pub async fn set_pinned(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        pinned: bool,
        trace: Option<&str>,
    ) -> Result<MessageView, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let raw = sm::raw_message(&mut tx, ctx, id)
            .await?
            .ok_or(Error::NotFound)?;
        let c = self.readable_channel(&mut tx, ctx, raw.channel_id).await?;
        self.require_visible(&mut tx, ctx, id).await?;
        if raw.deleted_at.is_some() || raw.expired_at.is_some() {
            return Err(Error::Forbidden("削除・期限切れの発言はピン留めできません"));
        }
        self.require_post(ctx, &c, trace).await?;
        let me = ctx.principal().id.clone();
        sm::set_pinned(&mut tx, ctx, id, pinned.then_some(me.as_str())).await?;
        audit_message(
            &mut tx,
            ctx,
            if pinned {
                AuditAction::MessagingMessagePin
            } else {
                AuditAction::MessagingMessageUnpin
            },
            id,
            raw.channel_id,
            trace,
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await?;
        self.hub
            .publish(ctx.tenant_id(), raw.channel_id, Signal::Changed)
            .await;
        self.message(ctx, id).await
    }

    /// ピン留めした発言（新しくピン留めした順・最大 100 件・読める発言だけ）。
    ///
    /// # Errors
    ///
    /// チャンネルが無い・読めない（`NotFound`）、DB・認可基盤の失敗。
    pub async fn pinned_messages(
        &self,
        ctx: &AuthContext,
        channel: Uuid,
    ) -> Result<Vec<MessageView>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        self.readable_channel(&mut tx, ctx, channel).await?;
        let rows = sm::fetch_messages(
            &mut tx,
            ctx,
            &ctx.principal().id,
            MessageFilter::Pinned { channel },
        )
        .await?;
        let parts = self.view_parts(&mut tx, ctx, rows).await?;
        tx.commit().await?;
        self.finish_views(ctx, parts).await
    }
}
