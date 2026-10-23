//! 事後検査（query-optimizer.md §7.2）: 本体を取ったあとで、同じ行述語を **Rust で独立に**評価し直す。
//!
//! - SQL へのコンパイラ（[`crate::policy::compile`] / [`crate::policy::compile_index`]）とは別実装。
//!   同じバグを二度書かない。
//! - 評価は**行述語の意味論**（v1）: 値の無い行の `ne` は偽、数値は数値として比較。クエリ条件の
//!   2 値論理の評価器は流用しない（流用すると Guard の降下の誤りと同じ誤りを犯して検出できない）。
//! - 不一致の行は応答から落とし、セキュリティ指標として記録して警報を出す（オプティマイザか SQL 生成の
//!   バグを意味する）。オプティマイザの誤りは「見えるべきでない行が出る」ではなく「欠ける」に倒れる。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use uuid::Uuid;

use crate::field_key::json_key;
use crate::model::{FieldType, TableSchema};
use crate::numeric::Dec;
use crate::policy::ast::{CmpOp, PolicyExpr, PolicyOperand};
use crate::policy::material::PolicyMaterial;

/// プロセス全体で事後検査が落とした行数（警報の指標・0 であるべき）。
static VERIFY_DROPS: AtomicU64 = AtomicU64::new(0);

/// 事後検査が落とした行数の累計（運用の指標）。
pub(crate) fn verify_drops_total() -> u64 {
    VERIFY_DROPS.load(Ordering::Relaxed)
}

/// 評価対象の行（本体の保存形 `"f{id}"` キー）。
pub(crate) struct RowView<'a> {
    pub id: Uuid,
    pub owner: &'a str,
    pub data: &'a Value,
}

/// 行述語の材料。
pub(crate) struct PolicyEnv<'a> {
    pub schema: &'a TableSchema,
    pub material: &'a PolicyMaterial,
    pub user: &'a str,
    pub shared: &'a HashSet<Uuid>,
}

/// 読取の行述語 `(row_policy OR 個別共有)` を評価する（row_policy なしは真）。
pub(crate) fn eval_read(env: &PolicyEnv<'_>, row: &RowView<'_>) -> bool {
    let Some(policy) = &env.schema.row_policy else {
        return true;
    };
    eval(&policy.read, env, row) || env.shared.contains(&row.id)
}

fn eval(e: &PolicyExpr, env: &PolicyEnv<'_>, row: &RowView<'_>) -> bool {
    match e {
        PolicyExpr::Public => true,
        PolicyExpr::HasRole { role, subtree } => env.material.has_role(role, *subtree),
        PolicyExpr::IsOwner => row.owner == env.user,
        PolicyExpr::Any(v) => !v.is_empty() && v.iter().any(|c| eval(c, env, row)),
        PolicyExpr::All(v) => !v.is_empty() && v.iter().all(|c| eval(c, env, row)),
        PolicyExpr::FieldCmp { field, op, value } => {
            let Some(f) = env.schema.field(field) else {
                return false;
            };
            // v1 の `data ->> 'f{id}'`: 値なし（欠落・null）は NULL → どの比較も不成立。
            let Some(text) = scalar_text(row.data.get(json_key(f.id))) else {
                return false;
            };
            let numeric = f.field_type == FieldType::Number;
            let eq = |lit: &str| -> Option<bool> {
                if numeric {
                    Some(Dec::parse(&text)? == Dec::parse(lit)?)
                } else {
                    Some(text == lit)
                }
            };
            match (op, value) {
                (CmpOp::Eq, PolicyOperand::UserId) => eq(env.user).unwrap_or(false),
                (CmpOp::Ne, PolicyOperand::UserId) => eq(env.user).is_some_and(|b| !b),
                (CmpOp::In, PolicyOperand::UserRoles { subtree }) => env
                    .material
                    .roles(*subtree)
                    .iter()
                    .any(|r| eq(r).unwrap_or(false)),
                (CmpOp::Eq | CmpOp::Ne, PolicyOperand::Lit(v)) => {
                    let lit = match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => return false,
                    };
                    match eq(&lit) {
                        Some(b) => (*op == CmpOp::Eq) == b,
                        None => false,
                    }
                }
                (CmpOp::In, PolicyOperand::Lit(Value::Array(arr))) => arr
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|s| eq(s).unwrap_or(false)),
                _ => false,
            }
        }
    }
}

