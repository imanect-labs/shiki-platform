//! 承認待ち（human.approval・GAP-08）の受付の突合・行の削除による取り下げ・期限・監査（relay・tick から呼ぶ）。
//!
//! 判断の遷移そのものは、データ変更の wait(event) と同じ候補（[`super::relay_data`]）として起きる（その行の
//! `data.transition` で `rev > suspend_rev` のもの・実行主体の見え方）。ここは残りの 3 つを受け持つ。
//!
//! - **受付の突合**: 停止の commit より前に起きた遷移は、購読ができる前に配送済みになり得る。保留中で未突合の
//!   承認を、実行主体の見え方で行を読み直して決める。いまの状態が停止時の状態なら待ち続け、判断の行き先なら
//!   その出口へ、それ以外・行が見えない（消えた）なら取り下げる。承認の依頼の監査もここで書く。
//! - **行の削除**: outbox の削除（`data.record.changed` / delete）を受けたら、その行に束縛した保留中の承認を、
//!   実行主体の `data_get` が NotFound であることを確かめて取り下げる（見えなくなっただけでは取り下げない）。
//! - **期限**（[`expire_due_approvals`]・scheduler の tick）: 期限の来た承認を期限切れにする（on_timeout で
//!   timeout の出口か run の失敗）。その前に実行主体がまだテーブルを読めるかを確かめる。承認の期限切れを決めるのは
//!   ここだけで、確かめが一時障害で終わった承認は決めずに残す（ほかの承認・ほかの wait の回収は止めない）。
//!
//! **実行主体の失権**（D-62）: 受付の突合・判断の遷移を受けた relay・期限の 3 つとも、実行主体の委譲・テーブルの
//! 権限が停止の後に外れていたら（委譲が無い・テーブル単位の NotFound / Forbidden・user 主体の本人の失権を含む）、
//! 判断の遷移が二度と見えず、どの出口へ進めても後続が委譲なしで中途半端に走るだけなので、出口へは進めずに run を
//! [`ACTOR_UNAVAILABLE`] で失敗させる（[`ApprovalResolution::actor_unavailable`]）。決着の監査に同じ理由を残す。
//!
//! 権限の判定（Forbidden / NotFound）は承認の結論に使い、一時的な障害だけを `Err` で返す（次の tick で配り直す）。

