//! 承認待ち（human.approval・GAP-08）の受付の突合・行の削除による取り下げ・監査（relay から呼ぶ）。
//!
//! 判断の遷移そのものは、データ変更の wait(event) と同じ候補（[`super::relay_data`]）として起きる（その行の
//! `data.transition` で `rev > suspend_rev` のもの・実行主体の見え方）。ここは残りの 2 つを受け持つ。
//!
//! - **受付の突合**: 停止の commit より前に起きた遷移は、購読ができる前に配送済みになり得る。保留中で未突合の
//!   承認を、実行主体の見え方で行を読み直して決める。いまの状態が停止時の状態なら待ち続け、判断の行き先なら
//!   その出口へ、それ以外・行が見えない（消えた）なら取り下げる。実行主体の委譲・権限が停止の後に外れていたら、
//!   判断の遷移が二度と見えないので取り下げ、理由（[`ACTOR_UNAVAILABLE`]）を決着の監査に残す（判断の遷移を
//!   受けた relay も同じ扱い）。承認の依頼の監査もここで書く。
//! - **行の削除**: outbox の削除（`data.record.changed` / delete）を受けたら、その行に束縛した保留中の承認を、
//!   実行主体の `data_get` が NotFound であることを確かめて取り下げる（見えなくなっただけでは取り下げない）。
//!
//! 権限の判定（Forbidden / NotFound）は承認の結論に使い、一時的な障害だけを `Err` で返す（次の tick で配り直す）。

use authz::{AuthContext, Principal, PrincipalKind, Relation};
use data::{ChangeEventKind, ChangeEventRef, DataError};
use serde_json::{json, Value};
use storage::audit::{AuditAction, AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;
use workflow_engine::{
    ApprovalResolution, ApprovalWait, EventWaitSubscriber, ExecCtx, ResolvedApproval,
};

use super::auth::auth_ctx;
use super::data_port::DataPort;
use super::relay::RelayDeps;

/// 1 tick で突き合わせる承認の上限。
const RECONCILE_BATCH: i64 = 100;

fn exec_ctx(s: &EventWaitSubscriber) -> ExecCtx {
    ExecCtx {
        tenant_id: s.tenant_id.clone(),
        org: s.org.clone(),
        principal: s.principal.clone(),
        principal_kind: s.principal_kind.clone(),
        trace_id: None,
        run_id: Some(s.run_id),
        step_path: Some(s.step_path.clone()),
    }
}

/// relay の system 主体（監査の actor）。
fn system_ctx(tenant_id: &str, org: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "system:workflow-relay".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.to_string(),
        tenant_id.to_string(),
    )
}

/// 承認の監査（書込の失敗は警告に留める・relay を止めない）。
async fn audit(
    db: &sqlx::PgPool,
    ctx: &AuthContext,
    action: AuditAction,
    approval_id: Uuid,
    metadata: Value,
) {
    let entry = AuditEntry {
        action,
        object_type: "workflow_approval",
        object_id: &approval_id.to_string(),
        decision: Decision::Allow,
        trace_id: None,
        metadata,
    };
    if let Err(e) = AuditRecorder::new(db.clone()).record(ctx, entry).await {
        tracing::warn!(error = %e, %approval_id, "承認の監査に失敗");
    }
}

/// 承認を取り下げた理由（監査の `reason`）: 実行主体（委譲・権限）が行を読めなくなった。待っても判断の遷移が
/// 実行主体に見えないので、受信箱で判断されても run は期限まで待ち、行と run の結論が食い違う。
pub(super) const ACTOR_UNAVAILABLE: &str = "actor_unavailable";

/// 承認が決着したことの監査（起床・取り下げ・期限切れ・system が記録）。
pub(crate) async fn audit_resolved(db: &sqlx::PgPool, r: &ResolvedApproval) {
    audit_resolved_because(db, r, None).await;
}

/// 承認が決着したことの監査（理由つき・[`ACTOR_UNAVAILABLE`]）。
pub(super) async fn audit_resolved_because(
    db: &sqlx::PgPool,
    r: &ResolvedApproval,
    reason: Option<&str>,
) {
    let mut metadata = json!({ "run_id": r.run_id, "status": r.status });
    if let Some(reason) = reason {
        metadata["reason"] = json!(reason);
    }
    audit(
        db,
        &system_ctx(&r.tenant_id, &r.org),
        AuditAction::WorkflowApprovalResolve,
        r.approval_id,
        metadata,
    )
    .await;
}

/// 保留中の承認を確定し、起こしたら決着の監査を書く。
pub(super) async fn resolve(
    deps: &RelayDeps<'_>,
    subscriber: &EventWaitSubscriber,
    wait: &ApprovalWait,
    res: &ApprovalResolution,
    reason: Option<&str>,
) -> Result<(), anyhow::Error> {
    if deps.runs.resolve_approval(subscriber, wait, res).await? {
        audit_resolved_because(
            deps.db,
            &ResolvedApproval {
                tenant_id: subscriber.tenant_id.clone(),
                org: subscriber.org.clone(),
                approval_id: wait.approval_id,
                run_id: subscriber.run_id,
                status: res.status.to_string(),
            },
            reason,
        )
        .await;
    }
    Ok(())
}

