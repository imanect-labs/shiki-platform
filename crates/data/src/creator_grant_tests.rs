#![allow(clippy::unwrap_used)]

use super::*;
use crate::model::{FieldDef, FieldType};
use crate::policy::RowPolicy;
use serde_json::json;

fn cmp(field: &str, op: CmpOp) -> PolicyExpr {
    PolicyExpr::FieldCmp {
        field: field.into(),
        op,
        value: PolicyOperand::UserId,
    }
}

fn status_is(s: &str) -> PolicyExpr {
    PolicyExpr::FieldCmp {
        field: "status".into(),
        op: CmpOp::Eq,
        value: PolicyOperand::Lit(json!(s)),
    }
}

fn schema(write: Option<PolicyExpr>) -> TableSchema {
    let mut status = FieldDef::new("status", FieldType::Select);
    status.options = vec!["申請中".into(), "承認".into(), "差し戻し".into()];
    TableSchema {
        fields: vec![
            FieldDef::new("made_by", FieldType::CreatedBy),
            FieldDef::new("applicant", FieldType::UserRef),
            FieldDef::new("approver", FieldType::UserRef),
            status,
        ],
        status_field: Some("status".into()),
        row_policy: write.map(|w| RowPolicy {
            read: PolicyExpr::Public,
            write: Some(w),
        }),
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

fn mv(from: &str, to: &str, actor: PolicyExpr) -> FsmTransition {
    FsmTransition {
        from: from.into(),
        to: to.into(),
        actor,
    }
}

/// 差し戻し → 申請中 を作成者に、申請中 → 承認・差し戻し を承認者（本人は除く）に許す進め方。
fn resubmit_flow() -> FsmBody {
    let approver = PolicyExpr::All(vec![cmp("approver", CmpOp::Eq), cmp("made_by", CmpOp::Ne)]);
    FsmBody {
        states: vec!["申請中".into(), "承認".into(), "差し戻し".into()],
        transitions: vec![
            mv("申請中", "承認", approver.clone()),
            mv("申請中", "差し戻し", approver),
            mv("差し戻し", "申請中", cmp("made_by", CmpOp::Eq)),
        ],
    }
}

fn is_made_by(f: &str) -> bool {
    f == "made_by"
}

/// 作成者の列 = 本人の比べだけを取り出す。is_owner・メンバーの列・public からは取り出さない。
#[test]
fn creator_part_takes_only_the_branches_through_the_creator_column() {
    let me = cmp("made_by", CmpOp::Eq);
    assert_eq!(creator_part(&me, &is_made_by), Some(me.clone()));
    for other in [
        PolicyExpr::IsOwner,
        PolicyExpr::Public,
        cmp("applicant", CmpOp::Eq),
        cmp("made_by", CmpOp::Ne),
        PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: true,
        },
    ] {
        assert_eq!(creator_part(&other, &is_made_by), None, "{other:?}");
    }
    // any: 取り出せた枝だけ。
    let any = PolicyExpr::Any(vec![
        PolicyExpr::Public,
        me.clone(),
        cmp("approver", CmpOp::Eq),
    ]);
    assert_eq!(creator_part(&any, &is_made_by), Some(me.clone()));
    // all: 作成者を指す子があれば全体。
    let all = PolicyExpr::All(vec![status_is("差し戻し"), me.clone()]);
    assert_eq!(creator_part(&all, &is_made_by), Some(all.clone()));
    // all の子の any から取り出すと、その子を置き換えた all。
    let nested = PolicyExpr::All(vec![
        PolicyExpr::Any(vec![me.clone(), cmp("approver", CmpOp::Eq)]),
        status_is("差し戻し"),
    ]);
    assert_eq!(
        creator_part(&nested, &is_made_by),
        Some(PolicyExpr::All(vec![me.clone(), status_is("差し戻し")]))
    );
    // どの子からも取り出せない all は取り出さない。
    let none = PolicyExpr::All(vec![cmp("approver", CmpOp::Eq), status_is("差し戻し")]);
    assert_eq!(creator_part(&none, &is_made_by), None);
}

/// 状態と作成者だけで決まる式は決め、分からない値は成り立ちうるとして扱う。
#[test]
fn may_hold_decides_by_state_and_creator_only() {
    let both = PolicyExpr::All(vec![cmp("made_by", CmpOp::Eq), cmp("made_by", CmpOp::Ne)]);
    assert!(
        !may_hold(&both, "status", "差し戻し", &is_made_by),
        "作成者でありつつ作成者でない"
    );
    let s = PolicyExpr::All(vec![status_is("差し戻し"), cmp("made_by", CmpOp::Eq)]);
    assert!(may_hold(&s, "status", "差し戻し", &is_made_by));
    assert!(!may_hold(&s, "status", "承認", &is_made_by));
    let listed = PolicyExpr::FieldCmp {
        field: "status".into(),
        op: CmpOp::In,
        value: PolicyOperand::Lit(json!(["差し戻し", "下書き"])),
    };
    assert!(may_hold(&listed, "status", "下書き", &is_made_by));
    assert!(!may_hold(&listed, "status", "承認", &is_made_by));
    // 部署・メンバーの列は分からない（成り立ちうる）。
    let unknown = PolicyExpr::All(vec![
        cmp("approver", CmpOp::Eq),
        PolicyExpr::HasRole {
            role: "x".into(),
            subtree: false,
        },
    ]);
    assert!(may_hold(&unknown, "status", "承認", &is_made_by));
}

/// 画面の手がかり: 作成者が進められるのは差し戻しから。書き換えは書き込みの決まりが作成者を許す状態だけ。
#[test]
fn rights_follow_the_moves_and_the_write_rule() {
    let flow = resubmit_flow();
    // 書き込みの決まりが無い: 進められるが、直せない。
    let r = rights_of(&schema(None), &flow, "status").unwrap();
    assert_eq!(r.move_from, vec!["差し戻し".to_string()]);
    assert!(r.edit_in.is_empty());
    // 差し戻しのあいだ作成者の書き換えを許す決まり（画面の決まりと同じ形）。
    let guard = PolicyExpr::All(vec![
        PolicyExpr::Public,
        PolicyExpr::Any(vec![
            status_is("申請中"),
            cmp("made_by", CmpOp::Ne),
            PolicyExpr::All(vec![
                PolicyExpr::FieldCmp {
                    field: "status".into(),
                    op: CmpOp::In,
                    value: PolicyOperand::Lit(json!(["差し戻し"])),
                },
                cmp("made_by", CmpOp::Eq),
            ]),
        ]),
    ]);
    let r = rights_of(&schema(Some(guard.clone())), &flow, "status").unwrap();
    assert_eq!(r.edit_in, vec!["差し戻し".to_string()]);
    // 書き込みの決まりが申請中だけ作成者を許す: 進められる差し戻しでは直せない。
    let initial_only = PolicyExpr::All(vec![status_is("申請中"), cmp("made_by", CmpOp::Eq)]);
    let r = rights_of(&schema(Some(initial_only)), &flow, "status").unwrap();
    assert!(r.edit_in.is_empty(), "{r:?}");
    // 作成者に許した進め方が無い（is_owner・メンバーの列・本人を除く作成者）: 何も許さない。
    for actor in [
        PolicyExpr::IsOwner,
        cmp("applicant", CmpOp::Eq),
        PolicyExpr::All(vec![cmp("made_by", CmpOp::Eq), cmp("made_by", CmpOp::Ne)]),
    ] {
        let mut f = flow.clone();
        f.transitions[2].actor = actor.clone();
        assert_eq!(
            rights_of(&schema(Some(guard.clone())), &f, "status"),
            None,
            "{actor:?}"
        );
    }
}

/// 書き込みの決まりが作成者の書き換えを許す状態（最初の状態を除く・作成者を本人で除く進め方は数えない）。
#[test]
fn resubmit_states_are_where_the_applicant_can_move() {
    let flow = resubmit_flow();
    assert_eq!(
        resubmit_states(&flow, "status", "made_by"),
        vec!["差し戻し".to_string()]
    );
    // 申請者の列が別の列なら、その列で指す進め方だけ。
    assert!(resubmit_states(&flow, "status", "applicant").is_empty());
    // 最初の状態から作成者が進める進め方（取り下げ）は数えない（最初の状態は前から書き換えられる）。
    let f = FsmBody {
        transitions: vec![mv("申請中", "差し戻し", cmp("made_by", CmpOp::Eq))],
        ..flow
    };
    assert!(resubmit_states(&f, "status", "made_by").is_empty());
}
