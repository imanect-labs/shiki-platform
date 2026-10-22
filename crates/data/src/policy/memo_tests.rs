//! リクエストスコープの認可メモのキー契約（Task 15.9・PIT-18）。
//!
//! 偽の authz（呼ばれた回数を数え、主体ごとに答えを変える）で、
//! - 同じリクエスト・同じ問いは 2 度引かない
//! - キーの各成分（tenant / principal id / 種別 / org / 式集合 / 共有集合の要否 / object）が違えば引き直す
//! - 新しいリクエスト（新しいメモ）は剥奪後の答えを引く
//!
//! を固定する。

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use authz::{AuthzError, Principal, Subject};
use uuid::Uuid;

use super::*;
use crate::policy::ast::{CmpOp, PolicyOperand};
use crate::policy::material;

/// 主体ごとの答えを持つ偽 authz。
#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
    batches: AtomicUsize,
    /// `(subject, object)` の check が真になる組（relation は区別しない）。
    allow: Mutex<HashSet<(String, String)>>,
    /// `(subject, object)` の直接タプル（read_subject_objects 用）。
    direct: Mutex<HashSet<(String, String)>>,
}

impl Fake {
    fn grant(&self, subject: &Subject, object: &FgaObject) {
        self.allow
            .lock()
            .unwrap()
            .insert((subject.to_string(), object.as_str().to_string()));
    }
    fn revoke(&self, subject: &Subject, object: &FgaObject) {
        self.allow
            .lock()
            .unwrap()
            .remove(&(subject.to_string(), object.as_str().to_string()));
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn allowed(&self, s: &Subject, o: &FgaObject) -> bool {
        self.allow
            .lock()
            .unwrap()
            .contains(&(s.to_string(), o.as_str().to_string()))
    }
    fn objects_of(
        set: &Mutex<HashSet<(String, String)>>,
        s: &Subject,
        ty: ObjectType,
    ) -> Vec<String> {
        let prefix = format!("{}:", ty.as_str());
        set.lock()
            .unwrap()
            .iter()
            .filter(|(sub, o)| *sub == s.to_string() && o.starts_with(&prefix))
            .map(|(_, o)| o.clone())
            .collect()
    }
}

#[async_trait]
impl AuthzClient for Fake {
    async fn check(
        &self,
        s: &Subject,
        _r: Relation,
        o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.allowed(s, o))
    }
    async fn batch_check(
        &self,
        items: &[CheckTuple],
        _c: Consistency,
    ) -> Result<Vec<bool>, AuthzError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batches.fetch_add(1, Ordering::SeqCst);
        Ok(items
            .iter()
            .map(|i| self.allowed(&i.subject, &i.object))
            .collect())
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        s: &Subject,
        _r: Relation,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Self::objects_of(&self.allow, s, t))
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        s: &Subject,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Self::objects_of(&self.direct, s, t))
    }
}

fn ctx_of(kind: PrincipalKind, tenant: &str, org: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        tenant.into(),
    )
}

fn base() -> AuthContext {
    ctx_of(PrincipalKind::User, "t1", "o1", "u1")
}

fn has_role(role: &str, subtree: bool) -> PolicyExpr {
    PolicyExpr::HasRole {
        role: role.into(),
        subtree,
    }
}

#[tokio::test]
async fn same_request_does_not_ask_twice() {
    let fake = Fake::default();
    let ctx = base();
    fake.grant(&ctx.subject(), &ctx.ns().role("mgr"));
    let az = AuthzMemo::new(&fake);
    let mgr = has_role("mgr", true);
    let m1 = material::resolve(&ctx, &az, &[&mgr]).await.unwrap();
    let after_first = fake.calls();
    // 名指しロール 1 問（check）＋共有集合（list_objects）。
    assert_eq!(after_first, 2);
    let m2 = material::resolve(&ctx, &az, &[&mgr]).await.unwrap();
    assert_eq!(
        fake.calls(),
        after_first,
        "同じリクエストの同じ問いは引き直さない"
    );
    assert!(m1.has_role("mgr", true) && m2.has_role("mgr", true));
    // 式の順序・重複は同じ集合（同じキー）。
    let other = has_role("x", true);
    material::resolve(&ctx, &az, &[&mgr, &other]).await.unwrap();
    let n = fake.calls();
    material::resolve(&ctx, &az, &[&other, &mgr, &mgr])
        .await
        .unwrap();
    assert_eq!(fake.calls(), n);
    // ロールだけの材料（マスク用）は共有集合を引かないが、ロールの答えはメモから使う。
    let roles_only = material::resolve_roles(&ctx, &az, &[&mgr]).await.unwrap();
    assert_eq!(fake.calls(), n);
    assert!(roles_only.shared_record_ids.is_empty());
}