/// 受付の突合（上のモジュールの説明）。
pub(super) async fn reconcile_approvals(deps: &RelayDeps<'_>) -> Result<(), anyhow::Error> {
    let port = DataPort {
        data: deps.data.as_ref(),
        fsms: deps.fsms.as_ref(),
        db: deps.db,
    };
    for p in deps.runs.approvals_to_reconcile(RECONCILE_BATCH).await? {
        let ec = exec_ctx(&p.subscriber);
        let actor = match port.actor(&ec, p.table_id, Relation::Viewer).await {
            Ok(a) => Some(a),
            // 委譲・権限が無い（停止の後に外れた）: 実行主体には判断の遷移が見えず、待っても決まらない。
            // 取り下げて理由を監査に残す（受信箱で判断されても run が期限まで待つ食い違いを作らない）。
            Err(e) if !e.retryable => None,
            Err(e) => {
                tracing::warn!(code = %e.code, error = %e.message, "承認の受付の突合で行を読めません（次の tick で再試行）");
                continue;
            }
        };
        let withdrawn = ApprovalResolution::withdrawn();
        if let Some(a) = actor {
            let target = a
                .fsms
                .approval_target(&a.ctx, p.table_id, p.wait.record_id, None)
                .await;
            match target {
                Err(DataError::NotFound) => {
                    resolve(deps, &p.subscriber, &p.wait, &withdrawn, None).await?;
                }
                Err(e) if e.is_transient() => {
                    tracing::warn!(error = %e, "承認の受付の突合に失敗（次の tick で再試行）");
                    continue;
                }
                // 権限が無い・状態の列が読めない・その他の確定的な誤り: 待ち続ける。
                Err(_) => {}
                Ok(v) if v.current.as_deref() == Some(p.from_state.as_str()) => {}
                Ok(v) => {
                    // 停止の commit より前に動いた。判断の行き先ならその出口、それ以外は取り下げ。
                    let decided_by = a
                        .data
                        .change_event_view(
                            &a.ctx,
                            &ChangeEventRef {
                                table_id: p.table_id,
                                record_id: p.wait.record_id,
                                rev: v.rev,
                                kind: ChangeEventKind::Transition,
                            },
                            None,
                        )
                        .await
                        .ok()
                        .flatten()
                        .filter(|c| c.to.as_deref() == v.current.as_deref())
                        .map(|c| (c.actor, c.occurred_at));
                    let res = ApprovalResolution::transition(
                        &p.wait.choices,
                        v.current.clone(),
                        decided_by.as_ref().map(|(by, _)| by.clone()),
                        decided_by.map(|(_, at)| at),
                        Some(v.rev),
                    );
                    resolve(deps, &p.subscriber, &p.wait, &res, None).await?;
                }
            }
        } else {
            let reason = Some(ACTOR_UNAVAILABLE);
            resolve(deps, &p.subscriber, &p.wait, &withdrawn, reason).await?;
        }
        audit(
            deps.db,
            &auth_ctx(&ec),
            AuditAction::WorkflowApprovalRequest,
            p.wait.approval_id,
            json!({
                "run_id": p.subscriber.run_id, "workflow_id": p.workflow_id, "table_id": p.table_id,
                "record_id": p.wait.record_id, "assignees": p.assignees,
            }),
        )
        .await;
        deps.runs
            .mark_approval_reconciled(&p.subscriber.tenant_id, p.wait.approval_id)
            .await?;
    }
    Ok(())
}

/// 行の削除で、その行に束縛した保留中の承認を取り下げる（上のモジュールの説明）。
pub(super) async fn withdraw_on_deletes(
    deps: &RelayDeps<'_>,
    events: &[storage::event::OutboxEvent],
) -> Result<(), anyhow::Error> {
    let mut by_tenant: std::collections::BTreeMap<&str, Vec<(Uuid, Uuid)>> =
        std::collections::BTreeMap::new();
    for ev in events {
        let p = &ev.payload;
        if p.get("event_type").and_then(Value::as_str) != Some(data::RECORD_CHANGED_EVENT)
            || p.get("change").and_then(Value::as_str) != Some("delete")
        {
            continue;
        }
        let id = |k: &str| {
            p.get(k)
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
        };
        if let (Some(t), Some(r)) = (id("table_id"), id("record_id")) {
            by_tenant
                .entry(ev.tenant_id.as_str())
                .or_default()
                .push((t, r));
        }
    }
    let port = DataPort {
        data: deps.data.as_ref(),
        fsms: deps.fsms.as_ref(),
        db: deps.db,
    };
    for (tenant, records) in by_tenant {
        for p in deps.runs.pending_approvals_on(tenant, &records).await? {
            let ec = exec_ctx(&p.subscriber);
            let gone = match port.get(&ec, p.table_id, p.wait.record_id).await {
                Err(e) if e.code == "not_found" => true,
                Err(e) if e.retryable => {
                    return Err(anyhow::anyhow!("{}: {}", e.code, e.message));
                }
                _ => false,
            };
            if gone {
                resolve(
                    deps,
                    &p.subscriber,
                    &p.wait,
                    &ApprovalResolution::withdrawn(),
                    None,
                )
                .await?;
            }
        }
    }
    Ok(())
}
