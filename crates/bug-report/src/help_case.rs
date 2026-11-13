//! AI 下書きの証拠は DB に限る。引用・LLM 引数・再生したイベントを証拠にしない。
use crate::{store, BugReport, BugReportService, Error, ReportContent};
use authz::{AuthContext, Consistency, Relation};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use storage::{
    audit::{AuditAction, Chain},
    db_tenant::begin_tenant_tx,
};
use uuid::Uuid;

/// ワーカーが現在実行中の run に束縛する。LLM の入力には含めない。
#[derive(Debug, Clone)]
pub struct HelpRun {
    pub run_id: Uuid,
    pub fencing: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HelpReceipt {
    pub hit_count: u32,
    pub corpus_versions: Vec<String>,
    pub chunk_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HelpResolution {
    Unresolved,
    Resolved,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HelpCase {
    pub id: Uuid,
    pub guidance_message_id: Uuid,
    pub hit_count: u32,
    pub resolution: String,
    pub report_intent: bool,
    pub report_id: Option<Uuid>,
}
impl BugReportService {
    async fn thread_viewer(&self, ctx: &AuthContext, thread: Uuid) -> Result<(), Error> {
        self.member(ctx).await?;
        if !self
            .authz
            .check(
                &ctx.subject(),
                Relation::Viewer,
                &ctx.ns().thread(&thread.to_string()),
                Consistency::HigherConsistency,
            )
            .await?
        {
            return Err(Error::NotFound);
        }
        Ok(())
    }
    /// 実検索の成功イベント専用。検索失敗はこのメソッドを呼ばない。
    pub async fn record_help_receipt(
        &self,
        ctx: &AuthContext,
        run: &HelpRun,
        call: &str,
        receipt: HelpReceipt,
    ) -> Result<Uuid, Error> {
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let (thread, message) = self.help_run(&mut tx, ctx, run).await?;
        self.thread_viewer(ctx, thread).await?;
        let id: Uuid = sqlx::query_scalar("INSERT INTO help_resolution_case (tenant_id,org,id,reporter_id,thread_id,guidance_message_id,run_id,help_tool_call_id,receipt,fencing_token) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT (tenant_id,org,run_id,help_tool_call_id) DO UPDATE SET receipt=EXCLUDED.receipt,fencing_token=EXCLUDED.fencing_token WHERE help_resolution_case.tenant_id=$1 AND help_resolution_case.org=$2 RETURNING id")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(Uuid::new_v4()).bind(&ctx.principal().id)
            .bind(thread).bind(message).bind(run.run_id).bind(call).bind(store::json(&receipt)?).bind(run.fencing)
            .fetch_one(&mut *tx).await?;
        crate::service::audit(
            &mut tx,
            ctx,
            AuditAction::HelpSearchReceipt,
            &id.to_string(),
            "help-search",
            serde_json::json!({"run_id":run.run_id}),
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }
    async fn help_run(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        run: &HelpRun,
    ) -> Result<(Uuid, Uuid), Error> {
        sqlx::query_as("SELECT thread_id,message_id FROM generation_run r WHERE r.tenant_id=$1 AND r.org=$2 AND r.actor=$3 AND r.run_id=$4 AND r.fencing_token=$5 AND r.status='running' AND NOT r.cancel_requested AND r.lease_until>now() AND NOT r.autonomous AND r.mini_app_id IS NULL AND r.skill_pins='[]'::jsonb AND NOT EXISTS (SELECT 1 FROM generation_event e WHERE e.run_id=r.run_id AND e.type='skill_invoked') FOR SHARE OF r")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).bind(run.run_id).bind(run.fencing)
            .fetch_optional(conn).await?.ok_or(Error::NotFound)
    }
    /// 本人だけが現在の枝の case を読む。guidance 確定前・失敗 run は拒否する。
    pub async fn help_case(
        &self,
        ctx: &AuthContext,
        thread: Uuid,
        id: Uuid,
    ) -> Result<HelpCase, Error> {
        self.thread_viewer(ctx, thread).await?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let case = load_case(&mut tx, ctx, thread, id).await?;
        tx.commit().await?;
        Ok(case)
    }
    pub async fn resolve_help_case(
        &self,
        ctx: &AuthContext,
        thread: Uuid,
        id: Uuid,
        resolution: HelpResolution,
    ) -> Result<HelpCase, Error> {
        self.thread_viewer(ctx, thread).await?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let case = load_case(&mut tx, ctx, thread, id).await?;
        if case.report_id.is_some() {
            return Err(Error::Conflict);
        }
        let resolution = match resolution {
            HelpResolution::Unresolved => "unresolved",
            HelpResolution::Resolved => "resolved",
        };
        sqlx::query("UPDATE help_resolution_case SET resolution=$5, confirmed_at=now(), report_intent_at=NULL WHERE tenant_id=$1 AND org=$2 AND reporter_id=$3 AND id=$4")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).bind(id).bind(resolution).execute(&mut *tx).await?;
        crate::service::audit(
            &mut tx,
            ctx,
            AuditAction::HelpCaseResolution,
            &id.to_string(),
            "help-resolution",
            serde_json::json!({"resolution":resolution}),
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        self.help_case(ctx, thread, id).await
    }
    pub async fn help_report_intent(
        &self,
        ctx: &AuthContext,
        thread: Uuid,
        id: Uuid,
    ) -> Result<HelpCase, Error> {
        self.thread_viewer(ctx, thread).await?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let case = load_case(&mut tx, ctx, thread, id).await?;
        eligible(&case)?;
        let current: bool = sqlx::query_scalar("SELECT COALESCE(active_message_id,(SELECT id FROM message WHERE tenant_id=$1 AND org=$2 AND thread_id=$3 ORDER BY created_at DESC,id DESC LIMIT 1))=$4 FROM thread WHERE tenant_id=$1 AND org=$2 AND id=$3")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(thread).bind(case.guidance_message_id).fetch_one(&mut *tx).await?;
        if !current {
            return Err(Error::Conflict);
        }

        sqlx::query("UPDATE help_resolution_case SET report_intent_at=COALESCE(report_intent_at,now()) WHERE tenant_id=$1 AND org=$2 AND reporter_id=$3 AND id=$4")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).bind(id).execute(&mut *tx).await?;
        crate::service::audit(
            &mut tx,
            ctx,
            AuditAction::HelpReportIntent,
            &id.to_string(),
            "help-intent",
            serde_json::json!({}),
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        self.help_case(ctx, thread, id).await
    }
    /// サーバが次の対話 run に提示する case を選ぶ。モデルには選択させない。
    pub async fn available_help_case(
        &self,
        ctx: &AuthContext,
        run: &HelpRun,
    ) -> Result<Uuid, Error> {
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        let (thread, message) = self.help_run(&mut tx, ctx, run).await?;
        let id = sqlx::query_scalar("SELECT c.id FROM help_resolution_case c JOIN message u ON u.tenant_id=$1 AND u.org=$2 AND u.thread_id=$3 AND u.parent_id=c.guidance_message_id JOIN message a ON a.tenant_id=$1 AND a.org=$2 AND a.thread_id=$3 AND a.parent_id=u.id AND a.id=$4 WHERE c.tenant_id=$1 AND c.org=$2 AND c.thread_id=$3 AND c.reporter_id=$5 AND c.report_intent_at IS NOT NULL AND c.expires_at>now() AND c.resolution<>'resolved' ORDER BY c.report_intent_at DESC,c.id LIMIT 1")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(thread).bind(message).bind(&ctx.principal().id).fetch_optional(&mut *tx).await?.ok_or(Error::NotFound)?;
        tx.commit().await?;
        let case = self.help_case(ctx, thread, id).await?;
        eligible(&case)?;
        Ok(id)
    }
    pub async fn create_ai_draft(
        &self,
        ctx: &AuthContext,
        run: &HelpRun,
        id: Uuid,
        content: ReportContent,
        trace: &str,
    ) -> Result<BugReport, Error> {
        content.validate()?;
        let mut tx = begin_tenant_tx(&self.store.pool, ctx.tenant()).await?;
        store::lock(&mut tx, ctx).await?;
        // run はここでは参照だけに留め、削除・finalize と同じ thread → run 順にロックする。
        let thread: Uuid = sqlx::query_scalar(
            "SELECT thread_id FROM generation_run WHERE tenant_id=$1 AND org=$2 AND run_id=$3",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(run.run_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound)?;
        self.thread_viewer(ctx, thread).await?;
        let case = load_case(&mut tx, ctx, thread, id).await?;
        // thread のロック待ち中に取り消し・lease/fencing が変わり得るため、取得後に再検証する。
        let (run_thread, message) = self.help_run(&mut tx, ctx, run).await?;
        if run_thread != thread {
            return Err(Error::NotFound);
        }
        eligible(&case)?;
        if !case.report_intent {
            return Err(Error::Conflict);
        }
        // 確認後、案内の直後の本人発話への回答でのみ使う。話題を後の turn へ持ち越さない。
        let valid: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM message a JOIN message u ON u.id=a.parent_id AND u.tenant_id=$1 AND u.org=$2 AND u.thread_id=$3 JOIN generation_run r ON r.message_id=a.id AND r.tenant_id=$1 AND r.org=$2 JOIN help_resolution_case c ON c.tenant_id=$1 AND c.org=$2 AND c.id=$5 WHERE a.tenant_id=$1 AND a.org=$2 AND a.thread_id=$3 AND a.id=$4 AND u.role='user' AND u.parent_id=c.guidance_message_id AND r.created_at>=c.report_intent_at)")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(thread).bind(message).bind(id).fetch_one(&mut *tx).await?;
        if !valid {
            return Err(Error::Conflict);
        }
        if let Some(report) = case.report_id {
            let same: bool = sqlx::query_scalar("SELECT draft_run_id=$5 FROM help_resolution_case WHERE tenant_id=$1 AND org=$2 AND reporter_id=$3 AND id=$4")
                .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).bind(id).bind(run.run_id).fetch_one(&mut *tx).await?;
            if !same {
                return Err(Error::Conflict);
            }
            return store::report(&mut tx, ctx, report).await;
        }
        let report = self.create_on(&mut tx, ctx, content, trace).await?;
        sqlx::query("UPDATE help_resolution_case SET report_id=$5,draft_run_id=$6 WHERE tenant_id=$1 AND org=$2 AND reporter_id=$3 AND id=$4")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(&ctx.principal().id).bind(id).bind(report.id).bind(run.run_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(report)
    }
}
fn eligible(case: &HelpCase) -> Result<(), Error> {
    if case.resolution == "resolved" || (case.hit_count > 0 && case.resolution != "unresolved") {
        return Err(Error::Conflict);
    }
    Ok(())
}
async fn load_case(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    thread: Uuid,
    id: Uuid,
) -> Result<HelpCase, Error> {
    // thread 行ロックで現在枝の検査と本人操作を直列化する。本文は取得しない。
    let leaf: Option<Uuid> = sqlx::query_scalar("SELECT COALESCE(active_message_id,(SELECT id FROM message WHERE tenant_id=$1 AND org=$2 AND thread_id=$3 ORDER BY created_at DESC,id DESC LIMIT 1)) FROM thread WHERE tenant_id=$1 AND org=$2 AND id=$3 AND deleted_at IS NULL FOR UPDATE")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(thread).fetch_optional(&mut *conn).await?.flatten();
    let (guidance, receipt, resolution, intent, report): (Uuid,serde_json::Value,String,bool,Option<Uuid>) = sqlx::query_as("WITH RECURSIVE path AS (SELECT id,parent_id,0 depth FROM message WHERE tenant_id=$1 AND org=$2 AND thread_id=$3 AND id=$6 UNION ALL SELECT m.id,m.parent_id,p.depth+1 FROM message m JOIN path p ON m.id=p.parent_id WHERE m.tenant_id=$1 AND m.org=$2 AND m.thread_id=$3 AND p.depth<10000) SELECT c.guidance_message_id,c.receipt,c.resolution,c.report_intent_at IS NOT NULL,c.report_id FROM help_resolution_case c JOIN generation_run r ON r.run_id=c.run_id AND r.tenant_id=$1 AND r.org=$2 WHERE c.tenant_id=$1 AND c.org=$2 AND c.thread_id=$3 AND c.reporter_id=$4 AND c.id=$5 AND c.expires_at>now() AND r.status='done' AND r.fencing_token=c.fencing_token AND EXISTS (SELECT 1 FROM message g, jsonb_array_elements(g.content) b WHERE g.tenant_id=$1 AND g.org=$2 AND g.thread_id=$3 AND g.id=c.guidance_message_id AND g.role='assistant' AND b->>'type'='text' AND length(trim(b->>'text'))>0) AND EXISTS (SELECT 1 FROM path WHERE id=c.guidance_message_id) FOR UPDATE OF c")
        .bind(ctx.tenant_id()).bind(ctx.org()).bind(thread).bind(&ctx.principal().id).bind(id).bind(leaf)
        .fetch_optional(conn).await?.ok_or(Error::NotFound)?;
    let receipt: HelpReceipt = serde_json::from_value(receipt).map_err(|_| Error::Invalid)?;
    Ok(HelpCase {
        id,
        guidance_message_id: guidance,
        hit_count: receipt.hit_count,
        resolution,
        report_intent: intent,
        report_id: report,
    })
}
