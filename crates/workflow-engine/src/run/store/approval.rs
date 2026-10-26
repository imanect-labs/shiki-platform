//! 承認待ち（human.approval）の停止・起床・期限・取消と台帳の確定（GAP-08・D-24）。
//!
//! - **停止**（checkpoint の Tx）: step を `waiting_event` にし、台帳・担当者・購読（`data.transition`・その行・
//!   `suspend_rev`）を同じ Tx で書く。再実行・再開（at-least-once）でも同じ承認 id を上書きして待ち直す。
//! - **起床**（relay・受付の突合・行の削除）: その行の遷移で `rev > suspend_rev` のものだけを、遷移先の状態に
//!   応じた出口へ写す。判断の外の状態・行が消えたら取り下げ。step の terminal 化と台帳の確定を同じ Tx で行う。
//! - **期限**: `expire_due_waits` と同じ Tx で台帳を `expired` にする。
//! - **取消**: run の失敗・取消（`cancel_remaining_steps`・ドレイン）と同じ Tx で保留中の台帳を `cancelled` にする。
//!
//! 判断に添えたコメントは、遷移の後の rev・遷移した人・判断の種別が一致するものだけを採る
//! （失敗した判断のコメント・別の人の遷移を拾わない）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::types::Json;
use sqlx::PgPool;
use uuid::Uuid;

use super::super::graph::RunGraph;
use super::super::OnTimeout;
use super::wait::{graph_from_snapshot, lock_waiting, settle_woken, EventWaitSubscriber};
use super::{map_db, ClaimedStep, RunStore, RunStoreError};
use crate::approval::{ApprovalChoiceSpec, ApprovalResolution, ApprovalSuspend};
use crate::vocab::EventSource;

/// 購読の spec に載せる承認の材料（起床で遷移先を出口へ写す・期限切れの扱い）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalWait {
    pub approval_id: Uuid,
    pub record_id: Uuid,
    pub choices: Vec<ApprovalChoiceSpec>,
    pub on_timeout: OnTimeout,
}

impl ApprovalWait {
    /// 購読の spec から（承認でない購読は `None`）。
    #[must_use]
    pub fn from_spec(spec: &Value) -> Option<Self> {
        serde_json::from_value(spec.get("approval")?.clone()).ok()
    }
}

/// 保留中の承認 1 件（受付の突合・行の削除で、実行主体の見え方で読み直す材料）。
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub subscriber: EventWaitSubscriber,
    pub workflow_id: Uuid,
    pub table_id: Uuid,
    pub from_state: String,
    pub suspend_rev: i64,
    pub assignees: i64,
    pub wait: ApprovalWait,
}

/// 確定した承認（監査の材料）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedApproval {
    pub tenant_id: String,
    pub org: String,
    pub approval_id: Uuid,
    pub run_id: Uuid,
    pub status: String,
}