use authz::{AuthContext, Principal, PrincipalKind, Relation};
use data::{ChangeEventKind, ChangeEventRef, DataError, DataStore, FsmStore};
use serde_json::{json, Value};
use storage::audit::{AuditAction, AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;
pub(super) use workflow_engine::approval::ACTOR_UNAVAILABLE;
use workflow_engine::{
    ApprovalResolution, ApprovalWait, EventWaitSubscriber, ExecCtx, PendingApproval,
    ResolvedApproval, RunStore,
};

use super::auth::auth_ctx;
use super::data_port::{Actor, DataPort};
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

/// relay の system 主体（監査の actor）。tenant_id が文字ポリシーに反するなら組まない（#1032）。
fn system_ctx(tenant_id: &str, org: &str) -> Option<AuthContext> {
    let tenant = authz::TenantId::parse(tenant_id).ok()?;
    Some(AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "system:workflow-relay".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.to_string(),
        tenant,
    ))
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
    // 監査の actor を組めない（tenant_id が不正）なら書かない（ほかの行と同じく relay を止めない）。
    let Some(ctx) = system_ctx(&r.tenant_id, &r.org) else {
        tracing::warn!(approval_id = %r.approval_id, "承認の決着の監査を書けない（tenant_id が不正）");
        return;
    };
    audit(
        db,
        &ctx,
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
    resolve_on(deps.db, deps.runs, subscriber, wait, res, reason)
        .await
        .map(|_| ())
}

/// [`resolve`] の本体（relay の外の tick からも呼ぶ・戻り値 = 起こしたか）。
async fn resolve_on(
    db: &sqlx::PgPool,
    runs: &RunStore,
    subscriber: &EventWaitSubscriber,
    wait: &ApprovalWait,
    res: &ApprovalResolution,
    reason: Option<&str>,
) -> Result<bool, anyhow::Error> {
    // 監査には確定した status を書く（期限切れのつもりでも、判断 API が期限内に受け付けた判断で決まり得る）。
    let Some(status) = runs.resolve_approval(subscriber, wait, res).await? else {
        return Ok(false);
    };
    audit_resolved_because(
        db,
        &ResolvedApproval {
            tenant_id: subscriber.tenant_id.clone(),
            org: subscriber.org.clone(),
            approval_id: wait.approval_id,
            run_id: subscriber.run_id,
            status: status.to_string(),
        },
        reason,
    )
    .await;
    Ok(true)
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
            // 出口へ進めずに run を失敗させ、理由を監査に残す（D-62）。
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
                Err(e) if e.is_transient() => {
                    tracing::warn!(error = %e, "承認の受付の突合に失敗（次の tick で再試行）");
                    continue;
                }
                Err(e @ (DataError::NotFound | DataError::Forbidden)) => {
                    // テーブルそのものを読めない（user 主体の本人・委譲元が ReBAC を失った）なら、判断の遷移が
                    // 二度と見えないので run を失敗させる（D-62）。行が見えない・消えたなら理由なしで取り下げる。
                    match table_unreadable(&a, p.table_id).await {
                        Ok(true) => {
                            fail_unavailable(deps, &p.subscriber, &p.wait).await?;
                        }
                        Ok(false) if matches!(e, DataError::NotFound) => {
                            resolve(deps, &p.subscriber, &p.wait, &withdrawn, None).await?;
                        }
                        // テーブルは読めるのに行の権限が無い: 待ち続ける。
                        Ok(false) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "承認の受付の突合に失敗（次の tick で再試行）");
                            continue;
                        }
                    }
                }
                // 状態の列が読めない・その他の確定的な誤り: 待ち続ける。
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
            fail_unavailable(deps, &p.subscriber, &p.wait).await?;
        }
        // 実行主体を組めない（run の tenant_id が不正）なら監査は書かない（突合の印は付ける）。
        if let Ok(ctx) = auth_ctx(&ec) {
            audit(
                deps.db,
                &ctx,
                AuditAction::WorkflowApprovalRequest,
                p.wait.approval_id,
                json!({
                    "run_id": p.subscriber.run_id, "workflow_id": p.workflow_id, "table_id": p.table_id,
                    "record_id": p.wait.record_id, "assignees": p.assignees,
                }),
            )
            .await;
        }
        deps.runs
            .mark_approval_reconciled(&p.subscriber.tenant_id, p.wait.approval_id)
            .await?;
    }
    Ok(())
}

/// 実行主体が承認を待つ間に失権した承認を、出口へ進めずに run の失敗で確定し、理由を監査に残す（D-62）。
pub(super) async fn fail_unavailable(
    deps: &RelayDeps<'_>,
    subscriber: &EventWaitSubscriber,
    wait: &ApprovalWait,
) -> Result<(), anyhow::Error> {
    let res = ApprovalResolution::actor_unavailable();
    resolve(deps, subscriber, wait, &res, Some(ACTOR_UNAVAILABLE)).await
}

/// 期限の回収の 1 回に読む件数と回数の上限（[`expire_due_approvals_within`]・timer と timeout の回収と同じ・BUG-76）。
pub use workflow_engine::DueSweepLimits;

/// 期限の回収の結果（tick が警告に残す）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DueApprovalSweep {
    /// 決着させた承認の数。
    pub resolved: usize,
    /// 確かめ・確定が一時障害で終わり、決めずに残した承認の数（次の tick で確かめ直す）。
    pub deferred: usize,
    /// 上限まで読んでも続きがあった（残りは次の tick）。
    pub more: bool,
}

