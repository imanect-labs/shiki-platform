//! 本文の全複製を有界バッチで消去する。配送と同じ org lock で直列化する。
use crate::Error;
use authz::TenantId;
use sqlx::PgPool;
use storage::db_tenant::{begin_cross_tenant_tx, begin_tenant_tx};

pub struct Retention {
    pool: PgPool,
}
impl Retention {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 起動時と毎時実行。無効化後も既存データの消去を続ける。
    pub async fn run_batch(&self) -> Result<u64, Error> {
        let mut enumeration =
            begin_cross_tenant_tx(&self.pool, "bug-report expired scope enumeration only").await?;
        // tenant-sql: allow(本文を含まない期限切れ scope の有界列挙。消去は tenant tx)
        let scopes: Vec<(String,String)> = sqlx::query_as("SELECT tenant_id,org FROM (SELECT tenant_id,org FROM bug_report WHERE body_purged_at IS NULL AND ((status='draft' AND updated_at<=now()-interval '30 days') OR (status<>'draft' AND COALESCE((SELECT max(s.consent_at) FROM bug_report_submission s WHERE s.tenant_id=bug_report.tenant_id AND s.org=bug_report.org AND s.report_id=bug_report.id),created_at)<=now()-interval '90 days')) UNION SELECT tenant_id,org FROM help_resolution_case WHERE expires_at<=now() UNION SELECT tenant_id,org FROM bug_report_submission WHERE body_purged_at IS NULL AND consent_at<=now()-interval '90 days' UNION SELECT tenant_id,org FROM bug_report_command WHERE created_at<=now()-interval '30 days') scopes ORDER BY tenant_id,org LIMIT 100")
            .fetch_all(&mut *enumeration).await?;
        // tenant-sql: allow(運用監視用の全体集計のみ。本文・識別子は取得もラベル化もしない)
        let (pending, queued, failed, unknown, rate_limited, oldest_seconds): (i64,i64,i64,i64,i64,f64) = sqlx::query_as("SELECT count(*) FILTER (WHERE status='pending_approval'),count(*) FILTER (WHERE status='queued'),(SELECT count(*) FROM bug_report_delivery WHERE outcome='failed'),(SELECT count(*) FROM bug_report_delivery WHERE outcome='delivery_unknown'),(SELECT count(*) FROM bug_report_delivery WHERE error_code='rate_limit'),COALESCE(EXTRACT(EPOCH FROM now()-min(updated_at) FILTER (WHERE status IN ('pending_approval','queued')))::float8,0) FROM bug_report WHERE body_purged_at IS NULL")
            .fetch_one(&mut *enumeration).await?;
        tracing::info!(
            pending,
            queued,
            failed,
            unknown,
            rate_limited,
            oldest_seconds,
            "bug-report queue health"
        );
        enumeration.commit().await?;
        let mut total = 0;
        for (tenant, org) in scopes {
            let tenant = TenantId::parse(&tenant).map_err(|_| Error::Invalid)?;
            total += self.sweep(&tenant, &org).await?;
        }
        tracing::info!(purged = total, "bug-report retention batch complete");
        Ok(total)
    }

    pub async fn sweep(&self, tenant: &TenantId, org: &str) -> Result<u64, Error> {
        let mut tx = begin_tenant_tx(&self.pool, tenant).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("bug-report:{tenant}:{org}"))
            .execute(&mut *tx)
            .await?;
        let mut count = sqlx::query("DELETE FROM help_resolution_case WHERE tenant_id=$1 AND org=$2 AND id IN (SELECT id FROM help_resolution_case WHERE tenant_id=$1 AND org=$2 AND expires_at<=now() LIMIT 100)")
            .bind(tenant.as_str()).bind(org).execute(&mut *tx).await?.rows_affected();
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT r.id FROM bug_report r WHERE r.tenant_id=$1 AND r.org=$2 AND r.body_purged_at IS NULL AND ((r.status='draft' AND r.updated_at<=now()-interval '30 days') OR (r.status<>'draft' AND COALESCE((SELECT max(s.consent_at) FROM bug_report_submission s WHERE s.tenant_id=$1 AND s.org=$2 AND s.report_id=r.id),r.created_at)<=now()-interval '90 days')) ORDER BY r.updated_at,r.id LIMIT 100 FOR UPDATE")
            .bind(tenant.as_str()).bind(org).fetch_all(&mut *tx).await?;
        // 本文は snapshot / submission / 冪等応答に複製される。lease を fence して復活を防ぐ。
        count += sqlx::query("UPDATE bug_report SET snapshot='{}',body_purged_at=now(),rev=rev+1 WHERE tenant_id=$1 AND org=$2 AND id=ANY($3)")
            .bind(tenant.as_str()).bind(org).bind(&ids).execute(&mut *tx).await?.rows_affected();
        sqlx::query("UPDATE bug_report_delivery SET outcome=CASE WHEN outcome='queued' THEN 'held' WHEN outcome='sending' THEN 'delivery_unknown' ELSE outcome END,error_code=CASE WHEN outcome IN ('queued','sending') THEN 'retention' ELSE error_code END,lease_owner=NULL,lease_until=NULL,fence=fence+1 WHERE tenant_id=$1 AND org=$2 AND report_id=ANY($3)")
            .bind(tenant.as_str()).bind(org).bind(&ids).execute(&mut *tx).await?;
        count += sqlx::query("UPDATE bug_report_submission SET payload='{}',principal=NULL,rejection_reason=NULL,body_purged_at=now() WHERE tenant_id=$1 AND org=$2 AND body_purged_at IS NULL AND (report_id=ANY($3) OR (report_id,submission_rev) IN (SELECT report_id,submission_rev FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND body_purged_at IS NULL AND consent_at<=now()-interval '90 days' LIMIT 100))")
            .bind(tenant.as_str()).bind(org).bind(&ids).execute(&mut *tx).await?.rows_affected();
        count += sqlx::query("DELETE FROM bug_report_command WHERE tenant_id=$1 AND org=$2 AND (report_id=ANY($3) OR (actor_id,operation,idempotency_key) IN (SELECT actor_id,operation,idempotency_key FROM bug_report_command WHERE tenant_id=$1 AND org=$2 AND created_at<=now()-interval '30 days' LIMIT 1000))")
            .bind(tenant.as_str()).bind(org).bind(&ids).execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(count)
    }
}