/// `->>` 相当の text 表現（null は None）。
fn scalar_text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// ページの行を事後検査し、不一致を落とす。落とした件数を返す（0 であるべき）。
pub(crate) fn post_filter<T>(
    env: &PolicyEnv<'_>,
    rows: &mut Vec<T>,
    view: impl Fn(&T) -> RowView<'_>,
    table_id: Uuid,
) -> u64 {
    let before = rows.len();
    rows.retain(|r| eval_read(env, &view(r)));
    let dropped = (before - rows.len()) as u64;
    if dropped > 0 {
        VERIFY_DROPS.fetch_add(dropped, Ordering::Relaxed);
        // 値や内部キーは出さない（件数とテーブルだけ）。
        tracing::error!(
            target: "security",
            alert = "data.query.post_filter_mismatch",
            %table_id,
            dropped,
            "行述語の事後検査で不一致の行を落としました（オプティマイザまたは SQL 生成の不具合）"
        );
    }
    dropped
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::FieldDef;
    use crate::policy::ast::RowPolicy;
    use serde_json::json;
    use std::sync::Arc;

    fn schema(read: PolicyExpr) -> TableSchema {
        let mut s = TableSchema {
            fields: vec![
                FieldDef::new("applicant", FieldType::UserRef),
                FieldDef::new("amount", FieldType::Number),
            ],
            status_field: None,
            row_policy: Some(RowPolicy { read, write: None }),
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        s
    }

    fn mat() -> PolicyMaterial {
        PolicyMaterial {
            roles_direct: Some(Arc::new(vec![])),
            roles_effective: Some(Arc::new(vec![])),
            shared_record_ids: vec![],
            shares_truncated: false,
            role_members: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn ne_on_missing_value_is_false_like_v1() {
        let s = schema(PolicyExpr::FieldCmp {
            field: "applicant".into(),
            op: CmpOp::Ne,
            value: PolicyOperand::UserId,
        });
        let m = mat();
        let shared = HashSet::new();
        let env = PolicyEnv {
            schema: &s,
            material: &m,
            user: "alice",
            shared: &shared,
        };
        let id = Uuid::nil();
        let row = |d: Value| (d, id);
        for (data, want) in [
            (json!({}), false),               // 値なし → 不成立（fail-closed）
            (json!({"f16": null}), false),    // null も同じ
            (json!({"f16": "alice"}), false), // 本人 → 不成立
            (json!({"f16": "bob"}), true),    // 他人 → 成立
        ] {
            let (d, id) = row(data.clone());
            let v = RowView {
                id,
                owner: "x",
                data: &d,
            };
            assert_eq!(eval_read(&env, &v), want, "{data}");
        }
    }

    #[test]
    fn numbers_compare_as_numbers() {
        let s = schema(PolicyExpr::FieldCmp {
            field: "amount".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::Lit(json!(1)),
        });
        let m = mat();
        let shared = HashSet::new();
        let env = PolicyEnv {
            schema: &s,
            material: &m,
            user: "u",
            shared: &shared,
        };
        let d = json!({"f17": 1.0});
        let v = RowView {
            id: Uuid::nil(),
            owner: "o",
            data: &d,
        };
        assert!(eval_read(&env, &v));
    }

    #[test]
    fn mismatches_are_dropped_and_alerted() {
        let s = schema(PolicyExpr::IsOwner);
        let m = mat();
        let shared = HashSet::new();
        let env = PolicyEnv {
            schema: &s,
            material: &m,
            user: "alice",
            shared: &shared,
        };
        let d = json!({});
        let mut rows = vec![("alice", d.clone()), ("mallory", d.clone())];
        let before = verify_drops_total();
        let dropped = post_filter(
            &env,
            &mut rows,
            |(o, d)| RowView {
                id: Uuid::nil(),
                owner: o,
                data: d,
            },
            Uuid::nil(),
        );
        assert_eq!(dropped, 1);
        assert_eq!(rows.len(), 1);
        assert!(verify_drops_total() > before, "警報の指標が増える");
    }
}
