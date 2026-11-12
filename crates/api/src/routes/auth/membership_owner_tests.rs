//! 部署・ロールの付け外しの監査の行が、ロールを持つ組織に書かれること（[`super::role_member::role_owner_org`]）と、
//! 付ける書き込みが誤りで返ったとき（応答が切れても適用されていることがある）に、記録の無い付与が残らないことを
//! 確かめる（偽の記録係とメモリの認可は [`super::membership_audit_tests`] のもの）。
//!
//! - 単体: 剥奪の行の組織・組織の member の書き込みの誤り。
//! - 結合（`STORAGE_TEST_DATABASE_URL`・台帳に実 Postgres が要る）: ログインの同期（`sync_roles`）で、組織を移った人・
//!   2 つの組織の部署にいる人の付け外しの行の組織と、ロールの書き込みの誤り。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;

use authz::{AuthContext, AuthzClient, GroupLayout, Namespace, Relation, TenantId};
use futures::FutureExt as _;
use serde_json::json;
use storage::DirectoryStore;
use uuid::Uuid;

use super::membership_audit::{MembershipAudit, MembershipSource, RECONCILE_ACTOR, SYNC_ACTOR};
use super::membership_audit_tests::{row, test_pool, user, v, Rows, Tuples};
use super::org_member::sync_org_member_audited;
use super::role_member::{current_roles, revoke_roles, role_owner_org, roles_by_owner, sync_roles};

#[test]
fn the_owner_of_a_department_role_is_its_org() {
    assert_eq!(role_owner_org("acme/eng", "globex"), "acme");
    assert_eq!(role_owner_org("acme/eng/qa", "globex"), "acme");
    // 接頭辞の無いロール（属性 roles）と、組織の名前にできない接頭辞は、利用者の組織。
    assert_eq!(role_owner_org("sales", "globex"), "globex");
    assert_eq!(role_owner_org("/eng", "globex"), "globex");
    assert_eq!(role_owner_org("a:b/eng", "globex"), "globex");
    assert_eq!(
        roles_by_owner(&v(&["acme/eng", "sales", "globex/x", "acme/ops"]), "globex"),
        [
            ("acme".to_string(), v(&["acme/eng", "acme/ops"])),
            ("globex".to_string(), v(&["sales", "globex/x"])),
        ]
    );
}

#[tokio::test]
async fn revoked_department_roles_are_recorded_under_the_department_org() {
    // acme/eng にいた人が globex/sales へ移った後の照合（利用者の組織は globex）。
    let fga = Tuples::default();
    let ns = Namespace::for_tenant("t");
    let me = ns.user("u1");
    for role in ["acme/eng", "ops"] {
        fga.write_tuple(&me, Relation::Member, &ns.role(role))
            .await
            .unwrap();
    }
    let sink = Rows::default();
    let tenant = TenantId::parse("t").unwrap();
    let audit = MembershipAudit::new(&sink, MembershipSource::Reconcile, &tenant, "u1");
    let revoked = revoke_roles(&fga, &audit, "globex", &me, &ns, &v(&["acme/eng", "ops"])).await;
    assert_eq!(revoked, ["acme/eng", "ops"]);
    assert_eq!(
        sink.take(),
        [
            row(
                "t",
                RECONCILE_ACTOR,
                "acme",
                "role.member.revoke",
                json!({ "source": "reconcile", "roles": ["acme/eng"] })
            ),
            row(
                "t",
                RECONCILE_ACTOR,
                "globex",
                "role.member.revoke",
                json!({ "source": "reconcile", "roles": ["ops"] })
            ),
        ],
        "acme の部署は acme の監査に、接頭辞の無いロールは利用者の組織の監査に"
    );
}

