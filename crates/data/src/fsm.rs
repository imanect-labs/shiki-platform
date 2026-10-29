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

/// FSM 定義の静的検証（状態網羅・遷移の重複・actor 式の妥当性）。
///
/// `status_field` はテーブルの select フィールドで、その options が states と一致する必要が
/// ある（呼び出し側が渡す）。actor 述語は 9.3 の行レベル検証を再利用する。文言はテーブルの持ち主が
/// 「状態の進め方」の画面で読むので、画面の呼び名（状態・進め方・状態の列）で言う（付録 C）。
pub(crate) fn validate_fsm(
    body: &FsmBody,
    schema: &crate::model::TableSchema,
) -> Result<(), crate::DataError> {
    let invalid =
        |m: String| crate::DataError::input_detail(crate::InvalidCode::FsmInvalid, None, m);
    if body.states.is_empty() {
        return Err(invalid("状態を 1 つ以上決めてください。".into()));
    }
    let states: std::collections::HashSet<&str> = body.states.iter().map(String::as_str).collect();
    if states.len() != body.states.len() {
        return Err(invalid(
            "同じ名前の状態が 2 つあります。どちらかの名前を変えてください。".into(),
        ));
    }
    if body.transitions.is_empty() {
        return Err(invalid("進め方を 1 つ以上追加してください。".into()));
    }
    let mut pairs = std::collections::HashSet::new();
    for (i, t) in body.transitions.iter().enumerate() {
        for s in [&t.from, &t.to] {
            if !states.contains(s.as_str()) {
                return Err(invalid(format!(
                    "「{s}」は状態にありません。進め方の状態を選び直してください。"
                )));
            }
        }
        if t.from == t.to {
            return Err(invalid(format!(
                "「{}」から同じ状態へは進められません。次の状態を選び直してください。",
                t.from
            )));
        }
        // 同じ組を 2 つ置くと、遷移の判定（`transition`）は先の 1 つしか見ない（後の進められる人が効かない）。
        if !pairs.insert((t.from.as_str(), t.to.as_str())) {
            return Err(invalid(format!(
                "「{}」から「{}」への進め方が 2 つあります。1 つにまとめてください。",
                t.from, t.to
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
    let status_field = schema
        .status_field
        .as_deref()
        .ok_or_else(|| invalid("状態の列を選んでください。".into()))?;
    let field = schema
        .field(status_field)
        .ok_or_else(|| invalid("状態の列がテーブルにありません。選び直してください。".into()))?;
    let opts: std::collections::HashSet<&str> = field.options.iter().map(String::as_str).collect();
    if opts != states {
        return Err(invalid(format!(
            "「{}」の選択肢と、状態の進め方の状態が合いません。状態は「状態の進め方」で変えてください。",
            field.label()
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
        let mut status = FieldDef::new("status", FieldType::Select);
        status.indexed = true;
        status.options = vec!["draft".into(), "submitted".into(), "approved".into()];
        TableSchema {
            fields: vec![FieldDef::new("approver", FieldType::UserRef), status],
            status_field: Some("status".into()),
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
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
        if let Some(f) = s.fields.iter_mut().find(|f| f.key == "status") {
            f.options = vec!["draft".into()];
        }
        assert!(validate_fsm(&body(), &s).is_err());
    }

    /// 同じ組の進め方を 2 つ置く・同じ状態へ進める定義は、保存の前に断る（遷移の判定は先の 1 つしか見ない）。
    #[test]
    fn rejects_duplicate_pairs_and_self_loops() {
        let mut b = body();
        b.transitions.push(FsmTransition {
            from: "draft".into(),
            to: "submitted".into(),
            actor: PolicyExpr::IsOwner,
        });
        let e = validate_fsm(&b, &schema()).unwrap_err();
        assert_eq!(e.invalid_code(), Some(crate::InvalidCode::FsmInvalid));
        assert!(e.to_string().contains("2 つあります"), "{e}");
        let mut b = body();
        b.transitions[0].to = "draft".into();
        let e = validate_fsm(&b, &schema()).unwrap_err();
        assert!(e.to_string().contains("同じ状態"), "{e}");
    }
}