/// 停止（checkpoint の Tx 内）: step を待機にし、台帳・担当者・購読を書く。
pub(super) async fn suspend(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    claimed: &ClaimedStep,
    a: &ApprovalSuspend,
) -> Result<(), RunStoreError> {
    let tenant = &claimed.tenant_id;
    sqlx::query(
        "UPDATE step_execution SET status = 'waiting_event', \
         lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant)
    .bind(claimed.run_id)
    .bind(&claimed.step_path)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    let wait = ApprovalWait {
        approval_id: a.approval_id,
        record_id: a.record_id,
        choices: a.choices.clone(),
        on_timeout: a.on_timeout,
    };
    let spec = json!({
        "scope": { "table": a.table_id.to_string(), "record": a.record_id.to_string() },
        "filter": null,
        "on_timeout": match a.on_timeout { OnTimeout::Fail => "fail", OnTimeout::Continue => "continue" },
        "suspend_rev": a.suspend_rev,
        "approval": wait,
    });
    sqlx::query(
        "INSERT INTO wait_subscription \
         (tenant_id, run_id, step_path, kind, wake_at, timeout_at, source, spec, fired) \
         VALUES ($1, $2, $3, 'event', NULL, $4, $5, $6, false) \
         ON CONFLICT (tenant_id, run_id, step_path) DO UPDATE SET \
         kind = 'event', wake_at = NULL, timeout_at = $4, source = $5, spec = $6, fired = false",
    )
    .bind(tenant)
    .bind(claimed.run_id)
    .bind(&claimed.step_path)
    .bind(a.timeout_at)
    .bind(EventSource::DataTransition.as_str())
    .bind(Json(&spec))
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    let decisions = serde_json::to_value(&a.choices)
        .map_err(|e| RunStoreError::Internal(format!("approval decisions: {e}")))?;
    // 再実行・再開では同じ承認 id を保留中へ戻して待ち直す（判断の結果と突合の印を消す）。
    sqlx::query(
        "INSERT INTO workflow_approval \
         (tenant_id, approval_id, org, workflow_id, run_id, step_path, node_id, table_id, record_id, \
          title, instructions, show_fields, decisions, from_state, suspend_rev, deadline) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
         ON CONFLICT (tenant_id, approval_id) DO UPDATE SET \
           table_id = $8, record_id = $9, title = $10, instructions = $11, show_fields = $12, \
           decisions = $13, from_state = $14, suspend_rev = $15, deadline = $16, status = 'pending', \
           decided_by = NULL, decided_at = NULL, decided_to = NULL, decided_rev = NULL, comment = NULL, \
           reconciled_at = NULL, updated_at = now()",
    )
    .bind(tenant)
    .bind(a.approval_id)
    .bind(&claimed.org)
    .bind(claimed.workflow_id)
    .bind(claimed.run_id)
    .bind(&claimed.step_path)
    .bind(&claimed.node_id)
    .bind(a.table_id)
    .bind(a.record_id)
    .bind(&a.title)
    .bind(&a.instructions)
    .bind(&a.show_fields)
    .bind(Json(&decisions))
    .bind(&a.from_state)
    .bind(a.suspend_rev)
    .bind(a.timeout_at)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    sqlx::query("DELETE FROM workflow_approval_assignee WHERE tenant_id = $1 AND approval_id = $2")
        .bind(tenant)
        .bind(a.approval_id)
        .execute(&mut **tx)
        .await
        .map_err(map_db)?;
    let kinds: Vec<&str> = a.assignees.iter().map(|x| x.kind.as_str()).collect();
    let subjects: Vec<&str> = a.assignees.iter().map(|x| x.subject.as_str()).collect();
    sqlx::query(
        "INSERT INTO workflow_approval_assignee (tenant_id, approval_id, kind, subject) \
         SELECT $1, $2, k, s FROM unnest($3::text[], $4::text[]) AS t(k, s) ON CONFLICT DO NOTHING",
    )
    .bind(tenant)
    .bind(a.approval_id)
    .bind(&kinds)
    .bind(&subjects)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// 承認を確定して step を起こす（step の terminal 化と台帳の確定を同じ Tx で）。戻り値 = 起こしたか。
pub(super) async fn wake_approval(
    db: &PgPool,
    tenant_id: &str,
    run_id: Uuid,
    step_path: &str,
    wait: &ApprovalWait,
    res: &ApprovalResolution,
    graph: &RunGraph,
) -> Result<bool, RunStoreError> {
    let mut tx = db.begin().await.map_err(map_db)?;
    if !lock_waiting(&mut tx, tenant_id, run_id, step_path).await? {
        tx.rollback().await.map_err(map_db)?;
        return Ok(false);
    }
    // 判断に添えたコメント（遷移の後の rev・遷移した人・判断の種別が一致するものだけ）。
    let comment: Option<String> = match (res.kind(), res.rev, res.decided_by.as_deref()) {
        (Some(kind), Some(rev), Some(by)) => sqlx::query_scalar(
            "SELECT body FROM workflow_approval_comment \
             WHERE tenant_id = $1 AND approval_id = $2 AND rev = $3 AND author = $4 AND decision = $5",
        )
        .bind(tenant_id)
        .bind(wait.approval_id)
        .bind(rev)
        .bind(by)
        .bind(kind.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?,
        _ => None,
    };
    // 遷移による確定は遷移の時刻、取り下げ・期限切れは確定した時刻。
    let decided_at: DateTime<Utc> = res.decided_at.unwrap_or_else(Utc::now);
    sqlx::query(
        "UPDATE workflow_approval SET status = $3, decided_by = $4, decided_at = $5, decided_to = $6, \
         decided_rev = $7, comment = $8, updated_at = now() \
         WHERE tenant_id = $1 AND approval_id = $2 AND status = 'pending'",
    )
    .bind(tenant_id)
    .bind(wait.approval_id)
    .bind(res.status)
    .bind(&res.decided_by)
    .bind(decided_at)
    .bind(&res.to)
    .bind(res.rev)
    .bind(&comment)
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    let output = json!({
        "approval_id": wait.approval_id,
        "decision": res.decision,
        "to": res.to,
        "decided_by": res.decided_by,
        "decided_at": res.decided_at.map(|t| t.to_rfc3339()),
        "comment": comment,
        "record_id": wait.record_id,
        "rev": res.rev,
    });
    settle_woken(
        &mut tx, tenant_id, run_id, step_path, res.port, output, graph,
    )
    .await?;
    tx.commit().await.map_err(map_db)?;
    Ok(true)
}

/// run の失敗・取消で、その run の保留中の承認を取り消す（`cancel_remaining_steps`・ドレインと同じ Tx）。
pub(super) async fn cancel_pending(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
) -> Result<(), RunStoreError> {
    sqlx::query(
        "UPDATE workflow_approval SET status = 'cancelled', updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND status = 'pending'",
    )
    .bind(tenant_id)
    .bind(run_id)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct PendingRow {
    #[sqlx(flatten)]
    subscriber: EventWaitSubscriber,
    workflow_id: Uuid,
    table_id: Uuid,
    from_state: String,
    suspend_rev: i64,
    assignees: i64,
    spec: Json<Value>,
}

impl PendingRow {
    fn into_pending(self) -> Option<PendingApproval> {
        Some(PendingApproval {
            wait: ApprovalWait::from_spec(&self.spec.0)?,
            subscriber: self.subscriber,
            workflow_id: self.workflow_id,
            table_id: self.table_id,
            from_state: self.from_state,
            suspend_rev: self.suspend_rev,
            assignees: self.assignees,
        })
    }
}

/// 保留中で、run が実行中・step が待機中の承認（別名 `a` / `w` / `r` / `s`）。
const PENDING_FROM: &str = "SELECT a.tenant_id, r.org, a.run_id, a.step_path, r.principal, r.principal_kind, \
            a.workflow_id, a.table_id, a.from_state, a.suspend_rev, \
            (SELECT count(*) FROM workflow_approval_assignee x \
              WHERE x.tenant_id = a.tenant_id AND x.approval_id = a.approval_id) AS assignees, \
            w.spec \
     FROM workflow_approval a \
     JOIN workflow_run r ON r.tenant_id = a.tenant_id AND r.run_id = a.run_id AND r.status = 'running' \
     JOIN step_execution s ON s.tenant_id = a.tenant_id AND s.run_id = a.run_id \
                          AND s.step_path = a.step_path AND s.status = 'waiting_event' \
     JOIN wait_subscription w ON w.tenant_id = a.tenant_id AND w.run_id = a.run_id \
                             AND w.step_path = a.step_path AND NOT w.fired \
     WHERE a.status = 'pending'";

impl RunStore {
    /// 受付の突合がまだの保留中の承認（作成の古い順・購読の commit の後に走る・テナントを問わない）。
    pub async fn approvals_to_reconcile(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingApproval>, RunStoreError> {
        let sql =
            format!("{PENDING_FROM} AND a.reconciled_at IS NULL ORDER BY a.created_at LIMIT $1");
        let rows: Vec<PendingRow> = sqlx::query_as(&sql)
            .bind(limit)
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(PendingRow::into_pending)
            .collect())
    }

    /// 行（テーブル, 行）に束縛した保留中の承認（行の削除で取り下げる材料）。
    pub async fn pending_approvals_on(
        &self,
        tenant_id: &str,
        records: &[(Uuid, Uuid)],
    ) -> Result<Vec<PendingApproval>, RunStoreError> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let tables: Vec<Uuid> = records.iter().map(|(t, _)| *t).collect();
        let rows_ids: Vec<Uuid> = records.iter().map(|(_, r)| *r).collect();
        let sql = format!(
            "{PENDING_FROM} AND a.tenant_id = $1 \
               AND (a.table_id, a.record_id) IN (SELECT * FROM unnest($2::uuid[], $3::uuid[]))"
        );
        let rows: Vec<PendingRow> = sqlx::query_as(&sql)
            .bind(tenant_id)
            .bind(&tables)
            .bind(&rows_ids)
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(PendingRow::into_pending)
            .collect())
    }

    /// 受付の突合を済ませた印を付ける（待ち続ける承認・確定した承認とも）。
    pub async fn mark_approval_reconciled(
        &self,
        tenant_id: &str,
        approval_id: Uuid,
    ) -> Result<(), RunStoreError> {
        sqlx::query(
            "UPDATE workflow_approval SET reconciled_at = now() \
             WHERE tenant_id = $1 AND approval_id = $2 AND reconciled_at IS NULL",
        )
        .bind(tenant_id)
        .bind(approval_id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// 保留中の承認を確定して step を起こす（受付の突合・行の削除・relay の遷移）。戻り値 = 起こしたか。
    pub async fn resolve_approval(
        &self,
        subscriber: &EventWaitSubscriber,
        wait: &ApprovalWait,
        res: &ApprovalResolution,
    ) -> Result<bool, RunStoreError> {
        let snapshot: Option<Json<Value>> = sqlx::query_scalar(
            "SELECT ir_snapshot FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&subscriber.tenant_id)
        .bind(subscriber.run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some(graph) = snapshot.and_then(|s| graph_from_snapshot(s.0)) else {
            return Ok(false);
        };
        wake_approval(
            &self.db,
            &subscriber.tenant_id,
            subscriber.run_id,
            &subscriber.step_path,
            wait,
            res,
            &graph,
        )
        .await
    }

    /// 期限の来た承認を期限切れにする（監査の材料を返す・[`Self::expire_due_waits`] より先に呼ぶ）。
    ///
    /// `expire_due_waits` も承認の期限切れを同じ経路で確定するので、こちらを呼ばなくても承認は期限で
    /// 流れる（こちらは確定したものを返すだけの違い）。
    pub async fn expire_due_approvals(
        &self,
        now: DateTime<Utc>,
        tenant_scope: Option<&str>,
    ) -> Result<Vec<ResolvedApproval>, RunStoreError> {
        type Due = (String, String, Uuid, String, Json<Value>, Json<Value>);
        let rows: Vec<Due> = sqlx::query_as(
            "SELECT w.tenant_id, r.org, w.run_id, w.step_path, w.spec, r.ir_snapshot \
             FROM wait_subscription w \
             JOIN workflow_run r ON r.tenant_id = w.tenant_id AND r.run_id = w.run_id \
             WHERE w.timeout_at <= $1 AND NOT w.fired AND r.status = 'running' \
               AND w.spec ? 'approval' \
               AND (($2::text IS NULL) OR (w.tenant_id = $2)) \
             ORDER BY w.timeout_at LIMIT 256",
        )
        .bind(now)
        .bind(tenant_scope)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut out = Vec::new();
        for (tenant, org, run_id, step_path, spec, ir_json) in rows {
            let (Some(wait), Some(graph)) = (
                ApprovalWait::from_spec(&spec.0),
                graph_from_snapshot(ir_json.0),
            ) else {
                continue;
            };
            let res = ApprovalResolution::expired(wait.on_timeout);
            if wake_approval(&self.db, &tenant, run_id, &step_path, &wait, &res, &graph).await? {
                out.push(ResolvedApproval {
                    tenant_id: tenant,
                    org,
                    approval_id: wait.approval_id,
                    run_id,
                    status: res.status.to_string(),
                });
            }
        }
        Ok(out)
    }
}