#[tokio::test]
async fn an_org_member_write_that_errors_but_applies_is_recorded() {
    let fga = Tuples::default();
    let ns = Namespace::for_tenant("t");
    let me = ns.user("u1");
    let acme = ns.organization("acme");
    let sink = Rows::default();
    let tenant = TenantId::parse("t").unwrap();
    let layout = GroupLayout::TenantScoped;
    let login = MembershipSource::Login;
    let principal = user("t", &["/tenants/t/acme"]);
    let grant = || {
        row(
            "t",
            SYNC_ACTOR,
            "acme",
            "org.member.grant",
            json!({ "source": "login", "org": "acme" }),
        )
    };

    // 1. 書き込みは誤りで返ったが適用された: 確かめ直してあるので、付けたものとして記録する。
    *fga.write_err.lock().unwrap() = Some(true);
    let out = sync_org_member_audited(&fga, &sink, login, &principal, &tenant, layout).await;
    assert!(out.granted, "{out:?}");
    assert!(fga.has(&me, Relation::Member, &acme));
    assert_eq!(sink.take(), [grant()]);
    // 次の同期は何も変えず、何も記録しない（タプルと grant の行がそろっている）。
    *fga.write_err.lock().unwrap() = None;
    let out = sync_org_member_audited(&fga, &sink, login, &principal, &tenant, layout).await;
    assert!(!out.granted && out.revoked.is_empty());
    assert!(sink.take().is_empty());

    // 2. 誤りで返り、確かめ直しもできない: 書けていたかもしれないタプルを外し、記録しない。
    fga.delete_tuple(&me, Relation::Member, &acme)
        .await
        .unwrap();
    *fga.write_err.lock().unwrap() = Some(true);
    fga.fail_after_write_err.store(true, Ordering::SeqCst);
    let out = sync_org_member_audited(&fga, &sink, login, &principal, &tenant, layout).await;
    assert!(!out.granted);
    assert!(
        !fga.has(&me, Relation::Member, &acme),
        "記録の無い付与を残さない"
    );
    assert!(sink.take().is_empty());
    // 次の同期で付け直して記録する。
    *fga.write_err.lock().unwrap() = None;
    fga.fail_after_write_err.store(false, Ordering::SeqCst);
    let out = sync_org_member_audited(&fga, &sink, login, &principal, &tenant, layout).await;
    assert!(out.granted);
    assert!(fga.has(&me, Relation::Member, &acme));
    assert_eq!(sink.take(), [grant()]);

    // 3. 誤りで返り、適用もされていない: 何も記録しない（次の同期で付ける）。
    fga.delete_tuple(&me, Relation::Member, &acme)
        .await
        .unwrap();
    *fga.write_err.lock().unwrap() = Some(false);
    let out = sync_org_member_audited(&fga, &sink, login, &principal, &tenant, layout).await;
    assert!(!out.granted);
    assert!(!fga.has(&me, Relation::Member, &acme));
    assert!(sink.take().is_empty());
}

/// 台帳（`directory_role`）に実 Postgres が要るログインの同期の試し（認可はメモリ）。`body` の後に試しのテナントの
/// 台帳を消す。
async fn with_ledger<F, Fut>(body: F)
where
    F: FnOnce(DirectoryStore, String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = test_pool(&db_url).await;
    let tenant = format!("t-mowner-{}", Uuid::new_v4().simple());
    let outcome = AssertUnwindSafe(body(DirectoryStore::new(pool.clone()), tenant.clone()))
        .catch_unwind()
        .await;
    sqlx::query("DELETE FROM directory_role WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .expect("cleanup");
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn ctx_for(tenant: &str, org: &str) -> AuthContext {
    AuthContext::new(
        user(tenant, &[]),
        org.into(),
        TenantId::parse(tenant).unwrap(),
    )
}

#[tokio::test]
async fn login_sync_records_role_changes_under_each_department_org() {
    with_ledger(|directory, tenant| async move {
        let fga = Tuples::default();
        let sink = Rows::default();
        let meta = |roles: &[&str]| json!({ "source": "login", "roles": roles });

        // 1. acme/eng にいた人を globex/sales へ移した（利用者の組織は globex）: acme/eng の剥奪は acme に、globex/sales の
        //    付与は globex に残す。
        let ctx = ctx_for(&tenant, "globex");
        let (me, ns) = (ctx.subject(), ctx.ns());
        fga.write_tuple(&me, Relation::Member, &ns.role("acme/eng"))
            .await
            .unwrap();
        let audit = MembershipAudit::new(&sink, MembershipSource::Login, ctx.tenant(), "u1");
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &v(&["globex/sales"])).await);
        assert_eq!(
            sink.take(),
            [
                row(
                    &tenant,
                    SYNC_ACTOR,
                    "acme",
                    "role.member.revoke",
                    meta(&["acme/eng"])
                ),
                row(
                    &tenant,
                    SYNC_ACTOR,
                    "globex",
                    "role.member.grant",
                    meta(&["globex/sales"])
                ),
            ]
        );

        // 2. acme/eng と globex/sales の両方にいる人（利用者の組織は acme）: 付与はそれぞれの組織に 1 行ずつ。属性 roles の
        //    ロールは利用者の組織に。
        let ctx = ctx_for(&tenant, "acme");
        let fga = Tuples::default();
        let audit = MembershipAudit::new(&sink, MembershipSource::Login, ctx.tenant(), "u1");
        let desired = v(&["acme/eng", "globex/sales", "lead"]);
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &desired).await);
        assert_eq!(
            sink.take(),
            [
                row(
                    &tenant,
                    SYNC_ACTOR,
                    "acme",
                    "role.member.grant",
                    meta(&["acme/eng", "lead"])
                ),
                row(
                    &tenant,
                    SYNC_ACTOR,
                    "globex",
                    "role.member.grant",
                    meta(&["globex/sales"])
                ),
            ]
        );

        // 3. 片方の組織に記録できなければ、その組織のぶんのタプルだけを外す（記録の無い付与を残さない）。
        let fga = Tuples::default();
        let failing = FailFor("globex");
        let audit = MembershipAudit::new(&failing, MembershipSource::Login, ctx.tenant(), "u1");
        sync_roles(&fga, &directory, &audit, &ctx, &desired).await;
        let mut held = current_roles(&fga, &ctx.subject(), &ctx.ns())
            .await
            .unwrap();
        held.sort();
        assert_eq!(held, ["acme/eng", "lead"], "globex のぶんだけ外す");
        assert_eq!(audit.failed(), 1);
    })
    .await;
}

