#![allow(clippy::unwrap_used, clippy::many_single_char_names)]

use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::model::{FieldDef, FieldType};
use crate::policy::ast::RowPolicy;

/// applicant = f16（user_ref）/ dept = f17（text）。
fn schema(read: PolicyExpr) -> TableSchema {
    let mut s = TableSchema {
        fields: vec![
            FieldDef::new("applicant", FieldType::UserRef),
            FieldDef::new("dept", FieldType::Text),
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

fn material(roles: &[&str], admin: bool) -> PolicyMaterial {
    PolicyMaterial {
        roles_direct: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
        roles_effective: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
        role_members: HashMap::from([("admin".to_string(), admin)]),
        shared_record_ids: vec![],
        shares_truncated: false,
    }
}

/// `IsOwner ∨ applicant = $user ∨ dept ∈ $user.roles ∨ has_role(admin)`（＋個別共有）。
fn typical() -> TableSchema {
    schema(PolicyExpr::Any(vec![
        PolicyExpr::IsOwner,
        PolicyExpr::FieldCmp {
            field: "applicant".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        },
        PolicyExpr::FieldCmp {
            field: "dept".into(),
            op: CmpOp::In,
            value: PolicyOperand::UserRoles { subtree: false },
        },
        PolicyExpr::HasRole {
            role: "admin".into(),
            subtree: true,
        },
    ]))
}

#[test]
fn group_key_ignores_principal_and_shares() {
    let s = typical();
    let d = Decomposed::of(&s).unwrap();
    let m_sales = material(&["sales"], false);
    let m_dev = material(&["dev"], false);
    let shared_a: HashSet<Uuid> = [Uuid::new_v4()].into();
    let shared_b: HashSet<Uuid> = HashSet::new();
    let alice = Viewer {
        principal: "alice",
        material: &m_sales,
        shared: &shared_a,
    };
    let bob = Viewer {
        principal: "bob",
        material: &m_sales,
        shared: &shared_b,
    };
    let carol = Viewer {
        principal: "carol",
        material: &m_dev,
        shared: &shared_b,
    };
    let ka = group_plan(&d, &s, &alice).unwrap().key;
    let kb = group_plan(&d, &s, &bob).unwrap().key;
    let kc = group_plan(&d, &s, &carol).unwrap().key;
    assert_eq!(ka, kb, "principal id・共有集合は鍵に入らない");
    assert_ne!(ka, kc, "ロール構成が違えば別グループ");
    assert!(!ka.contains("alice") && !ka.contains("bob"));
    // has_role の定数も鍵に入る（admin とそれ以外は別グループ）。
    let m_admin = material(&["sales"], true);
    let admin = Viewer {
        principal: "dave",
        material: &m_admin,
        shared: &shared_b,
    };
    assert_ne!(group_plan(&d, &s, &admin).unwrap().key, ka);
}

#[test]
fn user_specific_leaves_are_evaluated_without_sql() {
    let s = typical();
    let d = Decomposed::of(&s).unwrap();
    let m = material(&["sales"], false);
    let id = Uuid::new_v4();
    let shared: HashSet<Uuid> = HashSet::new();
    let alice = Viewer {
        principal: "alice",
        material: &m,
        shared: &shared,
    };
    let row = |owner: &str, data: Value| RowState {
        id,
        owner: owner.into(),
        data,
    };
    // 共通葉（dept ∈ roles）が偽でも、owner / applicant が本人なら可視。
    assert!(visible(&d, &alice, &row("alice", json!({})), &[false]));
    assert!(visible(
        &d,
        &alice,
        &row("x", json!({"f16": "alice"})),
        &[false]
    ));
    assert!(!visible(
        &d,
        &alice,
        &row("x", json!({"f16": "bob"})),
        &[false]
    ));
    // 共通葉が真なら可視。
    assert!(visible(&d, &alice, &row("x", json!({})), &[true]));
    // 個別共有は record_id の集合判定。
    let shared: HashSet<Uuid> = [id].into();
    let alice_shared = Viewer {
        principal: "alice",
        material: &m,
        shared: &shared,
    };
    assert!(visible(&d, &alice_shared, &row("x", json!({})), &[false]));
    // 事後検査（独立した評価器）とも一致する（共通葉を SQL の代わりに Rust で与えた場合）。
    assert!(verify(&s, &alice_shared, &row("x", json!({}))));
    assert!(verify(&s, &alice, &row("x", json!({"f17": "sales"}))));
    assert!(!verify(&s, &alice, &row("x", json!({"f17": "dev"}))));
}

#[test]
fn ne_user_id_without_value_fails_closed_like_v1() {
    let s = schema(PolicyExpr::FieldCmp {
        field: "applicant".into(),
        op: CmpOp::Ne,
        value: PolicyOperand::UserId,
    });
    let d = Decomposed::of(&s).unwrap();
    let m = material(&[], false);
    let shared = HashSet::new();
    let v = Viewer {
        principal: "alice",
        material: &m,
        shared: &shared,
    };
    let row = |data: Value| RowState {
        id: Uuid::nil(),
        owner: "o".into(),
        data,
    };
    assert!(!visible(&d, &v, &row(json!({})), &[]), "値なしは不成立");
    assert!(!visible(&d, &v, &row(json!({"f16": "alice"})), &[]));
    assert!(visible(&d, &v, &row(json!({"f16": "bob"})), &[]));
    // 共通葉が無いので SQL は 1 本も組まれない。
    assert!(group_plan(&d, &s, &v).unwrap().sql.is_empty());
}

#[test]
fn no_policy_is_visible_to_all_viewers() {
    let mut s = typical();
    s.row_policy = None;
    let d = Decomposed::of(&s).unwrap();
    assert!(!d.has_policy());
    let m = material(&[], false);
    let shared = HashSet::new();
    let v = Viewer {
        principal: "u",
        material: &m,
        shared: &shared,
    };
    let r = RowState {
        id: Uuid::nil(),
        owner: "x".into(),
        data: json!({}),
    };
    assert!(visible(&d, &v, &r, &[]));
}

/// 作成者の列（D-122）の比べは、保存した値ではなく行の owner と比べる。保存形に作成者の列の値があっても
/// （作成者の列は保存しないので本来は無い）使わない。owner は空にならないので、`ne` は作成者以外で真。
#[test]
fn created_by_leaves_compare_the_row_owner() {
    let mut s = schema(PolicyExpr::FieldCmp {
        field: "made_by".into(),
        op: CmpOp::Ne,
        value: PolicyOperand::UserId,
    });
    s.fields
        .push(FieldDef::new("made_by", FieldType::CreatedBy));
    crate::field_key::assign_field_ids(None, &mut s).unwrap();
    let made_by = crate::field_key::json_key(s.field("made_by").unwrap().id);
    let d = Decomposed::of(&s).unwrap();
    let m = material(&[], false);
    let shared = HashSet::new();
    let v = Viewer {
        principal: "alice",
        material: &m,
        shared: &shared,
    };
    let row = |owner: &str, data: Value| RowState {
        id: Uuid::nil(),
        owner: owner.into(),
        data,
    };
    // 作成者が本人なら不成立（保存形に別の値があっても owner で決まる）。
    assert!(!visible(
        &d,
        &v,
        &row("alice", json!({ made_by.clone(): "bob" })),
        &[]
    ));
    assert!(!verify(
        &s,
        &v,
        &row("alice", json!({ made_by.clone(): "bob" }))
    ));
    assert!(visible(&d, &v, &row("bob", json!({})), &[]));
    assert!(verify(&s, &v, &row("bob", json!({}))));
    // 本人の比べなので SQL は組まない。
    assert!(group_plan(&d, &s, &v).unwrap().sql.is_empty());
    // リテラルとの比べは共通葉の SQL で、本体の owner を読む（保存形のキーは読まない）。
    s.row_policy = Some(RowPolicy {
        read: PolicyExpr::FieldCmp {
            field: "made_by".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::Lit(json!("bob")),
        },
        write: None,
    });
    let d = Decomposed::of(&s).unwrap();
    let plan = group_plan(&d, &s, &v).unwrap();
    assert_eq!(plan.sql.len(), 1);
    assert!(plan.sql[0].contains("r.owner"), "{:?}", plan.sql);
    assert!(!plan.sql[0].contains(&made_by), "{:?}", plan.sql);
}