#[tokio::test]
async fn every_context_part_is_in_the_key() {
    let fake = Fake::default();
    let b = base();
    // 基準の主体だけが mgr の member で、共有行も 1 件持つ。
    fake.grant(&b.subject(), &b.ns().role("mgr"));
    let rec = Uuid::new_v4();
    fake.grant(&b.subject(), &b.ns().data_record(&rec.to_string()));
    let az = AuthzMemo::new(&fake);
    let mgr = has_role("mgr", true);
    let m = material::resolve(&b, &az, &[&mgr]).await.unwrap();
    assert!(m.has_role("mgr", true));
    assert_eq!(m.shared_record_ids, vec![rec]);

    let variants = [
        ("tenant", ctx_of(PrincipalKind::User, "t2", "o1", "u1")),
        ("principal", ctx_of(PrincipalKind::User, "t1", "o1", "u2")),
        ("kind", ctx_of(PrincipalKind::Workflow, "t1", "o1", "u1")),
        (
            "kind-miniapp",
            ctx_of(PrincipalKind::MiniApp, "t1", "o1", "u1"),
        ),
        ("org", ctx_of(PrincipalKind::User, "t1", "o2", "u1")),
    ];
    for (what, v) in variants {
        assert!(
            az.material(&MaterialKey::new(&v, &[&mgr], true)).is_none(),
            "{what}: 別の主体の材料がキーで引けてはならない"
        );
        let before = fake.calls();
        let vm = material::resolve(&v, &az, &[&mgr]).await.unwrap();
        assert!(fake.calls() > before, "{what}: 別キーは OpenFGA へ引き直す");
        if what == "org" {
            // 同じ subject なので答えは同じ（ただしメモは共有しない）。
            assert!(vm.has_role("mgr", true));
        } else {
            assert!(!vm.has_role("mgr", true), "{what}: 材料が混ざらない");
            assert!(
                vm.shared_record_ids.is_empty(),
                "{what}: 共有集合が混ざらない"
            );
        }
    }
    // 基準の答えはそのまま。
    let again = material::resolve(&b, &az, &[&mgr]).await.unwrap();
    assert!(again.has_role("mgr", true));
}

#[tokio::test]
async fn expression_set_is_in_the_key() {
    let fake = Fake::default();
    let ctx = base();
    // hr にだけ所属（配下込み）・直接所属は lead だけ。
    fake.grant(&ctx.subject(), &ctx.ns().role("hr"));
    fake.direct.lock().unwrap().insert((
        ctx.subject().to_string(),
        ctx.ns().role("lead").as_str().to_string(),
    ));
    let az = AuthzMemo::new(&fake);
    let a = material::resolve(&ctx, &az, &[&has_role("mgr", true)])
        .await
        .unwrap();
    assert!(!a.has_role("mgr", true));
    // 別の式（別ロール）で a の材料を流用すると hr の判定が無く不成立になる＝流用していない。
    let b = material::resolve(&ctx, &az, &[&has_role("hr", true)])
        .await
        .unwrap();
    assert!(b.has_role("hr", true));
    // 直接所属の式は集合を引く（配下込みの名指し判定を流用しない）。
    let c = material::resolve(&ctx, &az, &[&has_role("lead", false)])
        .await
        .unwrap();
    assert!(c.has_role("lead", false));
    assert!(c.roles_direct.is_some() && a.roles_direct.is_none());
    // `$user.roles` は実効集合そのものを引く。
    let cmp = PolicyExpr::FieldCmp {
        field: "dept".into(),
        op: CmpOp::In,
        value: PolicyOperand::UserRoles { subtree: true },
    };
    let d = material::resolve(&ctx, &az, &[&cmp]).await.unwrap();
    assert_eq!(d.roles(true), ["hr".to_string()]);
    assert!(a.roles_effective.is_none());
    // 共有集合の要否もキー（ロールだけの材料を行述語へ流用しない）。
    let roles_only = material::resolve_roles(&ctx, &az, &[&has_role("hr", true)])
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&roles_only, &b));
}

