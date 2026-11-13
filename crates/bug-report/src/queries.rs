//! 本人と管理者の読取面。提出本文と下書きを分離する。
use crate::{
    service::audit, store, BugReportService, Error, ReportDetail, ReportPage, ReportSummary,
    ReporterPolicy,
};
use authz::AuthContext;
use storage::{
    audit::{AuditAction, Chain},
    db_tenant::begin_tenant_tx,
};
use uuid::Uuid;
impl BugReportService {
    pub async fn reporter_policy(
        &self,
        ctx: &AuthContext,
        trace: &str,
    ) -> Result<ReporterPolicy, Error> {
        self.member(ctx).await?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let policy = store::policy(&mut tx, ctx).await?;
        audit(
            &mut tx,
            ctx,
            AuditAction::BugReportRead,
            "policy",
            trace,
            serde_json::json!({}),
            Chain::No,
        )
        .await?;
        tx.commit().await?;
        Ok(ReporterPolicy {
            policy,
            destination_available: self.destination_version.is_some(),
            destination_version: self.destination_version,
        })
    }
    pub async fn list(
        &self,
        ctx: &AuthContext,
        admin: bool,
        cursor: Option<Uuid>,
        trace: &str,
    ) -> Result<ReportPage, Error> {
        if admin {
            self.admin(ctx).await?;
        } else {
            self.member(ctx).await?;
        }
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let rows: Vec<(Uuid,serde_json::Value,i64,String)> = sqlx::query_as("SELECT r.id,r.snapshot->'status',r.rev,CASE WHEN $3 THEN s.payload->>'title' ELSE r.snapshot->'content'->>'title' END FROM bug_report r LEFT JOIN bug_report_submission s ON s.tenant_id=r.tenant_id AND s.org=r.org AND s.report_id=r.id AND s.submission_rev=(r.snapshot->>'submission_rev')::bigint WHERE r.tenant_id=$1 AND r.org=$2 AND r.body_purged_at IS NULL AND (($3 AND r.status<>'draft' AND (r.snapshot->>'submission_rev')::bigint>0) OR (NOT $3 AND r.reporter_id=$4)) AND ($5::uuid IS NULL OR (r.created_at,r.id)<(SELECT c.created_at,c.id FROM bug_report c WHERE c.tenant_id=$1 AND c.org=$2 AND c.id=$5)) ORDER BY r.created_at DESC,r.id DESC LIMIT 51")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(admin).bind(&ctx.principal().id).bind(cursor).fetch_all(&mut *tx).await?;
        let more = rows.len() > 50;
        let items: Vec<ReportSummary> = rows
            .into_iter()
            .take(50)
            .map(|(id, status, rev, title)| {
                Ok(ReportSummary {
                    id,
                    status: serde_json::from_value(status).map_err(|_| Error::Invalid)?,
                    rev,
                    title,
                })
            })
            .collect::<Result<_, Error>>()?;
        let next_cursor = if more {
            items.last().map(|r| r.id)
        } else {
            None
        };
        audit(
            &mut tx,
            ctx,
            AuditAction::BugReportRead,
            "list",
            trace,
            serde_json::json!({"admin":admin}),
            Chain::No,
        )
        .await?;
        tx.commit().await?;
        Ok(ReportPage { items, next_cursor })
    }
    pub async fn detail(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        admin: bool,
        trace: &str,
    ) -> Result<ReportDetail, Error> {
        if admin {
            self.admin(ctx).await?;
        } else {
            self.member(ctx).await?;
        }
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let mut report = store::report(&mut tx, ctx, id).await?;
        if !admin && report.reporter_id != ctx.principal().id {
            return Err(Error::NotFound);
        }
        if admin && (report.submission_rev == 0 || report.status == crate::BugReportStatus::Draft) {
            return Err(Error::NotFound);
        }
        let value: Option<serde_json::Value> = sqlx::query_scalar("SELECT jsonb_build_object('content',payload,'content_consent',content_consent,'sink_profile_id',sink_profile_id,'sink_config_version',sink_config_version,'notice_version',notice_version,'policy_rev',policy_rev,'consent_at',consent_at,'approved_by',approved_by,'approved_at',approved_at,'rejected_by',rejected_by,'rejected_at',rejected_at,'rejection_reason',rejection_reason) FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND report_id=$3 AND submission_rev=$4 AND body_purged_at IS NULL")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(id).bind(report.submission_rev).fetch_optional(&mut *tx).await?;
        let submission: Option<crate::SubmissionDetail> = value
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::Invalid)?;
        if admin {
            if let Some(ref submitted) = submission {
                report.content = submitted.content.clone();
            }
        }
        audit(
            &mut tx,
            ctx,
            AuditAction::BugReportRead,
            &id.to_string(),
            trace,
            serde_json::json!({"admin":admin}),
            Chain::No,
        )
        .await?;
        tx.commit().await?;
        Ok(ReportDetail { report, submission })
    }
}
