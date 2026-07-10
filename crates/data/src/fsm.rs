//! FSM 宣言的ガード（Task 9.10・2026-07 改訂）。
//!
//! 旧「軽量FSMエンジン」は廃止し、FSM を data サービスの**宣言的ガード**へ縮退させた
//! （miniapp-platform.md §1）。FSM = record の status フィールド＋遷移認可であり、
//! **副作用（通知/転記/AI）は持たない**。遷移コミットで outbox イベントを発行するのみで、
//! 実際の副作用は Phase 10 workflow-engine のトリガが担う。
//!
//! - 遷移は record 書込と**同一トランザクション**（原子的・途中状態なし）。
//! - 遷移認可 = Task 9.3 の行述語（[`PolicyExpr`]）を actor として再利用（当該行に対し評価）。
//! - status は行の可視性（row_policy）を駆動できる（`status` を述語に使える）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::policy::ast::PolicyExpr;

/// FSM 定義本文（`artifact(kind=fsm)` の body）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FsmBody {
    /// 状態集合（record の status フィールドが取り得る値）。
    pub states: Vec<String>,
    /// 遷移集合。
    pub transitions: Vec<FsmTransition>,
}

/// 1 つの遷移（from → to・actor 述語で認可）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FsmTransition {
    pub from: String,
    pub to: String,
    /// 遷移を実行できる主体の述語（当該行に対し評価・9.3 の PolicyExpr）。
    pub actor: PolicyExpr,
}

/// テーブルスキーマからの FSM 参照（バージョンピン・改訂は明示アップグレード）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FsmRef {
    pub artifact_id: Uuid,
    pub version: i64,
}

impl FsmBody {
    /// from 状態から to への遷移定義を引く（定義外は None）。
    pub fn transition(&self, from: &str, to: &str) -> Option<&FsmTransition> {
        self.transitions
            .iter()
            .find(|t| t.from == from && t.to == to)
    }
}

/// FSM 定義の静的検証（状態網羅・到達性・actor 式の妥当性）。
///
/// `status_field` はテーブルの select フィールドで、その options が states と一致する必要が
/// ある（呼び出し側が渡す）。actor 述語は 9.3 の行レベル検証を再利用する。
pub(crate) fn validate_fsm(
    body: &FsmBody,
    schema: &crate::model::TableSchema,
) -> Result<(), crate::DataError> {
    use crate::DataError;
    if body.states.is_empty() {
        return Err(DataError::Invalid("fsm: states が空です".into()));
    }
    let states: std::collections::HashSet<&str> = body.states.iter().map(String::as_str).collect();
    if states.len() != body.states.len() {
        return Err(DataError::Invalid("fsm: states に重複があります".into()));
    }
    if body.transitions.is_empty() {
        return Err(DataError::Invalid("fsm: transitions が空です".into()));
    }
    for (i, t) in body.transitions.iter().enumerate() {
        if !states.contains(t.from.as_str()) {
            return Err(DataError::Invalid(format!(
                "fsm: transition[{i}] の from '{}' が states にありません",
                t.from
            )));
        }
        if !states.contains(t.to.as_str()) {
            return Err(DataError::Invalid(format!(
                "fsm: transition[{i}] の to '{}' が states にありません",
                t.to
            )));
        }
        // actor 述語は行レベル述語と同じ文法・型整合で検証する（field_cmp のフィールド実在等）。
        crate::policy::validate::validate_policy_expr(
            schema,
            &t.actor,
            &format!("fsm.transition[{i}].actor"),
        )?;
    }
    // status_field が select でその options が states と一致することを検証する。
    let status_field = schema.status_field.as_deref().ok_or_else(|| {
        DataError::Invalid("fsm: テーブルに status_field が定義されていません".into())
    })?;
    let field = schema.field(status_field).ok_or_else(|| {
        DataError::Invalid(format!(
            "fsm: status_field '{status_field}' が fields にありません"
        ))
    })?;
    let opts: std::collections::HashSet<&str> = field.options.iter().map(String::as_str).collect();
    if opts != states {
        return Err(DataError::Invalid(format!(
            "fsm: status_field '{status_field}' の options が states と一致しません"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FieldDef, FieldType, TableSchema};
    use crate::policy::ast::{CmpOp, PolicyOperand};

    fn schema() -> TableSchema {
        let mut status = FieldDef {
            name: "status".into(),
            field_type: FieldType::Select,
            required: false,
            unique: false,
            indexed: true,
            options: vec!["draft".into(), "submitted".into(), "approved".into()],
            ref_table: None,
            lookup: None,
            computed: None,
        };
        status.options = vec!["draft".into(), "submitted".into(), "approved".into()];
        TableSchema {
            fields: vec![
                FieldDef {
                    name: "approver".into(),
                    field_type: FieldType::UserRef,
                    required: false,
                    unique: false,
                    indexed: false,
                    options: vec![],
                    ref_table: None,
                    lookup: None,
                    computed: None,
                },
                status,
            ],
            status_field: Some("status".into()),
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        }
    }

    fn body() -> FsmBody {
        FsmBody {
            states: vec!["draft".into(), "submitted".into(), "approved".into()],
            transitions: vec![
                FsmTransition {
                    from: "draft".into(),
                    to: "submitted".into(),
                    actor: PolicyExpr::Public,
                },
                FsmTransition {
                    from: "submitted".into(),
                    to: "approved".into(),
                    actor: PolicyExpr::FieldCmp {
                        field: "approver".into(),
                        op: CmpOp::Eq,
                        value: PolicyOperand::UserId,
                    },
                },
            ],
        }
    }

    #[test]
    fn valid_fsm_accepted() {
        assert!(validate_fsm(&body(), &schema()).is_ok());
        assert!(body().transition("draft", "submitted").is_some());
        assert!(body().transition("draft", "approved").is_none());
    }

    #[test]
    fn rejects_unknown_state_and_option_mismatch() {
        let mut b = body();
        b.transitions[0].to = "ghost".into();
        assert!(validate_fsm(&b, &schema()).is_err());
        // options 不一致。
        let mut s = schema();
        if let Some(f) = s.fields.iter_mut().find(|f| f.name == "status") {
            f.options = vec!["draft".into()];
        }
        assert!(validate_fsm(&body(), &s).is_err());
    }
}
