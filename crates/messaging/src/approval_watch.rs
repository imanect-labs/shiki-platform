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
        let mut tx =
            begin_cross_tenant_tx(&self.pool, "messaging approval watch enumeration only").await?;
        // tenant-sql: allow(AI が作ったチャットの有界列挙だけ。本文は読まない。通知は tenant tx と照会者の権限)
        let rows: Vec<WatchRow> = sqlx::query_as(
            "SELECT tenant_id, org, id, invoked_by, chat_thread_id, chat_title FROM channel_agent_reply \
             WHERE chat_thread_id IS NOT NULL AND created_at > now() - interval '24 hours' \
             ORDER BY created_at DESC LIMIT $1",
        )
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut sent = 0;
        for r in rows {
            let Ok(tenant) = TenantId::parse(&r.tenant_id) else {
                continue;
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
                continue;
            };
            let title = r.chat_title.clone().unwrap_or_default();
            for p in pending {
                if !self.claim_notice(&ctx, r.id, &p.approval_id).await? {
                    continue;
                }
                let text = format!(
                    "チャット「{title}」で承認が必要です（{}）。内容を確かめてから承認してください: /c/{}",
                    p.tool_name, r.chat_thread_id
                );
                if let Err(e) = self.notify_owner(&ctx, &text).await {
                    // 知らせられなかったら取り下げて次の周で再試行する。
                    tracing::warn!(error = %e, "messaging: 承認待ちを DM で知らせられません");
                    self.release_notice(&ctx, r.id, &p.approval_id).await?;
                } else {
                    sent += 1;
                }
            }
        }
        Ok(sent)
    }

    /// 承認待ちを「知らせた」として先に記録する（取れたら true・複数レプリカで 1 つだけが知らせる）。
    async fn claim_notice(
        &self,
        ctx: &AuthContext,
        reply: Uuid,
        approval: &str,
    ) -> Result<bool, Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let n = sqlx::query(
            "UPDATE channel_agent_reply SET notified_approvals = array_append(notified_approvals, $4) \
             WHERE tenant_id = $1 AND org = $2 AND id = $3 AND NOT ($4 = ANY(notified_approvals))",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(reply)
        .bind(approval)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(n == 1)
    }

    async fn release_notice(
        &self,
        ctx: &AuthContext,
        reply: Uuid,
        approval: &str,
    ) -> Result<(), Error> {
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        sqlx::query(
            "UPDATE channel_agent_reply SET notified_approvals = array_remove(notified_approvals, $4) \
             WHERE tenant_id = $1 AND org = $2 AND id = $3",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(reply)
        .bind(approval)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
