//! 発言の保存期間（design §4.14 保存期間・Task 14.18）。
//!
//! 既定は無期限（`messaging_settings.retention_days` が NULL）。期限を設定した org だけ、期限より古い
//! 発言の本文・改訂・リアクション・メンション・プレビューを消し、検索の索引（`search_text`）からも除く。
//! 行そのものは「この発言は保存期間を過ぎました」として残す（スレッドの返信を孤立させない）。

use authz::{AuthContext, Principal, PrincipalKind, TenantId};
use sqlx::PgPool;
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use storage::db_tenant::{begin_cross_tenant_tx, begin_tenant_tx};

use crate::Error;

/// 1 回のトランザクションで消す上限（長いロックを避ける）。
const BATCH: i64 = 1_000;
const ACTOR: &str = "system:messaging-retention";

/// 保存期間の削除を一巡する。消した発言の数を返す。
///
/// # Errors
///
/// DB の失敗。
pub async fn sweep(pool: &PgPool) -> Result<u64, Error> {
    let mut tx = begin_cross_tenant_tx(pool, "messaging retention scope enumeration only").await?;
    // tenant-sql: allow(保存期間を設定した org の有界列挙だけ。本文は読まない。消去は tenant tx)
    let scopes: Vec<(String, String, i32)> = sqlx::query_as(
        "SELECT tenant_id, org, retention_days FROM messaging_settings \
         WHERE retention_days IS NOT NULL ORDER BY tenant_id, org LIMIT 10000",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut total = 0;
    for (tenant, org, days) in scopes {
        let Ok(tenant) = TenantId::parse(&tenant) else {
            continue;
        };
        // 監査ログの保存期間が後から延びても、それより短い期限では消さない（eDiscovery の前提・
        // design §4.14 保存期間）。設定時の検証に加え、消す時点でも監査の保存期間と比べる。
        // バッチごとに引き直す（途中で監査の保存期間が延びたら、後続のバッチから延びた日数で判定する）。
        loop {
            let audit_days = storage::audit_retention::AuditRetention::new(pool.clone())
                .retention_days(tenant.as_str())
                .await?;
            let n = sweep_org(pool, &tenant, &org, days.max(audit_days)).await?;
            total += n;
            if n < BATCH as u64 {
                break;
            }
        }
    }
    Ok(total)
}

/// 1 つの org の期限切れを最大 [`BATCH`] 件消す。
///
/// # Errors
///
/// DB の失敗。
pub async fn sweep_org(
    pool: &PgPool,
    tenant: &TenantId,
    org: &str,
    days: i32,
) -> Result<u64, Error> {
    let mut tx = begin_tenant_tx(pool, tenant).await?;
    let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT id FROM channel_message WHERE tenant_id = $1 AND org = $2 AND expired_at IS NULL \
           AND created_at < now() - make_interval(days => $3) ORDER BY created_at LIMIT $4 FOR UPDATE",
    )
    .bind(tenant.as_str())
    .bind(org)
    .bind(days)
    .bind(BATCH)
    .fetch_all(&mut *tx)
    .await?;
    if ids.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }
    for sql in [
        "DELETE FROM channel_message_revision WHERE tenant_id = $1 AND org = $2 AND message_id = ANY($3)",
        "DELETE FROM channel_message_reaction WHERE tenant_id = $1 AND org = $2 AND message_id = ANY($3)",
        "DELETE FROM channel_mention WHERE tenant_id = $1 AND org = $2 AND message_id = ANY($3)",
        "DELETE FROM channel_agent_reply WHERE tenant_id = $1 AND org = $2 AND trigger_message_id = ANY($3)",
        "UPDATE channel_message SET blocks = '[]'::jsonb, search_text = NULL, link_previews = '[]'::jsonb, \
           expired_at = now(), updated_at = clock_timestamp() \
         WHERE tenant_id = $1 AND org = $2 AND id = ANY($3)",
    ] {
        sqlx::query(sql)
            .bind(tenant.as_str())
            .bind(org)
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
    }
    let ctx = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: ACTOR.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.as_str().to_string()),
        },
        org.to_string(),
        tenant.clone(),
    );
    record_on(
        &mut tx,
        &ctx,
        AuditEntry {
            action: AuditAction::MessagingRetentionPurge,
            object_type: "organization",
            object_id: org,
            decision: Decision::Allow,
            trace_id: None,
            metadata: serde_json::json!({ "retention_days": days, "messages": ids.len() }),
        },
        Chain::Yes,
    )
    .await?;
    tx.commit().await?;
    Ok(ids.len() as u64)
}

/// 起動時と毎時、保存期間の削除を回す。
pub fn spawn_retention_timer(pool: PgPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_hours(1));
        loop {
            tick.tick().await;
            match sweep(&pool).await {
                Ok(n) if n > 0 => tracing::info!(purged = n, "messaging retention sweep"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "messaging retention sweep failed"),
            }
        }
    })
}
