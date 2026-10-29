//! 「最初の状態を過ぎたら、申請者はレコードを書き換えられない」書き込みの決まり（D-115・BUG-110）。
//!
//! 状態の進め方の画面（GAP-44）が既定でオンにする決まりを、行の決まり（`row_policy.write`）の 1 つの形として持つ。
//!
//! ```text
//! write = all[ 元の書き込みの決まり, any[ 状態 = 最初の状態, 申請者 ≠ 自分 ] ]
//! ```
//!
//! - 行の決まりに `Not` は足さない（data-platform §5.1 の意図した非対称）。「申請者でない」は申請者の列
//!   （`user_ref`）と `ne $user.id` の比べで書く（経費のテンプレート expense_policy の進め方と同じ）。
//! - 元の書き込みの決まり（`write`。無ければ `read`。行の決まりが無ければ `public`）は外側の `all` の先頭に
//!   そのまま残すので、決まりを外すと元に戻る（[`without_guard`]）。
//! - 値なしの `ne` は偽（行述語の v1 の意味論）なので、申請者の列が空のレコードは、最初の状態を過ぎると
//!   この決まりでは誰も書き換えられない（fail-closed）。
//! - 書き込みの決まりは書き換え・削除に効く（状態を進めるのは状態の進め方の「進められる人」が決める）。

use serde_json::Value;

use crate::policy::{CmpOp, PolicyExpr, PolicyOperand, RowPolicy};

/// 決まりの後半（`any[ 状態 = 最初の状態, 申請者 ≠ 自分 ]`）。
fn guard(status_field: &str, initial: &str, applicant: &str) -> PolicyExpr {
    PolicyExpr::Any(vec![
        PolicyExpr::FieldCmp {
            field: status_field.to_string(),
            op: CmpOp::Eq,
            value: PolicyOperand::Lit(Value::String(initial.to_string())),
        },
        PolicyExpr::FieldCmp {
            field: applicant.to_string(),
            op: CmpOp::Ne,
            value: PolicyOperand::UserId,
        },
    ])
}

/// `expr` がこの決まりの後半の形なら、申請者の列（最初の状態の値は問わない・保存のたびに今の値で組み直す）。
fn guard_applicant<'a>(expr: &'a PolicyExpr, status_field: &str) -> Option<&'a str> {
    let PolicyExpr::Any(v) = expr else {
        return None;
    };
    match v.as_slice() {
        [PolicyExpr::FieldCmp {
            field: s,
            op: CmpOp::Eq,
            value: PolicyOperand::Lit(Value::String(_)),
        }, PolicyExpr::FieldCmp {
            field: a,
            op: CmpOp::Ne,
            value: PolicyOperand::UserId,
        }] if s == status_field => Some(a.as_str()),
        _ => None,
    }
}

/// 書き込みの決まりがこの決まりの形なら、（元の書き込みの決まり, 申請者の列）。
fn split(policy: &RowPolicy, status_field: &str) -> Option<(PolicyExpr, String)> {
    let PolicyExpr::All(v) = policy.write.as_ref()? else {
        return None;
    };
    match v.as_slice() {
        [base, tail] => guard_applicant(tail, status_field).map(|a| (base.clone(), a.to_string())),
        _ => None,
    }
}

/// いまの行の決まりで、この決まりが使っている申請者の列（オフなら `None`）。
#[must_use]
pub(crate) fn applicant_write_lock(
    policy: Option<&RowPolicy>,
    status_field: &str,
) -> Option<String> {
    policy.and_then(|p| split(p, status_field)).map(|(_, a)| a)
}

/// この決まりを（組み直して）付けた行の決まり。元の書き込みの決まりは保つ。
#[must_use]
pub(crate) fn with_guard(
    policy: Option<&RowPolicy>,
    status_field: &str,
    initial: &str,
    applicant: &str,
) -> RowPolicy {
    let read = policy.map_or(PolicyExpr::Public, |p| p.read.clone());
    let base = match policy {
        Some(p) => split(p, status_field).map_or_else(|| p.write_expr().clone(), |(b, _)| b),
        None => PolicyExpr::Public,
    };
    RowPolicy {
        read,
        write: Some(PolicyExpr::All(vec![
            base,
            guard(status_field, initial, applicant),
        ])),
    }
}

/// この決まりを外した行の決まり（付ける前の形に戻す・付いていなければそのまま）。
#[must_use]
pub(crate) fn without_guard(policy: Option<&RowPolicy>, status_field: &str) -> Option<RowPolicy> {
    let p = policy?;
    let Some((base, _)) = split(p, status_field) else {
        return Some(p.clone());
    };
    // 元の書き込みの決まりが読む決まりと同じなら、書き込みの決まりは無かった（read を流用していた）。
    let write = (base != p.read).then_some(base);
    if write.is_none() && p.read == PolicyExpr::Public {
        // 行の決まりが無かったテーブル（付けるときに read = public で作った）。
        return None;
    }
    Some(RowPolicy {
        read: p.read.clone(),
        write,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own() -> PolicyExpr {
        PolicyExpr::IsOwner
    }

    /// 行の決まりの無いテーブルに付けて外すと、行の決まりの無い形に戻る。最初の状態を変えて付け直すと、
    /// 今の最初の状態で組み直す（元の決まりを二重に包まない）。
    #[test]
    fn round_trips_on_a_table_without_row_policy() {
        let p = with_guard(None, "status", "申請中", "applicant");
        assert_eq!(p.read, PolicyExpr::Public);
        assert_eq!(
            applicant_write_lock(Some(&p), "status").as_deref(),
            Some("applicant")
        );
        assert_eq!(without_guard(Some(&p), "status"), None);
        let again = with_guard(Some(&p), "status", "下書き", "applicant");
        assert_eq!(
            again.write,
            Some(PolicyExpr::All(vec![
                PolicyExpr::Public,
                guard("status", "下書き", "applicant"),
            ]))
        );
    }

    /// 元の書き込みの決まり（read の流用・明示の write）を保ち、外すと元の形に戻る。
    #[test]
    fn keeps_the_original_write_rule() {
        let read_only = RowPolicy {
            read: own(),
            write: None,
        };
        let p = with_guard(Some(&read_only), "status", "申請中", "applicant");
        assert_eq!(p.read, own());
        assert_eq!(without_guard(Some(&p), "status"), Some(read_only));
        let explicit = RowPolicy {
            read: PolicyExpr::Public,
            write: Some(own()),
        };
        let p = with_guard(Some(&explicit), "status", "申請中", "applicant");
        assert_eq!(without_guard(Some(&p), "status"), Some(explicit));
    }

    /// 形の違う書き込みの決まり（チャットや API で書いたもの）は、この決まりとは読まない。外しても変えない。
    #[test]
    fn other_shapes_are_not_the_guard() {
        let other = RowPolicy {
            read: PolicyExpr::Public,
            write: Some(PolicyExpr::All(vec![own(), PolicyExpr::Public])),
        };
        assert_eq!(applicant_write_lock(Some(&other), "status"), None);
        assert_eq!(without_guard(Some(&other), "status"), Some(other.clone()));
        // 別の状態の列で書いた決まりも、この列の決まりとは読まない。
        let p = with_guard(None, "phase", "申請中", "applicant");
        assert_eq!(applicant_write_lock(Some(&p), "status"), None);
    }
}
