//! 「最初の状態を過ぎたら、申請者はレコードを書き換えられない」書き込みの決まり（D-115・BUG-110）。
//!
//! 状態の進め方の画面（GAP-44）が既定でオンにする決まりを、行の決まり（`row_policy.write`）の 1 つの形として持つ。
//!
//! ```text
//! write = all[ 元の書き込みの決まり, any[ 状態 = 最初の状態, 申請者 ≠ 自分 ] ]
//! write = all[ 元の書き込みの決まり, any[ 状態 = 最初の状態, 申請者 ≠ 自分, all[ 状態 in 出し直す状態, 申請者 = 自分 ] ] ]
//! ```
//!
//! 2 つ目の形は、申請者が進められる進め方（例: 差し戻し → 申請中 を「作成者」に許す）があるときに付ける
//! （出し直す状態は、その進め方の元の状態・最初の状態を除く・[`crate::creator_grant::resubmit_states`]）。申請者は
//! 差し戻しのあいだ自分の申請を直して出し直せる（D-127）。申請者の列が作成者の列なら、この枝は作成者の列で本人を
//! 指すので、「追加だけ」の人にも差し戻しのあいだの書き換えを許す（[`crate::creator_grant`]）。出し直す状態は保存の
//! たびに進め方から組み直す（進め方を変えれば決まりも変わる）。
//!
//! - 行の決まりに `Not` は足さない（data-platform §5.1 の意図した非対称）。「申請者でない」は申請者の列
//!   （作成者の列 `created_by` か `user_ref`）と `ne $user.id` の比べで書く（経費のテンプレート expense_policy の
//!   進め方と同じ）。作成者の列はレコードの作成者（だれも書き換えられない）と比べるので、申請者の列を
//!   ほかの人にしてすり抜ける経路が無い（D-122・BUG-148）。
//! - 元の書き込みの決まり（`write`。無ければ `read`。行の決まりが無ければ `public`）は外側の `all` の先頭に
//!   そのまま残すので、決まりを外すと元に戻る（[`without_guard`]）。
//! - 値なしの `ne` は偽（行述語の v1 の意味論）なので、申請者の列（`user_ref`）が空のレコードは、最初の状態を
//!   過ぎるとこの決まりでは誰も書き換えられない（fail-closed）。作成者の列は空にならない。
//! - 書き込みの決まりは書き換え・削除に効く（状態を進めるのは状態の進め方の「進められる人」が決める）。

use serde_json::Value;

use crate::policy::{CmpOp, PolicyExpr, PolicyOperand, RowPolicy};

/// 決まりの後半（`any[ 状態 = 最初の状態, 申請者 ≠ 自分 (, all[ 状態 in 出し直す状態, 申請者 = 自分 ]) ]`）。
fn guard(status_field: &str, initial: &str, applicant: &str, resubmit: &[String]) -> PolicyExpr {
    let mut v = vec![
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
    ];
    if !resubmit.is_empty() {
        v.push(PolicyExpr::All(vec![
            PolicyExpr::FieldCmp {
                field: status_field.to_string(),
                op: CmpOp::In,
                value: PolicyOperand::Lit(Value::Array(
                    resubmit.iter().cloned().map(Value::String).collect(),
                )),
            },
            PolicyExpr::FieldCmp {
                field: applicant.to_string(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
        ]));
    }
    PolicyExpr::Any(v)
}

/// 出し直す状態の枝（`all[ 状態 in […], 申請者 = 自分 ]`）なら、その状態。
fn resubmit_part<'a>(
    expr: &'a PolicyExpr,
    status_field: &str,
    applicant: &str,
) -> Option<&'a [Value]> {
    let PolicyExpr::All(v) = expr else {
        return None;
    };
    match v.as_slice() {
        [PolicyExpr::FieldCmp {
            field: s,
            op: CmpOp::In,
            value: PolicyOperand::Lit(Value::Array(states)),
        }, PolicyExpr::FieldCmp {
            field: a,
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        }] if s == status_field && a == applicant && states.iter().all(Value::is_string) => {
            Some(states.as_slice())
        }
        _ => None,
    }
}

