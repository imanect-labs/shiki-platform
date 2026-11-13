//! 専用 transactional outbox。HTTP は claim の commit 後だけで実行する。
use crate::{
    github::{Dispatch, GitHubIssueSink},
    service::{audit, save},
    sink::{ConfirmedBugReport, DeliveryIdentity},
    store, BugReportPolicy, BugReportStatus as Status, Error,
};
use authz::{AuthContext, AuthzClient, Consistency, Principal, PrincipalKind, Relation, TenantId};
use sqlx::{PgConnection, PgPool, Row};
use std::sync::Arc;
use storage::{
    audit::{AuditAction, Chain},
    db_tenant::{begin_cross_tenant_tx, begin_tenant_tx},
};
use uuid::Uuid;

pub struct DeliveryWorker {
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    pub(crate) sink: Arc<GitHubIssueSink>,
    owner: Uuid,
}
pub(crate) struct Claim {
    pub(crate) ctx: AuthContext,
    pub(crate) identity: DeliveryIdentity,
    pub(crate) report: ConfirmedBugReport,
    pub(crate) fence: i64,
    pub(crate) reconcile: bool,
}
impl DeliveryWorker {
    pub fn new(pool: PgPool, authz: Arc<dyn AuthzClient>, sink: Arc<GitHubIssueSink>) -> Self {
        Self {
            pool,
            authz,
            sink,
            owner: Uuid::new_v4(),
        }
    }
    /// repo の全 replica を直列化し、一周の HTTP は最大8配送。失敗は監視に分類名だけを出す。
    pub async fn run_batch(&self) -> Result<usize, Error> {
        let mut serial = self.pool.begin().await?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(845237091)")
            .fetch_one(&mut *serial)
            .await?;
        if !locked {
            return Ok(0);
        }
        let mut enumeration =
            begin_cross_tenant_tx(&self.pool, "bug-report delivery tenant enumeration only")
                .await?;
        // tenant-sql: allow(本文を含まない有界の配送対象 tenant/org 列挙。以降は tenant tx)
        let scopes: Vec<(String,String)> = sqlx::query_as("SELECT tenant_id,org FROM bug_report_delivery WHERE outcome IN ('queued','sending','delivery_unknown') AND next_attempt_at<=now() AND (lease_until IS NULL OR lease_until<now()) GROUP BY tenant_id,org ORDER BY min(next_attempt_at),tenant_id,org LIMIT 8")
            .fetch_all(&mut *enumeration).await?;
        enumeration.commit().await?;
        let mut done = 0;
        for (tenant, org) in scopes {
            let tenant = TenantId::parse(&tenant).map_err(|_| Error::Invalid)?;
            if let Some(claim) = self.claim(&tenant, &org).await? {
                let result = if claim.reconcile {
                    self.sink.lookup(&claim.identity).await
                } else {
                    self.sink.dispatch(&claim.report, &claim.identity).await
                };
                self.finish(&claim, result).await?;
                done += 1;
            }
        }
        serial.commit().await?;
        Ok(done)
    }
    pub(crate) async fn claim(&self, tenant: &TenantId, org: &str) -> Result<Option<Claim>, Error> {
        let mut tx = begin_tenant_tx(&self.pool, tenant).await?;
        // 全 tenant/replica で共有する installation quota。本文を読む経路は tenant tx のまま。
        let cooling: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM bug_report_destination_cooldown WHERE installation_id=$1 AND next_attempt_at>now())")
            .bind(self.sink.installation_id()).fetch_one(&mut *tx).await?;
        if cooling {
            return Ok(None);
        }
        // org lock の順序は command/set_policy と共通。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("bug-report:{tenant}:{org}"))
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query("SELECT d.id,d.report_id,d.submission_rev,d.fence,d.outcome,d.attempt_count,(d.created_at>now()-interval '24 hours') AS within_deadline,s.principal,s.payload,s.payload_hash,s.sink_profile_id,s.sink_config_version,s.content_consent,s.approved_by FROM bug_report_delivery d JOIN bug_report_submission s USING (tenant_id,org,report_id,submission_rev) WHERE d.tenant_id=$1 AND d.org=$2 AND s.principal IS NOT NULL AND s.principal <> 'null'::jsonb AND d.outcome IN ('queued','sending','delivery_unknown') AND d.next_attempt_at<=now() AND (d.lease_until IS NULL OR d.lease_until<now()) ORDER BY d.next_attempt_at,d.id LIMIT 1 FOR UPDATE OF d SKIP LOCKED")
            .bind(tenant.as_str()).bind(org).fetch_optional(&mut *tx).await?;
        let Some(row) = row else { return Ok(None) };
        let principal: Principal =
            serde_json::from_value(row.try_get("principal")?).map_err(|_| Error::Invalid)?;
        let ctx = AuthContext::new(principal, org.to_owned(), tenant.clone());
        let identity = DeliveryIdentity {
            id: row.try_get("id")?,
            report_id: row.try_get("report_id")?,
            submission_rev: row.try_get("submission_rev")?,
        };
        let mut report = store::report(&mut tx, &ctx, identity.report_id).await?;
        let reconcile = row.try_get::<String, _>("outcome")? != "queued";
        let policy = store::policy(&mut tx, &ctx).await?;
        let member = ctx.principal().kind == PrincipalKind::User
            && self
                .authz
                .check(
                    &ctx.subject(),
                    Relation::Member,
                    &ctx.ns().organization(ctx.org()),
                    Consistency::HigherConsistency,
                )
                .await?;
        let shared: bool = row.try_get("content_consent")?;
        let approved: Option<String> = row.try_get("approved_by")?;
        let valid = row.try_get::<i32, _>("attempt_count")? < 5
            && row.try_get::<bool, _>("within_deadline")?
            && member
            && policy.enabled
            && (!shared || policy.allow_content_sharing)
            && (!policy.require_approval || approved.is_some())
            && row.try_get::<String, _>("sink_profile_id")? == "github"
            && row.try_get::<i64, _>("sink_config_version")? == self.sink.config_version();
        if !reconcile
            && (!valid
                || report.status != Status::Queued
                || report.submission_rev != identity.submission_rev)
        {
            sqlx::query("UPDATE bug_report_delivery SET outcome='held',error_code='policy',lease_until=NULL WHERE tenant_id=$1 AND org=$2 AND id=$3")
                .bind(ctx.tenant_id()).bind(org).bind(identity.id).execute(&mut *tx).await?;
            if report.status == Status::Queued {
                report.status = Status::Held;
                save(&mut tx, &ctx, &mut report).await?;
            }
            audit(
                &mut tx,
                &ctx,
                AuditAction::BugReportFailed,
                &identity.report_id.to_string(),
                "delivery",
                serde_json::json!({"actor":"worker","error_code":"policy"}),
                Chain::Yes,
            )
            .await?;
            tx.commit().await?;
            return Ok(None);
        }
        let fence: i64 = row.try_get::<i64, _>("fence")? + 1;
        sqlx::query("UPDATE bug_report_delivery SET outcome=$4,attempt_count=attempt_count+$5,lease_owner=$6,lease_until=now()+interval '90 seconds',fence=$7 WHERE tenant_id=$1 AND org=$2 AND id=$3")
            .bind(ctx.tenant_id()).bind(org).bind(identity.id).bind(if reconcile { "delivery_unknown" } else { "sending" }).bind(i32::from(!reconcile)).bind(self.owner).bind(fence).execute(&mut *tx).await?;
        report.status = if reconcile {
            Status::DeliveryUnknown
        } else {
            Status::Sending
        };
        save(&mut tx, &ctx, &mut report).await?;
        audit(
            &mut tx,
            &ctx,
            if reconcile {
                AuditAction::BugReportReconcile
            } else {
                AuditAction::BugReportDispatch
            },
            &identity.report_id.to_string(),
            "delivery",
            serde_json::json!({"actor":"worker","delivery_id":identity.id,"fence":fence}),
            Chain::Yes,
        )
        .await?;
        let payload =
            serde_json::from_value(row.try_get("payload")?).map_err(|_| Error::Invalid)?;
        let payload_hash = row.try_get("payload_hash")?;
        tx.commit().await?;
        Ok(Some(Claim {
            ctx,
            identity,
            report: ConfirmedBugReport {
                content: payload,
                payload_hash,
            },
            fence,
            reconcile,
        }))
    }
    pub(crate) async fn finish(&self, claim: &Claim, result: Dispatch) -> Result<(), Error> {
        let ctx = &claim.ctx;
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        store::lock(&mut tx, ctx).await?;
        let row: Option<(i32,bool)> = sqlx::query_as("SELECT attempt_count,created_at>now()-interval '24 hours' FROM bug_report_delivery WHERE tenant_id=$1 AND org=$2 AND id=$3 AND lease_owner=$4 AND fence=$5 AND lease_until>now() FOR UPDATE")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(claim.identity.id).bind(self.owner).bind(claim.fence).fetch_optional(&mut *tx).await?;
        let Some((attempt, within_deadline)) = row else {
            return Err(Error::Conflict);
        };
        // 回数上限で当該行を停止する場合も、配送先全体の待機は失わない。
        if let Dispatch::Retry {
            after,
            code: "rate_limit",
        } = &result
        {
            sqlx::query("INSERT INTO bug_report_destination_cooldown (installation_id,next_attempt_at) VALUES ($1,now()+$2*interval '1 second') ON CONFLICT (installation_id) DO UPDATE SET next_attempt_at=GREATEST(bug_report_destination_cooldown.next_attempt_at,EXCLUDED.next_attempt_at)")
                .bind(self.sink.installation_id()).bind(i64::try_from(*after).unwrap_or(i64::MAX / 1000).min(315_360_000)).execute(&mut *tx).await?;
        }
        let mut delay: i64 = 0;
        let mut external_id = None;
        let mut url = None;
        let (status, outcome, code, action) = match result {
            Dispatch::Sent { id, url: u } => {
                external_id = Some(id);
                url = Some(u);
                (Status::Sent, "sent", None, AuditAction::BugReportSent)
            }
            Dispatch::Retry { after, code }
                if !claim.reconcile && attempt < 5 && within_deadline && after < 86400 =>
            {
                let backoff =
                    30_u64 * 2_u64.pow(u32::try_from(attempt).map_err(|_| Error::Invalid)?);
                delay = i64::try_from(after.max(backoff + rand::random::<u64>() % 30))
                    .unwrap_or(i64::MAX / 1000)
                    .min(86400);
                (
                    Status::Queued,
                    "queued",
                    Some(code),
                    AuditAction::BugReportFailed,
                )
            }
            Dispatch::Retry { after, .. } if claim.reconcile => {
                // 照合の rate limit でも再 POST せず、結果不明のまま待つ。
                delay = i64::try_from(after)
                    .unwrap_or(315_360_000)
                    .clamp(3600, 315_360_000);
                (
                    Status::DeliveryUnknown,
                    "delivery_unknown",
                    Some("rate_limit"),
                    AuditAction::BugReportReconcile,
                )
            }
            Dispatch::Unknown => {
                delay = 3600;
                (
                    Status::DeliveryUnknown,
                    "delivery_unknown",
                    Some("unknown"),
                    AuditAction::BugReportReconcile,
                )
            }
            Dispatch::Failed(code) | Dispatch::Retry { code, .. } => (
                Status::Failed,
                "failed",
                Some(code),
                AuditAction::BugReportFailed,
            ),
        };
        sqlx::query("UPDATE bug_report_delivery SET outcome=$6,error_code=$7,external_id=$8,url=$9,next_attempt_at=now()+$10*interval '1 second',lease_until=NULL,lease_owner=NULL WHERE tenant_id=$1 AND org=$2 AND id=$3 AND lease_owner=$4 AND fence=$5")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(claim.identity.id).bind(self.owner).bind(claim.fence).bind(outcome).bind(code).bind(external_id).bind(url).bind(delay).execute(&mut *tx).await?;
        let mut report = store::report(&mut tx, ctx, claim.identity.report_id).await?;
        report.status = status;
        save(&mut tx, ctx, &mut report).await?;
        audit(&mut tx, ctx, action, &report.id.to_string(), "delivery", serde_json::json!({"actor":"worker","delivery_id":claim.identity.id,"outcome":outcome,"error_code":code}), Chain::Yes).await?;
        tx.commit().await?;
        Ok(())
    }
}
/// ポリシー変更と未着手配送停止を同じ transaction で実行する。
pub(crate) async fn stop_unstarted(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    policy: &BugReportPolicy,
) -> Result<(), Error> {
    sqlx::query("UPDATE bug_report_delivery d SET outcome='held',error_code='policy' FROM bug_report_submission s WHERE d.tenant_id=$1 AND d.org=$2 AND s.tenant_id=d.tenant_id AND s.org=d.org AND s.report_id=d.report_id AND s.submission_rev=d.submission_rev AND d.outcome='queued' AND (NOT $3 OR (s.content_consent AND NOT $4) OR ($5 AND s.approved_by IS NULL))")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(policy.enabled).bind(policy.allow_content_sharing).bind(policy.require_approval).execute(&mut *conn).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT r.id FROM bug_report r JOIN bug_report_delivery d ON d.tenant_id=r.tenant_id AND d.org=r.org AND d.report_id=r.id AND d.submission_rev=(r.snapshot->>'submission_rev')::bigint WHERE r.tenant_id=$1 AND r.org=$2 AND r.status='queued' AND d.outcome='held'")
        .bind(ctx.tenant_id()).bind(ctx.org()).fetch_all(&mut *conn).await?;
    for id in ids {
        let mut r = store::report(conn, ctx, id).await?;
        r.status = Status::Held;
        save(conn, ctx, &mut r).await?;
    }
    Ok(())
}