/// 承認の期限の回収（scheduler の tick・結合テストも同じ口を呼ぶ）。
///
/// 期限の来た保留中の承認を 1 件ずつ、実行主体がまだテーブルを読めるかを確かめてから決める。読めなくなっていたら
/// （委譲が無い・テーブル単位の NotFound / Forbidden）期限の出口（timeout）へ進めずに run を [`ACTOR_UNAVAILABLE`]
/// で失敗させ（受付の突合・判断の遷移を受けた relay と揃える・D-62）、読めるなら期限切れにする（on_timeout で
/// timeout の出口か run の失敗）。どちらも決着の監査を書く。
///
/// 承認の期限切れはここだけが決める（`expire_due_waits` は承認を扱わない）。1 件の確かめ・確定が一時障害で
/// 終わっても、その承認だけを決めずに残し（期限の出口へは進めない・次の tick で確かめ直す）、ほかの承認
/// （ほかのテナントの分を含む）の回収を続ける。決められずに残った承認は、同じ回収の次のページで読み直さない
/// （期限・承認 id の位置から続きを読む）。`Err` は期限の来た承認を読めなかったときだけ。
pub async fn expire_due_approvals(
    db: &sqlx::PgPool,
    runs: &RunStore,
    data: &DataStore,
    fsms: &FsmStore,
    now: chrono::DateTime<chrono::Utc>,
    tenant_scope: Option<&str>,
) -> Result<DueApprovalSweep, anyhow::Error> {
    expire_due_approvals_within(
        db,
        runs,
        data,
        fsms,
        now,
        tenant_scope,
        DueSweepLimits::DEFAULT,
    )
    .await
}

/// [`expire_due_approvals`] を、1 回に読む件数と回数の上限を決めて呼ぶ（結合テストが小さなページで呼ぶ）。
#[doc(hidden)]
pub async fn expire_due_approvals_within(
    db: &sqlx::PgPool,
    runs: &RunStore,
    data: &DataStore,
    fsms: &FsmStore,
    now: chrono::DateTime<chrono::Utc>,
    tenant_scope: Option<&str>,
    limits: DueSweepLimits,
) -> Result<DueApprovalSweep, anyhow::Error> {
    let port = DataPort { data, fsms, db };
    let mut sweep = DueApprovalSweep::default();
    let mut after = None;
    for _ in 0..limits.pages {
        let page = runs
            .due_approvals(now, tenant_scope, after, limits.batch)
            .await?;
        for p in page.approvals {
            match expire_one(db, runs, &port, &p).await {
                Ok(true) => sweep.resolved += 1,
                Ok(false) => {}
                // 確かめられないまま期限切れにすると、失権した実行主体で後続が走り得る。この承認だけを残し、
                // 次の tick で確かめ直す（ほかの承認の回収は止めない）。
                Err(e) => {
                    sweep.deferred += 1;
                    tracing::warn!(
                        error = %e,
                        approval_id = %p.wait.approval_id,
                        tenant = %p.subscriber.tenant_id,
                        "承認の期限の前の確かめに失敗（この承認は次の tick で確かめ直す）"
                    );
                }
            }
        }
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(sweep),
        }
    }
    sweep.more = true;
    Ok(sweep)
}

/// 期限の来た承認 1 件を、実行主体を確かめてから決める（戻り値 = 決着させたか・一時障害は `Err`）。
async fn expire_one(
    db: &sqlx::PgPool,
    runs: &RunStore,
    port: &DataPort<'_>,
    p: &PendingApproval,
) -> Result<bool, anyhow::Error> {
    let ec = exec_ctx(&p.subscriber);
    let lost = match port.actor(&ec, p.table_id, Relation::Viewer).await {
        Err(e) if !e.retryable => true,
        Err(e) => return Err(anyhow::anyhow!("{}: {}", e.code, e.message)),
        Ok(a) => table_unreadable(&a, p.table_id).await?,
    };
    let (res, reason) = if lost {
        (
            ApprovalResolution::actor_unavailable(),
            Some(ACTOR_UNAVAILABLE),
        )
    } else {
        (ApprovalResolution::expired(p.wait.on_timeout), None)
    };
    resolve_on(db, runs, &p.subscriber, &p.wait, &res, reason).await
}

/// 実行主体がテーブルそのものを読めないか（テーブル単位の NotFound / Forbidden）。行が見えないだけなら偽。
///
/// 承認の行の読取（`approval_target`）の NotFound は、テーブルが見えないのか行が見えないのかを区別しないので、
/// 断られたときだけテーブルを読み直して見分ける（一時障害は `Err`・次の tick で再試行）。
async fn table_unreadable(a: &Actor, table_id: Uuid) -> Result<bool, DataError> {
    match a.data.get_table(&a.ctx, table_id, None).await {
        Err(DataError::NotFound | DataError::Forbidden) => Ok(true),
        Err(e) if e.is_transient() => Err(e),
        // 読めた・テーブルの読込のその他の確定的な誤り（読めないとは言えないので、従来どおり行で決める）。
        Ok(_) | Err(_) => Ok(false),
    }
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