/// `org` の行だけ記録に失敗する記録係（ほかの組織の行は捨てる）。
struct FailFor(&'static str);

#[async_trait::async_trait]
impl super::membership_audit::AuditSink for FailFor {
    async fn record(
        &self,
        ctx: &AuthContext,
        _: storage::audit::AuditEntry<'_>,
    ) -> Result<(), storage::StorageError> {
        if ctx.org() == self.0 {
            return Err(storage::StorageError::Invalid("試しの記録の失敗".into()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn a_role_write_that_errors_but_applies_is_recorded() {
    with_ledger(|directory, tenant| async move {
        let fga = Tuples::default();
        let sink = Rows::default();
        let ctx = ctx_for(&tenant, "acme");
        let (me, ns) = (ctx.subject(), ctx.ns());
        let audit = MembershipAudit::new(&sink, MembershipSource::Login, ctx.tenant(), "u1");
        let grant = |roles: &[&str]| {
            row(
                &tenant,
                SYNC_ACTOR,
                "acme",
                "role.member.grant",
                json!({ "source": "login", "roles": roles }),
            )
        };

        // 1. 書き込みは誤りで返ったが適用された: 今の直接タプルを読み直してあるので、付けたものとして記録する。
        *fga.write_err.lock().unwrap() = Some(true);
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &v(&["acme/eng"])).await);
        assert!(fga.has(&me, Relation::Member, &ns.role("acme/eng")));
        assert_eq!(sink.take(), [grant(&["acme/eng"])]);
        // 次の同期は何も変えず、何も記録しない（タプルと grant の行がそろっている）。
        *fga.write_err.lock().unwrap() = None;
        assert!(!sync_roles(&fga, &directory, &audit, &ctx, &v(&["acme/eng"])).await);
        assert!(sink.take().is_empty());

        // 2. 誤りで返り、読み直しもできない（同期の初めには読めた）: 書けていたかもしれないタプルを外し、記録しない。
        *fga.write_err.lock().unwrap() = Some(true);
        fga.fail_after_write_err.store(true, Ordering::SeqCst);
        let desired = v(&["acme/eng", "acme/ops"]);
        sync_roles(&fga, &directory, &audit, &ctx, &desired).await;
        assert!(
            !fga.has(&me, Relation::Member, &ns.role("acme/ops")),
            "記録の無い付与を残さない"
        );
        assert!(fga.has(&me, Relation::Member, &ns.role("acme/eng")));
        assert!(sink.take().is_empty());
        // 次の同期で付け直して記録する。
        *fga.write_err.lock().unwrap() = None;
        fga.fail_after_write_err.store(false, Ordering::SeqCst);
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &desired).await);
        assert_eq!(sink.take(), [grant(&["acme/ops"])]);

        // 3. 同期の初めに今のタプルを読めなかったときは、確かめられなくても外さない（もとからあったかもしれない）。
        fga.fail_read.store(true, Ordering::SeqCst);
        *fga.write_err.lock().unwrap() = Some(true);
        sync_roles(&fga, &directory, &audit, &ctx, &desired).await;
        fga.fail_read.store(false, Ordering::SeqCst);
        assert!(fga.has(&me, Relation::Member, &ns.role("acme/eng")));
        assert!(fga.has(&me, Relation::Member, &ns.role("acme/ops")));
        assert!(sink.take().is_empty());
    })
    .await;
}