/// `expr` がこの決まりの後半の形なら、（最初の状態, 申請者の列）。最初の状態・出し直す状態の値は形の判定では
/// 問わない（保存のたびに今の値で組み直す）。
fn guard_parts<'a>(expr: &'a PolicyExpr, status_field: &str) -> Option<(&'a str, &'a str)> {
    let PolicyExpr::Any(v) = expr else {
        return None;
    };
    let (head, rest) = v.split_at_checked(2)?;
    let parts = match head {
        [PolicyExpr::FieldCmp {
            field: s,
            op: CmpOp::Eq,
            value: PolicyOperand::Lit(Value::String(initial)),
        }, PolicyExpr::FieldCmp {
            field: a,
            op: CmpOp::Ne,
            value: PolicyOperand::UserId,
        }] if s == status_field => (initial.as_str(), a.as_str()),
        _ => return None,
    };
    match rest {
        [] => Some(parts),
        [r] if resubmit_part(r, status_field, parts.1).is_some() => Some(parts),
        _ => None,
    }
}

/// 書き込みの決まりがこの決まりの形なら、（元の書き込みの決まり, 最初の状態, 申請者の列）。
fn split<'a>(
    policy: &'a RowPolicy,
    status_field: &str,
) -> Option<(&'a PolicyExpr, &'a str, &'a str)> {
    let PolicyExpr::All(v) = policy.write.as_ref()? else {
        return None;
    };
    match v.as_slice() {
        [base, tail] => guard_parts(tail, status_field).map(|(i, a)| (base, i, a)),
        _ => None,
    }
}

/// いまの行の決まりで付いているこの決まりの（最初の状態, 申請者の列）（オフなら `None`）。
#[must_use]
pub(crate) fn current_guard(
    policy: Option<&RowPolicy>,
    status_field: &str,
) -> Option<(String, String)> {
    policy
        .and_then(|p| split(p, status_field))
        .map(|(_, i, a)| (i.to_string(), a.to_string()))
}

/// いまの行の決まりで、この決まりが使っている申請者の列（オフなら `None`）。
#[must_use]
pub(crate) fn applicant_write_lock(
    policy: Option<&RowPolicy>,
    status_field: &str,
) -> Option<String> {
    policy
        .and_then(|p| split(p, status_field))
        .map(|(_, _, a)| a.to_string())
}

/// この決まりを（組み直して）付けた行の決まり。元の書き込みの決まりは保つ。`resubmit` は申請者が直して出し直せる
/// 状態（空なら最初の状態のあいだだけ）。
#[must_use]
pub(crate) fn with_guard(
    policy: Option<&RowPolicy>,
    status_field: &str,
    initial: &str,
    applicant: &str,
    resubmit: &[String],
) -> RowPolicy {
    let read = policy.map_or(PolicyExpr::Public, |p| p.read.clone());
    let base = match policy {
        Some(p) => {
            split(p, status_field).map_or_else(|| p.write_expr().clone(), |(b, _, _)| b.clone())
        }
        None => PolicyExpr::Public,
    };
    RowPolicy {
        read,
        write: Some(PolicyExpr::All(vec![
            base,
            guard(status_field, initial, applicant, resubmit),
        ])),
    }
}

