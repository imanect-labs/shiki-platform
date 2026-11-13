use crate::{
    store::{self, json},
    BugReport, BugReportPolicy, BugReportStatus as Status, BugReportStore, Command, Error,
    ReportContent,
};
use authz::{AuthContext, AuthzClient, Consistency, PrincipalKind, Relation};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use std::sync::Arc;
use storage::{
    audit::{record_on, AuditAction, AuditEntry, Chain, Decision},
    db_tenant::begin_tenant_tx,
};
use uuid::Uuid;

pub struct BugReportService {
    pub(crate) store: BugReportStore,
    authz: Arc<dyn AuthzClient>,
    pub(crate) destination_version: Option<i64>,
}
impl BugReportService {
    pub fn new(store: BugReportStore, authz: Arc<dyn AuthzClient>) -> Self {
        Self {
            store,
            authz,
            destination_version: None,
        }
    }

    #[must_use]
    pub fn with_github_destination(mut self, version: i64) -> Self {
        self.destination_version = Some(version);
        self
    }
    pub(crate) async fn member(&self, ctx: &AuthContext) -> Result<(), Error> {
        if ctx.principal().kind != PrincipalKind::User
            || !self
                .authz
                .check(
                    &ctx.subject(),
                    Relation::Member,
                    &ctx.ns().organization(ctx.org()),
                    Consistency::HigherConsistency,
                )
                .await?
        {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
    pub(crate) async fn admin(&self, ctx: &AuthContext) -> Result<(), Error> {
        self.member(ctx).await?;
        if !authz::is_org_admin(self.authz.as_ref(), ctx, Consistency::HigherConsistency).await? {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
    pub async fn policy(&self, ctx: &AuthContext, trace: &str) -> Result<BugReportPolicy, Error> {
        self.admin(ctx).await?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let result = store::policy(&mut tx, ctx).await?;
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
        Ok(result)
    }
    pub async fn set_policy(
        &self,
        ctx: &AuthContext,
        mut policy: BugReportPolicy,
        trace: &str,
    ) -> Result<BugReportPolicy, Error> {
        self.admin(ctx).await?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        store::lock(&mut tx, ctx).await?;
        let old = store::policy(&mut tx, ctx).await?;
        if old.rev != policy.rev {
            return Err(Error::Conflict);
        }
        policy.rev += 1;
        sqlx::query("INSERT INTO bug_report_policy (tenant_id,org,settings,rev,updated_by) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (tenant_id,org) DO UPDATE SET settings=$3,rev=$4,updated_by=$5,updated_at=now() WHERE bug_report_policy.tenant_id=$1 AND bug_report_policy.org=$2")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(json(&policy)?).bind(policy.rev).bind(&ctx.principal().id).execute(&mut *tx).await?;
        crate::delivery::stop_unstarted(&mut tx, ctx, &policy).await?;
        audit(
            &mut tx,
            ctx,
            AuditAction::BugReportPolicyUpdate,
            "policy",
            trace,
            serde_json::json!({"policy_rev": policy.rev}),
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(policy)
    }
    pub async fn create(
        &self,
        ctx: &AuthContext,
        content: ReportContent,
        trace: &str,
    ) -> Result<BugReport, Error> {
        self.member(ctx).await?;
        content.validate()?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        store::lock(&mut tx, ctx).await?;
        let policy = store::policy(&mut tx, ctx).await?;
        check_content_sharing(&policy, &content)?;
        if !policy.enabled {
            return Err(Error::Disabled);
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bug_report WHERE tenant_id=$1 AND org=$2 AND reporter_id=$3 AND created_at > now() - interval '1 day'")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).fetch_one(&mut *tx).await?;
        if count >= 20 {
            return Err(Error::RateLimited);
        }
        let report = BugReport {
            id: Uuid::new_v4(),
            reporter_id: ctx.principal().id.clone(),
            status: Status::Draft,
            rev: 1,
            submission_rev: 0,
            content,
        };
        sqlx::query("INSERT INTO bug_report (tenant_id,org,id,reporter_id,status,rev,snapshot) VALUES ($1,$2,$3,$4,'draft',1,$5)")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(&report.reporter_id).bind(json(&report)?).execute(&mut *tx).await?;
        audit(
            &mut tx,
            ctx,
            AuditAction::BugReportDraftCreate,
            &report.id.to_string(),
            trace,
            serde_json::json!({"rev":1}),
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(report)
    }
    pub async fn get(&self, ctx: &AuthContext, id: Uuid, trace: &str) -> Result<BugReport, Error> {
        self.member(ctx).await?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let mut report = store::report(&mut tx, ctx, id).await?;
        if report.reporter_id != ctx.principal().id {
            if report.status == Status::Draft || report.submission_rev == 0 {
                return Err(Error::NotFound);
            }
            self.admin(ctx).await?;
            // 管理者に見せるのは本人提出 snapshot だけ。未共有の下書き本文は返さない。
            let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND report_id=$3 AND submission_rev=$4")
                .bind(ctx.tenant_id()).bind(ctx.org()).bind(id).bind(report.submission_rev).fetch_one(&mut *tx).await?;
            report.content = serde_json::from_value(payload).map_err(|_| Error::Invalid)?;
        }
        audit(
            &mut tx,
            ctx,
            AuditAction::BugReportRead,
            &id.to_string(),
            trace,
            serde_json::json!({}),
            Chain::No,
        )
        .await?;
        tx.commit().await?;
        Ok(report)
    }
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        expected_rev: i64,
        content: ReportContent,
        trace: &str,
    ) -> Result<BugReport, Error> {
        self.member(ctx).await?;
        content.validate()?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        store::lock(&mut tx, ctx).await?;
        let mut report = store::report(&mut tx, ctx, id).await?;
        owner(ctx, &report)?;
        if report.rev != expected_rev || report.status != Status::Draft {
            return Err(Error::Conflict);
        }
        check_content_sharing(&store::policy(&mut tx, ctx).await?, &content)?;
        report.content = content;
        save(&mut tx, ctx, &mut report).await?;
        audit(
            &mut tx,
            ctx,
            AuditAction::BugReportDraftUpdate,
            &id.to_string(),
            trace,
            serde_json::json!({"rev":report.rev}),
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(report)
    }
    /// 冪等キーは操作・本人・tenant/org ごと。異なる内容への再利用は Conflict (HTTP 409)。
    pub async fn command(
        &self,
        ctx: &AuthContext,
        request: CommandRequest,
        trace: &str,
    ) -> Result<BugReport, Error> {
        self.member(ctx).await?;
        if request.key.is_empty() || request.key.len() > 128 {
            return Err(Error::Invalid);
        }
        let command_value = json(&request.command)?;
        let operation = command_value
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or(Error::Invalid)?;
        let hash = hash(&serde_json::json!([
            request.id,
            request.expected_rev,
            command_value
        ]));
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        store::lock(&mut tx, ctx).await?;
        let mut report = store::report(&mut tx, ctx, request.id).await?;
        // replay 時にも現在の権限を再確認する。
        if matches!(
            request.command,
            Command::Approve | Command::Reject { .. } | Command::Retry
        ) {
            self.admin(ctx).await?;
        } else {
            owner(ctx, &report)?;
        }
        let replay: Option<(String, serde_json::Value)> = sqlx::query_as("SELECT request_hash,result FROM bug_report_command WHERE tenant_id=$1 AND org=$2 AND actor_id=$3 AND operation=$4 AND idempotency_key=$5")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).bind(operation).bind(&request.key).fetch_optional(&mut *tx).await?;
        if let Some((previous_hash, _)) = replay {
            if previous_hash != hash {
                return Err(Error::Conflict);
            }
            // 古い snapshot 本文を replay で返さない（管理者・再下書きも考慮）。
            tx.commit().await?;
            return self.get(ctx, request.id, trace).await;
        }
        if report.rev != request.expected_rev {
            return Err(Error::Conflict);
        }
        let policy = store::policy(&mut tx, ctx).await?;
        let old = report.status;
        let (next, action) = match &request.command {
            Command::Submit {
                policy_rev,
                consent,
                notice_version,
            } => {
                if old != Status::Draft {
                    return Err(Error::Conflict);
                }
                if !policy.enabled {
                    return Err(Error::Disabled);
                }
                if *policy_rev != policy.rev {
                    return Err(Error::Conflict);
                }
                if notice_version != "bug-report-v1" {
                    return Err(Error::Invalid);
                }
                check_submit_limit(&mut tx, ctx).await?;
                let payload = if policy.allow_content_sharing && *consent {
                    report.content.clone()
                } else {
                    report.content.metadata_only()
                };
                report.submission_rev += 1;
                sqlx::query("INSERT INTO bug_report_submission (tenant_id,org,report_id,submission_rev,payload,payload_hash,sink_profile_id,sink_config_version,policy_rev,consent_actor,content_consent,notice_version,principal) VALUES ($1,$2,$3,$4,$5,$6,$11,$12,$7,$8,$9,$10,$13)")
                    .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(report.submission_rev).bind(json(&payload)?).bind(hash_content(&payload)?)
                    .bind(policy.rev).bind(&ctx.principal().id).bind(policy.allow_content_sharing && *consent).bind(notice_version).bind(if self.destination_version.is_some() { "github" } else { "disabled" }).bind(self.destination_version.unwrap_or(1)).bind(json(ctx.principal())?).execute(&mut *tx).await?;
                // 宛先と版を本人提出時に固定する。
                (
                    policy.submitted_status(self.destination_version.is_some()),
                    AuditAction::BugReportSubmit,
                )
            }
            Command::Approve => {
                if old != Status::PendingApproval {
                    return Err(Error::Conflict);
                }
                (
                    approve_submission(&mut tx, ctx, &report, self.destination_version.is_some())
                        .await?,
                    AuditAction::BugReportApprove,
                )
            }
            Command::Reject { reason } => {
                if reason.trim().is_empty() || reason.chars().count() > 2000 {
                    return Err(Error::Invalid);
                }
                if old != Status::PendingApproval {
                    return Err(Error::Conflict);
                }
                sqlx::query("UPDATE bug_report_submission SET rejected_by=$5,rejected_at=now(),rejection_reason=$6 WHERE tenant_id=$1 AND org=$2 AND report_id=$3 AND submission_rev=$4")
                    .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(report.submission_rev).bind(&ctx.principal().id).bind(reason).execute(&mut *tx).await?;
                (Status::Rejected, AuditAction::BugReportReject)
            }
            Command::Retry => {
                retry_delivery(
                    &mut tx,
                    ctx,
                    &report,
                    &policy,
                    self.destination_version.is_some(),
                )
                .await?;
                (Status::Queued, AuditAction::BugReportDispatch)
            }
            Command::Cancel => (Status::Cancelled, AuditAction::BugReportCancel),
            Command::Reopen => (Status::Draft, AuditAction::BugReportReopen),
        };
        if !old.permits(next) {
            return Err(Error::Conflict);
        }
        report.status = next;
        if next == Status::Queued {
            sqlx::query("INSERT INTO bug_report_delivery (tenant_id,org,report_id,submission_rev,id) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (tenant_id,org,report_id,submission_rev) DO NOTHING")
                .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(report.submission_rev).bind(Uuid::new_v4()).execute(&mut *tx).await?;
        }
        save(&mut tx, ctx, &mut report).await?;
        // command 台帳に本文を複製しない。
        sqlx::query("INSERT INTO bug_report_command (tenant_id,org,actor_id,operation,idempotency_key,request_hash,report_id,result) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).bind(operation).bind(request.key).bind(hash).bind(report.id).bind(serde_json::json!({"id":report.id,"rev":report.rev})).execute(&mut *tx).await?;
        let submission_metadata: Option<serde_json::Value> = sqlx::query_scalar("SELECT jsonb_build_object('payload_hash',payload_hash,'policy_rev',policy_rev,'sink_profile_id',sink_profile_id,'sink_config_version',sink_config_version,'notice_version',notice_version,'content_consent',content_consent) FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND report_id=$3 AND submission_rev=$4")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(report.submission_rev).fetch_optional(&mut *tx).await?;
        audit(&mut tx, ctx, action, &report.id.to_string(), trace, serde_json::json!({"old":old,"new":next,"rev":report.rev,"submission_rev":report.submission_rev,"submission":submission_metadata}), Chain::Yes).await?;
        // commit 後の追加参照失敗で、変更が失敗したと誤報しない。
        // admin の command 応答も提出本文だけに制限する。
        if report.reporter_id != ctx.principal().id {
            let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND report_id=$3 AND submission_rev=$4")
                .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(report.submission_rev).fetch_one(&mut *tx).await?;
            report.content = serde_json::from_value(payload).map_err(|_| Error::Invalid)?;
        }
        tx.commit().await?;
        Ok(report)
    }
}
#[derive(Debug)]
pub struct CommandRequest {
    pub id: Uuid,
    pub expected_rev: i64,
    pub key: String,
    pub command: Command,
}
fn owner(ctx: &AuthContext, report: &BugReport) -> Result<(), Error> {
    if report.reporter_id != ctx.principal().id {
        return Err(Error::NotFound);
    }
    Ok(())
}
fn hash(value: &serde_json::Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn hash_content(content: &ReportContent) -> Result<String, Error> {
    Ok(hash(&json(content)?))
}
pub(crate) async fn save(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    report: &mut BugReport,
) -> Result<(), Error> {
    let old_rev = report.rev;
    report.rev += 1;
    let count = sqlx::query("UPDATE bug_report SET status=$4,rev=$5,snapshot=$6,updated_at=now() WHERE tenant_id=$1 AND org=$2 AND id=$3 AND rev=$7")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(json(&report.status)?.as_str().ok_or(Error::Invalid)?).bind(report.rev).bind(json(report)?).bind(old_rev).execute(conn).await?.rows_affected();
    if count != 1 {
        return Err(Error::Conflict);
    }
    Ok(())
}
async fn check_submit_limit(conn: &mut PgConnection, ctx: &AuthContext) -> Result<(), Error> {
    let (hour, day): (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE consent_at > now() - interval '1 hour'),count(*) FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND consent_actor=$3 AND consent_at > now() - interval '1 day'")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).fetch_one(conn).await?;
    if hour >= 5 || day >= 20 {
        return Err(Error::RateLimited);
    }
    Ok(())
}
pub(crate) async fn audit(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    action: AuditAction,
    id: &str,
    trace: &str,
    metadata: serde_json::Value,
    chain: Chain,
) -> Result<(), Error> {
    record_on(
        conn,
        ctx,
        AuditEntry {
            action,
            object_type: "bug_report",
            object_id: id,
            decision: Decision::Allow,
            trace_id: Some(trace),
            metadata,
        },
        chain,
    )
    .await?;
    Ok(())
}

async fn retry_delivery(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    report: &BugReport,
    policy: &BugReportPolicy,
    destination_available: bool,
) -> Result<(), Error> {
    if report.status != Status::Failed || !policy.enabled || !destination_available {
        return Err(Error::Conflict);
    }
    let changed = sqlx::query("UPDATE bug_report_delivery SET outcome='queued' WHERE tenant_id=$1 AND org=$2 AND report_id=$3 AND submission_rev=$4 AND outcome='failed' AND error_code IN ('connect','rate_limit','credential_unavailable') AND attempt_count<5 AND created_at>now()-interval '24 hours'")
                    .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(report.submission_rev).execute(conn).await?.rows_affected();
    if changed != 1 {
        return Err(Error::Conflict);
    }
    Ok(())
}

async fn approve_submission(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    report: &BugReport,
    destination_available: bool,
) -> Result<Status, Error> {
    sqlx::query("UPDATE bug_report_submission SET approved_by=$5,approved_at=now() WHERE tenant_id=$1 AND org=$2 AND report_id=$3 AND submission_rev=$4")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(report.submission_rev).bind(&ctx.principal().id).execute(&mut *conn).await?;
    let has_principal: bool = sqlx::query_scalar("SELECT principal IS NOT NULL AND principal <> 'null'::jsonb FROM bug_report_submission WHERE tenant_id=$1 AND org=$2 AND report_id=$3 AND submission_rev=$4")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(report.id).bind(report.submission_rev).fetch_one(conn).await?;
    // 承認は snapshot を変更しない。旧提出は本人の再同意を要求する。
    Ok(if destination_available && has_principal {
        Status::Queued
    } else {
        Status::Held
    })
}

fn check_content_sharing(policy: &BugReportPolicy, content: &ReportContent) -> Result<(), Error> {
    if !policy.allow_content_sharing && *content != content.metadata_only() {
        return Err(Error::Forbidden);
    }
    Ok(())
}
