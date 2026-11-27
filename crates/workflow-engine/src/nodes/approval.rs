//! human.approval の実行（停止まで・GAP-08・D-24）。
//!
//! 1. 束縛した行を実行主体の権限で読む（data.get として scope 天井を通す・要求スコープ data.read）。
//!    いまの状態を `from_state`、rev を `suspend_rev` に残す（これ以下の遷移では起きない）。
//! 2. FSM にいまの状態から判断の行き先（承認・却下・差し戻しの `to`）への遷移が無ければ、待っても決まらない
//!    ので permanent 失敗 `approval_not_decidable` にする。
//! 3. 担当者を解決する（ユーザー・ロールの固定値・値式、行のユーザー・ロールの列）。空なら `approval_no_assignee`。
//! 4. [`Suspend::Approval`] を返す。台帳・担当者・購読は checkpoint の同じ Tx で書く（`run/store/approval.rs`）。
//!
//! 判断はここでは扱わない（承認者本人の FSM 遷移を relay が受けて起こす）。

use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::approval::{
    approval_id_for, ApprovalChoiceSpec, ApprovalSuspend, AssigneeKind, AssigneeRef,
};
use crate::capability::ScopeCeiling;
use crate::ir::params::{self, HumanApprovalParams, WaitTimeout};
use crate::run::{NodeContext, NodeResult, OnTimeout, Suspend};

use super::exec::CapabilityNodeExecutor;
use super::ports::PortError;
use super::resolver::{as_string, as_uuid, param, typed, ParamResolver};

impl CapabilityNodeExecutor {
    /// human.approval を停止まで進める（上のモジュールの説明）。
    pub(super) async fn node_human_approval(&self, raw: &Value, ctx: &NodeContext) -> NodeResult {
        let p: HumanApprovalParams = match params::parse(raw) {
            Ok(p) => p,
            Err(e) => {
                return NodeResult::fail("bad_params", format!("承認の設定が不正: {e}"), false)
            }
        };
        if let ScopeCeiling::Denied(_) = Self::check_ceiling("data.get", ctx) {
            self.audit.record_out_of_scope(
                &Self::exec_ctx(ctx),
                "data.get",
                &json!({ "reason": "out_of_scope", "step": ctx.step_path }),
            );
            return NodeResult::fail("out_of_scope", "scope_ceiling 外の操作: data.get", false);
        }
        let Ok(table_id) = uuid::Uuid::parse_str(&p.record.table) else {
            return NodeResult::fail("bad_params", "承認するテーブルの ID が不正です", false);
        };
        let r = ParamResolver::new(ctx);
        let record_id = match param(&p.record.id, &r, "承認する行")
            .and_then(|v| typed(&v, "承認する行", "行の ID", as_uuid))
        {
            Ok(id) => id,
            Err(e) => return NodeResult::fail(&e.code, e.message, e.retryable),
        };
        let ec = Self::exec_ctx(ctx);
        let target = match self
            .ports
            .data_approval_target(&ec, table_id, record_id)
            .await
        {
            Ok(t) => t,
            Err(e) => return NodeResult::fail(&e.code, e.message, e.retryable),
        };
        let Some(from_state) = target.current.clone() else {
            return not_decidable("承認する行に状態が設定されていません".into());
        };
        let choices: Vec<ApprovalChoiceSpec> = p
            .choices()
            .into_iter()
            .map(|(kind, c)| ApprovalChoiceSpec {
                kind,
                to: c.to.clone(),
                label: c
                    .label
                    .clone()
                    .filter(|l| !l.trim().is_empty())
                    .unwrap_or_else(|| kind.default_label().to_string()),
                comment: c.comment.unwrap_or_default(),
            })
            .collect();
        if let Some(c) = choices.iter().find(|c| !target.targets.contains(&c.to)) {
            return not_decidable(format!(
                "行の状態「{from_state}」から「{}」へ進む遷移が、状態の遷移の規則（FSM）にありません",
                c.to
            ));
        }
        let assignees = match resolve_assignees(&p, &r, &target) {
            Ok(a) => a,
            Err(e) => return NodeResult::fail(&e.code, e.message, e.retryable),
        };
        let timeout_at = match p.timeout_sec {
            None => None,
            Some(s) => match Duration::try_seconds(i64::from(s))
                .and_then(|d| Utc::now().checked_add_signed(d))
            {
                Some(at) => Some(at),
                None => return NodeResult::fail("bad_params", "期限が大きすぎます", false),
            },
        };
        NodeResult::wait(Suspend::Approval(Box::new(ApprovalSuspend {
            approval_id: approval_id_for(&ctx.tenant_id, &ctx.idempotency_key),
            table_id,
            record_id,
            from_state,
            suspend_rev: target.rev,
            title: p.title.trim().to_string(),
            instructions: p.instructions.clone().filter(|s| !s.trim().is_empty()),
            show_fields: p.show_fields().to_vec(),
            choices,
            assignees,
            timeout_at,
            on_timeout: match p.on_timeout.unwrap_or_default() {
                WaitTimeout::Continue => OnTimeout::Continue,
                WaitTimeout::Fail => OnTimeout::Fail,
            },
        })))
    }
}

/// 待っても決まらない承認（permanent・同じ行・同じ定義では直らない）。
fn not_decidable(message: String) -> NodeResult {
    NodeResult::fail("approval_not_decidable", message, false)
}

/// 担当者を解決する（重複を除く・空なら `approval_no_assignee`）。
///
/// 行の列（`field`）は実行主体に読めること（読めなければ失敗・黙って外さない）。値が空の列は数えない。
fn resolve_assignees(
    p: &HumanApprovalParams,
    r: &ParamResolver<'_>,
    target: &crate::nodes::ApprovalTarget,
) -> Result<Vec<AssigneeRef>, PortError> {
    let mut out: BTreeSet<AssigneeRef> = BTreeSet::new();
    for (i, a) in p.assignees.iter().enumerate() {
        let what = format!("承認の担当者 {}", i + 1);
        let fixed = [(AssigneeKind::User, &a.user), (AssigneeKind::Role, &a.role)];
        for (kind, expr) in fixed {
            let Some(expr) = expr else { continue };
            let subject =
                param(expr, r, &what).and_then(|v| typed(&v, &what, "ID の文字列", as_string))?;
            if !subject.trim().is_empty() {
                out.insert(AssigneeRef { kind, subject });
            }
        }
        if let Some(field) = &a.field {
            match target.refs.get(field) {
                Some((kind, value)) => {
                    if let Some(subject) = value.clone().filter(|v| !v.trim().is_empty()) {
                        out.insert(AssigneeRef {
                            kind: *kind,
                            subject,
                        });
                    }
                }
                None => {
                    return Err(PortError::new(
                        "approval_assignee_unreadable",
                        format!(
                            "担当者の列 {field} を読めません（ユーザー・ロールの列でないか、実行する人に見えない列です）"
                        ),
                        false,
                    ))
                }
            }
        }
    }
    if out.is_empty() {
        return Err(PortError::new(
            "approval_no_assignee",
            "承認の担当者がいません（担当者の列が空です）",
            false,
        ));
    }
    Ok(out.into_iter().collect())
}
