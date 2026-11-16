//! AI が作ったチャットの承認待ちを、照会者への DM で知らせる（design §4.14・Task 14.17）。
//!
//! 見張りの対象は `channel_agent_reply` に残した行（`chat_thread_id` があるもの・24 時間以内）で、
//! 周期タスクが拾う。プロセスが再起動しても止まらず、複数のレプリカが動いても
//! `notified_approvals` への追記を先に取った 1 つだけが知らせる（二重通知しない）。
//! 承認できるのは従来どおり照会者本人だけで、DM は承認カードへの案内だけを運ぶ。

use authz::{AuthContext, Principal, PrincipalKind, TenantId};
use std::sync::Arc;
use std::time::Duration;
use storage::db_tenant::{begin_cross_tenant_tx, begin_tenant_tx};
use uuid::Uuid;

use crate::{AgentRuntime, Error, MessagingService};

/// 見張りの周期。
const TICK: Duration = Duration::from_secs(10);
/// 1 周で見る行の上限。
const BATCH: i64 = 500;

#[derive(sqlx::FromRow)]
struct WatchRow {
    tenant_id: String,
    org: String,
    id: Uuid,
    invoked_by: String,
    chat_thread_id: Uuid,
    chat_title: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl MessagingService {
    /// 承認待ちの見張りを起動する（AI が構成されている構成だけ）。
    pub fn spawn_approval_watcher(
        self: &Arc<Self>,
        runtime: Arc<dyn AgentRuntime>,
    ) -> tokio::task::JoinHandle<()> {
        let svc = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(e) = svc.watch_approvals_once(&runtime).await {
                    tracing::warn!(error = %e, "messaging: 承認待ちの見張りに失敗");
                }
            }
        })
    }

    /// 見張りを 1 周する。知らせた件数を返す。
    ///
    /// # Errors
    ///
    /// DB の失敗（チャット側の失敗は行ごとに飛ばす）。
    pub async fn watch_approvals_once(
        &self,
        runtime: &Arc<dyn AgentRuntime>,
    ) -> Result<usize, Error> {
        let mut sent = 0;
        // 24 時間以内の対象を、古い順にページを送って全部見る（上限で新しい分だけに偏らない）。
        let mut cursor: Option<(chrono::DateTime<chrono::Utc>, Uuid)> = None;
        loop {
            let rows = self.watch_page(cursor).await?;
            let last = rows.last().map(|r| (r.created_at, r.id));
            for r in &rows {
                sent += self.watch_one(runtime, r).await?;
            }
            if rows.len() < BATCH as usize {
                break;
            }
            cursor = last;
        }
        Ok(sent)
    }

    async fn watch_page(
        &self,
        after: Option<(chrono::DateTime<chrono::Utc>, Uuid)>,
    ) -> Result<Vec<WatchRow>, Error> {
        let mut tx =
            begin_cross_tenant_tx(&self.pool, "messaging approval watch enumeration only").await?;
        let (at, id) = after.unzip();
        // tenant-sql: allow(AI が作ったチャットの有界列挙だけ。本文は読まない。通知は tenant tx と照会者の権限)
        let rows: Vec<WatchRow> = sqlx::query_as(
            "SELECT tenant_id, org, id, invoked_by, chat_thread_id, chat_title, created_at \
             FROM channel_agent_reply WHERE chat_thread_id IS NOT NULL \
               AND created_at > now() - interval '24 hours' \
               AND ($2::timestamptz IS NULL OR (created_at, id) > ($2, $3)) \
             ORDER BY created_at, id LIMIT $1",
        )
        .bind(BATCH)
        .bind(at)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows)
    }

    async fn watch_one(
        &self,
        runtime: &Arc<dyn AgentRuntime>,
        r: &WatchRow,
    ) -> Result<usize, Error> {
        let Ok(tenant) = TenantId::parse(&r.tenant_id) else {
            return Ok(0);
        };
        // 照会者の AuthContext（チャットの持ち主＝照会者・権限は増えない）。
        let ctx = AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: r.invoked_by.clone(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(r.tenant_id.clone()),
            },
            r.org.clone(),
            tenant,
        );
        let Ok((pending, _active)) = runtime.chat_status(&ctx, r.chat_thread_id).await else {
            return Ok(0);
        };
        let title = r.chat_title.clone().unwrap_or_default();
        let mut sent = 0;
        for p in pending {
            let text = format!(
                "チャット「{title}」で承認が必要です（{}）。内容を確かめてから承認してください: /c/{}",
                p.tool_name, r.chat_thread_id
            );
            match self.notify_once(&ctx, r.id, &p.approval_id, &text).await {
                Ok(true) => sent += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "messaging: 承認待ちを DM で知らせられません"),
            }
        }
        Ok(sent)
    }

    /// 承認待ちを 1 度だけ知らせる。「知らせた」の記録と DM の書込を同じトランザクションで確定する
    /// （途中で落ちても、記録だけ残って通知が失われる状態を作らない・複数レプリカでも 1 つだけが書く）。
    async fn notify_once(
        &self,
        ctx: &AuthContext,
        reply: Uuid,
        approval: &str,
        text: &str,
    ) -> Result<bool, Error> {
        let channel = self.agent_dm(ctx).await?;
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let claimed = sqlx::query(
            "UPDATE channel_agent_reply SET notified_approvals = array_append(notified_approvals, $4) \
             WHERE tenant_id = $1 AND org = $2 AND id = $3 AND NOT ($4 = ANY(notified_approvals))",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(reply)
        .bind(approval)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !claimed {
            return Ok(false);
        }
        self.write_owner_notice(&mut tx, ctx, channel, text).await?;
        tx.commit().await?;
        self.hub
            .publish(ctx.tenant_id(), channel, crate::pubsub::Signal::Changed)
            .await;
        Ok(true)
    }
}