/// この決まりを外した行の決まり（付ける前の形に戻す・付いていなければそのまま）。
#[must_use]
pub(crate) fn without_guard(policy: Option<&RowPolicy>, status_field: &str) -> Option<RowPolicy> {
    let p = policy?;
    let Some((base, _, _)) = split(p, status_field) else {
        return Some(p.clone());
    };
    // 元の書き込みの決まりが読む決まりと同じなら、書き込みの決まりは無かった（read を流用していた）。
    let write = (*base != p.read).then(|| base.clone());
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
#[allow(clippy::panic)]
mod tests {
    use super::*;

    fn own() -> PolicyExpr {
        PolicyExpr::IsOwner
    }

    /// 行の決まりの無いテーブルに付けて外すと、行の決まりの無い形に戻る。最初の状態を変えて付け直すと、
    /// 今の最初の状態で組み直す（元の決まりを二重に包まない）。
    #[test]
    fn round_trips_on_a_table_without_row_policy() {
        let p = with_guard(None, "status", "申請中", "applicant", &[]);
        assert_eq!(p.read, PolicyExpr::Public);
        assert_eq!(
            applicant_write_lock(Some(&p), "status").as_deref(),
            Some("applicant")
        );
        assert_eq!(without_guard(Some(&p), "status"), None);
        assert_eq!(
            current_guard(Some(&p), "status"),
            Some(("申請中".to_string(), "applicant".to_string()))
        );
        assert_eq!(current_guard(None, "status"), None);
        let again = with_guard(Some(&p), "status", "下書き", "applicant", &[]);
        assert_eq!(
            again.write,
            Some(PolicyExpr::All(vec![
                PolicyExpr::Public,
                guard("status", "下書き", "applicant", &[]),
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
        let p = with_guard(Some(&read_only), "status", "申請中", "applicant", &[]);
        assert_eq!(p.read, own());
        assert_eq!(without_guard(Some(&p), "status"), Some(read_only));
        let explicit = RowPolicy {
            read: PolicyExpr::Public,
            write: Some(own()),
        };
        let p = with_guard(Some(&explicit), "status", "申請中", "applicant", &[]);
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
        let p = with_guard(None, "phase", "申請中", "applicant", &[]);
        assert_eq!(applicant_write_lock(Some(&p), "status"), None);
    }

    /// 出し直す状態のある決まり（D-127）も同じ決まりと読み、付け直すと今の出し直す状態で組み直す。外すと元に戻る。
    #[test]
    fn resubmit_branch_round_trips() {
        let back = vec!["差し戻し".to_string()];
        let p = with_guard(None, "status", "申請中", "made_by", &back);
        let Some(PolicyExpr::All(v)) = &p.write else {
            panic!("{p:?}");
        };
        assert_eq!(v[1], guard("status", "申請中", "made_by", &back));
        let PolicyExpr::Any(branches) = &v[1] else {
            panic!("{v:?}");
        };
        assert_eq!(
            branches[2],
            PolicyExpr::All(vec![
                PolicyExpr::FieldCmp {
                    field: "status".into(),
                    op: CmpOp::In,
                    value: PolicyOperand::Lit(serde_json::json!(["差し戻し"])),
                },
                PolicyExpr::FieldCmp {
                    field: "made_by".into(),
                    op: CmpOp::Eq,
                    value: PolicyOperand::UserId,
                },
            ])
        );
        assert_eq!(
            current_guard(Some(&p), "status"),
            Some(("申請中".to_string(), "made_by".to_string()))
        );
        // 出し直す状態が無くなれば 2 つの枝に戻す（元の書き込みの決まりは二重に包まない）。
        let again = with_guard(Some(&p), "status", "申請中", "made_by", &[]);
        assert_eq!(
            again.write,
            Some(PolicyExpr::All(vec![
                PolicyExpr::Public,
                guard("status", "申請中", "made_by", &[]),
            ]))
        );
        assert_eq!(without_guard(Some(&p), "status"), None);
        // 出し直す枝の申請者の列が違うものは、この決まりとは読まない。
        let mut other = p.clone();
        if let Some(PolicyExpr::All(v)) = &mut other.write {
            if let PolicyExpr::Any(b) = &mut v[1] {
                b[2] = PolicyExpr::All(vec![
                    PolicyExpr::FieldCmp {
                        field: "status".into(),
                        op: CmpOp::In,
                        value: PolicyOperand::Lit(serde_json::json!(["差し戻し"])),
                    },
                    PolicyExpr::FieldCmp {
                        field: "approver".into(),
                        op: CmpOp::Eq,
                        value: PolicyOperand::UserId,
                    },
                ]);
            }
        }
        assert_eq!(applicant_write_lock(Some(&other), "status"), None);
    }
}
