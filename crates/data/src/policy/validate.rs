//! row_policy のスキーマ時検証（Task 9.3）。
//!
//! フィールド実在・型整合・演算子/オペランドの組み合わせ・ネスト上限を保存時に強制し、
//! コンパイル時（クエリ時）に不正な式が到達しないようにする（fail-fast・PIT-21）。

use crate::model::{FieldType, TableSchema};
use crate::policy::ast::{
    CmpOp, PolicyExpr, PolicyOperand, RowPolicy, MAX_POLICY_BRANCHES, MAX_POLICY_DEPTH,
};
use crate::DataError;

/// ロール/ユーザーレベル式のみを許す検証（field_policy の readable_by 用・Task 9.4）。
///
/// マスク判定はリクエスト単位で一度だけ評価するため、行の値に依存する式
/// （field_cmp / is_owner）は使えない。
pub(crate) fn validate_role_level_expr(expr: &PolicyExpr, path: &str) -> Result<(), DataError> {
    validate_role_level_at(expr, 0, path)
}

fn validate_role_level_at(expr: &PolicyExpr, depth: usize, path: &str) -> Result<(), DataError> {
    if depth > MAX_POLICY_DEPTH {
        return Err(DataError::Invalid(format!(
            "{path}: ネストが深すぎます（最大 {MAX_POLICY_DEPTH}）"
        )));
    }
    match expr {
        PolicyExpr::Public => Ok(()),
        PolicyExpr::HasRole { role, .. } => {
            if role.is_empty() || role.len() > 256 {
                return Err(DataError::Invalid(format!("{path}: role が不正です")));
            }
            Ok(())
        }
        PolicyExpr::Any(children) | PolicyExpr::All(children) => {
            if children.is_empty() || children.len() > MAX_POLICY_BRANCHES {
                return Err(DataError::Invalid(format!(
                    "{path}: any/all の子は 1〜{MAX_POLICY_BRANCHES} 件で指定してください"
                )));
            }
            for (i, c) in children.iter().enumerate() {
                validate_role_level_at(c, depth + 1, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        PolicyExpr::FieldCmp { .. } | PolicyExpr::IsOwner => Err(DataError::Invalid(format!(
            "{path}: 行の値に依存する式（field_cmp/is_owner）はフィールドマスクに使えません"
        ))),
    }
}

/// row_policy 全体を検証する。
pub(crate) fn validate_row_policy(
    schema: &TableSchema,
    policy: &RowPolicy,
) -> Result<(), DataError> {
    validate_expr(schema, &policy.read, 0, "row_policy.read")?;
    if let Some(write) = &policy.write {
        validate_expr(schema, write, 0, "row_policy.write")?;
    }
    Ok(())
}

/// FSM actor 述語などの単発式検証（行レベル述語と同じ文法・型整合）。
pub(crate) fn validate_policy_expr(
    schema: &TableSchema,
    expr: &PolicyExpr,
    path: &str,
) -> Result<(), DataError> {
    validate_expr(schema, expr, 0, path)
}

fn validate_expr(
    schema: &TableSchema,
    expr: &PolicyExpr,
    depth: usize,
    path: &str,
) -> Result<(), DataError> {
    if depth > MAX_POLICY_DEPTH {
        return Err(DataError::Invalid(format!(
            "{path}: ネストが深すぎます（最大 {MAX_POLICY_DEPTH}）"
        )));
    }
    match expr {
        PolicyExpr::Any(children) | PolicyExpr::All(children) => {
            if children.is_empty() {
                return Err(DataError::Invalid(format!(
                    "{path}: any/all の子が空です（空は常に不成立になるため定義エラーとして拒否）"
                )));
            }
            if children.len() > MAX_POLICY_BRANCHES {
                return Err(DataError::Invalid(format!(
                    "{path}: any/all の子が多すぎます（最大 {MAX_POLICY_BRANCHES}）"
                )));
            }
            for (i, c) in children.iter().enumerate() {
                validate_expr(schema, c, depth + 1, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        PolicyExpr::Public | PolicyExpr::IsOwner => Ok(()),
        PolicyExpr::HasRole { role, .. } => {
            if role.is_empty() || role.len() > 256 {
                return Err(DataError::Invalid(format!("{path}: role が不正です")));
            }
            Ok(())
        }
        PolicyExpr::FieldCmp { field, op, value } => {
            let f = schema.field(field).ok_or_else(|| {
                DataError::Invalid(format!(
                    "{path}: フィールド '{field}' がスキーマにありません"
                ))
            })?;
            // 派生フィールドは述語の材料にしない（値が書込時検証を通らないため信頼できない）。
            if matches!(f.field_type, FieldType::Lookup | FieldType::Computed) {
                return Err(DataError::Invalid(format!(
                    "{path}: 派生フィールド '{field}' は row_policy に使えません"
                )));
            }
            match value {
                PolicyOperand::UserId => {
                    if f.field_type != FieldType::UserRef {
                        return Err(DataError::Invalid(format!(
                            "{path}: '$user.id' は user_ref フィールドにのみ比較できます（'{field}' は {:?}）",
                            f.field_type
                        )));
                    }
                    if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
                        return Err(DataError::Invalid(format!(
                            "{path}: '$user.id' の演算子は eq/ne のみです"
                        )));
                    }
                }
                PolicyOperand::UserRoles { .. } => {
                    if f.field_type != FieldType::RoleRef {
                        return Err(DataError::Invalid(format!(
                            "{path}: '$user.roles' は role_ref フィールドにのみ比較できます（'{field}' は {:?}）",
                            f.field_type
                        )));
                    }
                    if *op != CmpOp::In {
                        return Err(DataError::Invalid(format!(
                            "{path}: '$user.roles' の演算子は in のみです"
                        )));
                    }
                }
                PolicyOperand::Lit(v) => validate_lit(f.field_type, *op, v, path, field)?,
            }
            Ok(())
        }
    }
}

/// リテラル比較の型整合（text/select/number のみ・閉じた形）。
fn validate_lit(
    ty: FieldType,
    op: CmpOp,
    v: &serde_json::Value,
    path: &str,
    field: &str,
) -> Result<(), DataError> {
    let scalar_ok = |v: &serde_json::Value| match ty {
        FieldType::Number => v.is_number(),
        FieldType::Text | FieldType::Select | FieldType::UserRef | FieldType::RoleRef => {
            v.is_string()
        }
        _ => false,
    };
    match op {
        CmpOp::Eq | CmpOp::Ne => {
            if !scalar_ok(v) {
                return Err(DataError::Invalid(format!(
                    "{path}: '{field}'（{ty:?}）とリテラル {v} は比較できません"
                )));
            }
        }
        CmpOp::In => {
            // number の in はコンパイラ側で未対応（配列 numeric 比較を組まない）。
            // 述語が実行時 500 になるのを防ぐため保存時に拒否する（Codex P2）。
            if ty == FieldType::Number {
                return Err(DataError::Invalid(format!(
                    "{path}: number フィールドに in は使えません（eq/ne を使ってください）"
                )));
            }
            let arr = v.as_array().ok_or_else(|| {
                DataError::Invalid(format!("{path}: in のリテラルは配列で指定してください"))
            })?;
            if arr.is_empty() || arr.len() > 100 {
                return Err(DataError::Invalid(format!(
                    "{path}: in の配列は 1〜100 件で指定してください"
                )));
            }
            if !arr.iter().all(scalar_ok) {
                return Err(DataError::Invalid(format!(
                    "{path}: in の配列要素が '{field}'（{ty:?}）と型不一致です"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FieldDef;
    use serde_json::json;

    fn field(name: &str, ty: FieldType) -> FieldDef {
        FieldDef {
            name: name.into(),
            field_type: ty,
            required: false,
            unique: false,
            indexed: false,
            options: vec![],
            ref_table: None,
            lookup: None,
            computed: None,
        }
    }

    fn schema() -> TableSchema {
        let mut status = field("status", FieldType::Select);
        status.options = vec!["draft".into(), "done".into()];
        TableSchema {
            fields: vec![
                field("applicant", FieldType::UserRef),
                field("dept", FieldType::RoleRef),
                field("amount", FieldType::Number),
                status,
            ],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        }
    }

    fn policy(read: PolicyExpr) -> RowPolicy {
        RowPolicy { read, write: None }
    }

    #[test]
    fn accepts_typical_policy() {
        let p = policy(PolicyExpr::Any(vec![
            PolicyExpr::FieldCmp {
                field: "applicant".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            PolicyExpr::FieldCmp {
                field: "dept".into(),
                op: CmpOp::In,
                value: PolicyOperand::UserRoles { subtree: true },
            },
            PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
            PolicyExpr::IsOwner,
        ]));
        assert!(validate_row_policy(&schema(), &p).is_ok());
    }

    #[test]
    fn rejects_unknown_field_and_type_mismatch() {
        // 未知フィールド。
        let p = policy(PolicyExpr::FieldCmp {
            field: "nope".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        });
        assert!(validate_row_policy(&schema(), &p).is_err());
        // $user.id を user_ref 以外へ。
        let p = policy(PolicyExpr::FieldCmp {
            field: "amount".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        });
        assert!(validate_row_policy(&schema(), &p).is_err());
        // $user.roles は in のみ。
        let p = policy(PolicyExpr::FieldCmp {
            field: "dept".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserRoles { subtree: true },
        });
        assert!(validate_row_policy(&schema(), &p).is_err());
        // リテラル型不一致。
        let p = policy(PolicyExpr::FieldCmp {
            field: "amount".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::Lit(json!("x")),
        });
        assert!(validate_row_policy(&schema(), &p).is_err());
    }

    #[test]
    fn rejects_empty_branches_and_depth() {
        assert!(validate_row_policy(&schema(), &policy(PolicyExpr::Any(vec![]))).is_err());
        // 深さ超過。
        let mut e = PolicyExpr::Public;
        for _ in 0..(MAX_POLICY_DEPTH + 2) {
            e = PolicyExpr::All(vec![e]);
        }
        assert!(validate_row_policy(&schema(), &policy(e)).is_err());
    }

    #[test]
    fn write_branch_and_all_too_many_and_hasrole_empty() {
        // write 側も検証される（未知フィールドを write に入れると拒否）。
        let p = RowPolicy {
            read: PolicyExpr::Public,
            write: Some(PolicyExpr::FieldCmp {
                field: "nope".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            }),
        };
        assert!(validate_row_policy(&schema(), &p).is_err());
        // all の子が多すぎる。
        let many: Vec<PolicyExpr> = (0..=MAX_POLICY_BRANCHES)
            .map(|_| PolicyExpr::Public)
            .collect();
        assert!(validate_row_policy(&schema(), &policy(PolicyExpr::All(many))).is_err());
        // role 空は不正。
        let p = policy(PolicyExpr::HasRole {
            role: String::new(),
            subtree: false,
        });
        assert!(validate_row_policy(&schema(), &p).is_err());
    }

    #[test]
    fn lit_in_operator_rules() {
        // select の in（配列）は OK。
        let ok = policy(PolicyExpr::FieldCmp {
            field: "status".into(),
            op: CmpOp::In,
            value: PolicyOperand::Lit(json!(["draft", "done"])),
        });
        assert!(validate_row_policy(&schema(), &ok).is_ok());
        // number に in は不可。
        let num_in = policy(PolicyExpr::FieldCmp {
            field: "amount".into(),
            op: CmpOp::In,
            value: PolicyOperand::Lit(json!([1, 2])),
        });
        assert!(validate_row_policy(&schema(), &num_in).is_err());
        // in のリテラルが配列でない。
        let not_arr = policy(PolicyExpr::FieldCmp {
            field: "status".into(),
            op: CmpOp::In,
            value: PolicyOperand::Lit(json!("draft")),
        });
        assert!(validate_row_policy(&schema(), &not_arr).is_err());
        // in の配列が空。
        let empty = policy(PolicyExpr::FieldCmp {
            field: "status".into(),
            op: CmpOp::In,
            value: PolicyOperand::Lit(json!([])),
        });
        assert!(validate_row_policy(&schema(), &empty).is_err());
        // in の要素型不一致（select に数値）。
        let bad_elem = policy(PolicyExpr::FieldCmp {
            field: "status".into(),
            op: CmpOp::In,
            value: PolicyOperand::Lit(json!([1])),
        });
        assert!(validate_row_policy(&schema(), &bad_elem).is_err());
    }

    #[test]
    fn fsm_actor_expr_is_validated() {
        // validate_policy_expr（FSM actor 述語）は行述語と同じ文法・型整合。
        let ok = PolicyExpr::FieldCmp {
            field: "applicant".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        };
        assert!(validate_policy_expr(&schema(), &ok, "fsm.actor").is_ok());
        let bad = PolicyExpr::FieldCmp {
            field: "amount".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        };
        assert!(validate_policy_expr(&schema(), &bad, "fsm.actor").is_err());
    }

    #[test]
    fn role_level_expr_allows_role_shapes_only() {
        // field_policy.readable_by 用: role/公開の形のみ許す。
        assert!(validate_role_level_expr(&PolicyExpr::Public, "fp").is_ok());
        assert!(validate_role_level_expr(
            &PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true
            },
            "fp"
        )
        .is_ok());
        assert!(validate_role_level_expr(
            &PolicyExpr::Any(vec![PolicyExpr::HasRole {
                role: "a".into(),
                subtree: false,
            }]),
            "fp"
        )
        .is_ok());
        // role 空は不正。
        assert!(validate_role_level_expr(
            &PolicyExpr::HasRole {
                role: String::new(),
                subtree: false
            },
            "fp"
        )
        .is_err());
        // any の子が空は不正。
        assert!(validate_role_level_expr(&PolicyExpr::Any(vec![]), "fp").is_err());
        // 行の値に依存する式（field_cmp / is_owner）はマスクに使えない。
        assert!(validate_role_level_expr(&PolicyExpr::IsOwner, "fp").is_err());
        assert!(validate_role_level_expr(
            &PolicyExpr::FieldCmp {
                field: "applicant".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            "fp"
        )
        .is_err());
        // 深さ超過。
        let mut e = PolicyExpr::Public;
        for _ in 0..(MAX_POLICY_DEPTH + 2) {
            e = PolicyExpr::All(vec![e]);
        }
        assert!(validate_role_level_expr(&e, "fp").is_err());
    }
}