#[tokio::test]
async fn different_objects_are_different_questions() {
    let fake = Fake::default();
    let ctx = base();
    let (ta, tb) = (Uuid::new_v4(), Uuid::new_v4());
    fake.grant(&ctx.subject(), &ctx.ns().data_table(&ta.to_string()));
    let az = AuthzMemo::new(&fake);
    let a = ctx.ns().data_table(&ta.to_string());
    let b = ctx.ns().data_table(&tb.to_string());
    // 未知の 2 問は BatchCheck 1 往復。
    let got = az
        .check_all(
            &ctx,
            &[(Relation::Viewer, a.clone()), (Relation::Viewer, b.clone())],
        )
        .await
        .unwrap();
    assert_eq!(got, vec![true, false]);
    assert_eq!((fake.calls(), fake.batches.load(Ordering::SeqCst)), (1, 1));
    // 覚えた答えは別テーブルどうしで混ざらない。
    assert!(az.check(&ctx, Relation::Viewer, &a).await.unwrap());
    assert!(!az.check(&ctx, Relation::Viewer, &b).await.unwrap());
    assert_eq!(fake.calls(), 1);
    // relation が違えば別の問い。
    az.check(&ctx, Relation::Editor, &a).await.unwrap();
    assert_eq!(fake.calls(), 2);
    // 既知と未知が混ざれば未知の分だけ引く。
    let c = ctx.ns().data_table(&Uuid::new_v4().to_string());
    az.check_all(
        &ctx,
        &[(Relation::Viewer, a.clone()), (Relation::Viewer, c)],
    )
    .await
    .unwrap();
    assert_eq!(fake.calls(), 3);
}

#[tokio::test]
async fn next_request_sees_revocation() {
    let fake = Fake::default();
    let ctx = base();
    let role = ctx.ns().role("mgr");
    let table = ctx.ns().data_table(&Uuid::new_v4().to_string());
    fake.grant(&ctx.subject(), &role);
    fake.grant(&ctx.subject(), &table);
    let mgr = has_role("mgr", true);
    {
        let req1 = AuthzMemo::new(&fake);
        assert!(material::resolve(&ctx, &req1, &[&mgr])
            .await
            .unwrap()
            .has_role("mgr", true));
        assert!(req1.check(&ctx, Relation::Viewer, &table).await.unwrap());
        // 処理中の剥奪は同じリクエストには効かない（同一リクエスト内のメモは安全・§16.4）。
        fake.revoke(&ctx.subject(), &role);
        fake.revoke(&ctx.subject(), &table);
        assert!(material::resolve(&ctx, &req1, &[&mgr])
            .await
            .unwrap()
            .has_role("mgr", true));
    }
    // 次のリクエストは必ず引き直し、剥奪が即座に反映される（PIT-18）。
    let req2 = AuthzMemo::new(&fake);
    assert!(!material::resolve(&ctx, &req2, &[&mgr])
        .await
        .unwrap()
        .has_role("mgr", true));
    assert!(!req2.check(&ctx, Relation::Viewer, &table).await.unwrap());
}

#[tokio::test]
async fn unaskable_role_ids_are_false_without_round_trip() {
    let fake = Fake::default();
    let ctx = base();
    let az = AuthzMemo::new(&fake);
    let m = material::resolve_roles(&ctx, &az, &[&has_role("a b", true), &has_role("a#b", true)])
        .await
        .unwrap();
    assert!(!m.has_role("a b", true) && !m.has_role("a#b", true));
    assert_eq!(fake.calls(), 0);
}
