//! 部署・ロールの付け外しの監査の行が、ロールを持つ組織に書かれること（[`super::role_member::role_owner_org`]）と、
//! 付ける書き込みが誤りで返ったとき（応答が切れても適用されていることがある）に、記録の無い付与が残らないことを
//! 確かめる（偽の記録係とメモリの認可は [`super::membership_audit_tests`] のもの）。
//!
//! - 単体: 剥奪の行の組織・組織の member の書き込みの誤り。
//! - 結合（`STORAGE_TEST_DATABASE_URL`・台帳に実 Postgres が要る）: ログインの同期（`sync_roles`）で、組織を移った人・
//!   2 つの組織の部署にいる人・`/` を含む属性 roles のロールを持つ人の付け外しの行と台帳の行の組織と、ロールの
//!   書き込みの誤り（今のタプルを読めないときを含む）。

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
use super::membership_audit_tests::{attr, dept, row, test_pool, user, v, Rows, Tuples};
use super::org_member::sync_org_member_audited;
use super::role_member::{current_roles, revoke_roles, role_owner_org, roles_by_owner, sync_roles};

#[test]
fn the_owner_of_a_department_role_is_its_org() {
    // 所属グループから来たロールは、先頭のセグメントの組織（2 つ目の組織のグループそのものは、その組織）。
    assert_eq!(role_owner_org("acme/eng", true, "globex"), "acme");
    assert_eq!(role_owner_org("acme/eng/qa", true, "globex"), "acme");
    assert_eq!(role_owner_org("beta", true, "acme"), "beta");
    // 属性 roles のロールは、`/` を含んでも利用者の組織（ほかの組織の名前・組織でない接頭辞で決めない）。
    assert_eq!(role_owner_org("sales", false, "globex"), "globex");
    assert_eq!(role_owner_org("acme/admin", false, "globex"), "globex");
    assert_eq!(role_owner_org("sales/manager", false, "globex"), "globex");
    // 組織の名前にできない接頭辞は、利用者の組織。
    assert_eq!(role_owner_org("/eng", true, "globex"), "globex");
    assert_eq!(role_owner_org("a:b/eng", true, "globex"), "globex");
    let roles = [
        dept(&["acme/eng"]),
        attr(&["sales"]),
        dept(&["globex/x", "acme/ops"]),
        attr(&["acme/admin"]),
    ]
    .concat();
    assert_eq!(
        roles_by_owner(&roles, "globex"),
        [
            ("acme".to_string(), v(&["acme/eng", "acme/ops"])),
            (
                "globex".to_string(),
                v(&["sales", "globex/x", "acme/admin"])
            ),
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
    let stale = [dept(&["acme/eng"]), attr(&["ops"])].concat();
    let revoked = revoke_roles(&fga, &audit, "globex", &me, &ns, &stale).await;
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

    // 2. 誤りで返り、確かめ直しもできない: 書けていたかもしれないタプルを外し、付けたとは記録しない。実際に外したので、
    //    外したことは記録する（同じ人の並行する同期が付けて記録した member を外したときも、監査とタプルがそろう）。
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
    assert_eq!(
        sink.take(),
        [row(
            "t",
            SYNC_ACTOR,
            "acme",
            "org.member.revoke",
            json!({ "source": "login", "org": "acme" }),
        )]
    );
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
        //    付与は globex に残す。acme/eng は、acme の人のログインの同期が acme の部署として台帳に書いたもの。
        let ctx = ctx_for(&tenant, "globex");
        let (me, ns) = (ctx.subject(), ctx.ns());
        fga.write_tuple(&me, Relation::Member, &ns.role("acme/eng"))
            .await
            .unwrap();
        directory
            .upsert_role("acme/eng", &tenant, "acme", "acme/eng")
            .await
            .unwrap();
        let audit = MembershipAudit::new(&sink, MembershipSource::Login, ctx.tenant(), "u1");
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &dept(&["globex/sales"])).await);
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
        //    ロールは利用者の組織に。台帳の部署の行は、付けた人の組織（acme）ではなく部署の組織に書く。
        let ctx = ctx_for(&tenant, "acme");
        let fga = Tuples::default();
        let audit = MembershipAudit::new(&sink, MembershipSource::Login, ctx.tenant(), "u1");
        let desired = [dept(&["acme/eng", "globex/sales"]), attr(&["lead"])].concat();
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
        let globex = ctx_for(&tenant, "globex");
        assert!(!directory.role_exists(&ctx, "globex/sales").await.unwrap());
        assert!(directory
            .role_exists(&globex, "globex/sales")
            .await
            .unwrap());
        assert!(directory.role_exists(&ctx, "lead").await.unwrap());

        // 前の形（付けた人の組織で書いた行）は、部署の人の次のログインで部署の組織に直す。
        directory
            .upsert_role("globex/sales", &tenant, "acme", "globex/sales")
            .await
            .unwrap();
        assert!(!sync_roles(&fga, &directory, &audit, &ctx, &desired).await);
        assert!(sink.take().is_empty());
        assert!(!directory.role_exists(&ctx, "globex/sales").await.unwrap());
        assert!(directory
            .role_exists(&globex, "globex/sales")
            .await
            .unwrap());

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

        // 4. 2 つ目の組織のグループそのもの（beta）がロールになった人: その付与は beta の監査に書く。
        let fga = Tuples::default();
        let audit = MembershipAudit::new(&sink, MembershipSource::Login, ctx.tenant(), "u1");
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &dept(&["beta"])).await);
        assert_eq!(
            sink.take(),
            [row(
                &tenant,
                SYNC_ACTOR,
                "beta",
                "role.member.grant",
                meta(&["beta"])
            )]
        );
    })
    .await;
}

#[tokio::test]
async fn attribute_roles_with_a_slash_stay_in_the_users_org() {
    with_ledger(|directory, tenant| async move {
        // 属性 roles の値に `/` を含む（ほかの組織の名前の接頭辞・組織でない接頭辞）。付け外しの行も台帳の行も、
        // 利用者の組織（globex）に書く。acme の監査に globex の人の sub とロールを出さず、存在しない組織 sales の
        // 監査にも書かない。
        let fga = Tuples::default();
        let sink = Rows::default();
        let ctx = ctx_for(&tenant, "globex");
        let acme = ctx_for(&tenant, "acme");
        let audit = MembershipAudit::new(&sink, MembershipSource::Login, ctx.tenant(), "u1");
        let meta = |roles: &[&str]| json!({ "source": "login", "roles": roles });
        let desired = attr(&["acme/admin", "sales/manager"]);
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &desired).await);
        assert_eq!(
            sink.take(),
            [row(
                &tenant,
                SYNC_ACTOR,
                "globex",
                "role.member.grant",
                meta(&["acme/admin", "sales/manager"])
            )]
        );
        assert!(directory.role_exists(&ctx, "acme/admin").await.unwrap());
        assert!(!directory.role_exists(&acme, "acme/admin").await.unwrap());

        // 外すときも利用者の組織（台帳がその接頭辞の組織のロールとしていない）。
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &[]).await);
        assert_eq!(
            sink.take(),
            [row(
                &tenant,
                SYNC_ACTOR,
                "globex",
                "role.member.revoke",
                meta(&["acme/admin", "sales/manager"])
            )]
        );
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
        let line = |action: &'static str, roles: &[&str]| {
            row(
                &tenant,
                SYNC_ACTOR,
                "acme",
                action,
                json!({ "source": "login", "roles": roles }),
            )
        };
        let grant = |roles: &[&str]| line("role.member.grant", roles);

        // 1. 書き込みは誤りで返ったが適用された: 今の直接タプルを読み直してあるので、付けたものとして記録する。
        *fga.write_err.lock().unwrap() = Some(true);
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &dept(&["acme/eng"])).await);
        assert!(fga.has(&me, Relation::Member, &ns.role("acme/eng")));
        assert_eq!(sink.take(), [grant(&["acme/eng"])]);
        // 次の同期は何も変えず、何も記録しない（タプルと grant の行がそろっている）。
        *fga.write_err.lock().unwrap() = None;
        assert!(!sync_roles(&fga, &directory, &audit, &ctx, &dept(&["acme/eng"])).await);
        assert!(sink.take().is_empty());

        // 2. 誤りで返り、読み直しもできない（同期の初めには読めた）: 書けていたかもしれないタプルを外し、付けたとは記録
        //    しない。実際に外したので、外したことは記録する。
        *fga.write_err.lock().unwrap() = Some(true);
        fga.fail_after_write_err.store(true, Ordering::SeqCst);
        let desired = dept(&["acme/eng", "acme/ops"]);
        sync_roles(&fga, &directory, &audit, &ctx, &desired).await;
        assert!(
            !fga.has(&me, Relation::Member, &ns.role("acme/ops")),
            "記録の無い付与を残さない"
        );
        assert!(fga.has(&me, Relation::Member, &ns.role("acme/eng")));
        assert_eq!(sink.take(), [line("role.member.revoke", &["acme/ops"])]);
        // 次の同期で付け直して記録する。
        *fga.write_err.lock().unwrap() = None;
        fga.fail_after_write_err.store(false, Ordering::SeqCst);
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &desired).await);
        assert_eq!(sink.take(), [grant(&["acme/ops"])]);

        // 3. 同期の初めの読み取りだけ失敗し、読み直しは読める: もとからあったロールは書きにいかず、記録もしない。
        //    書き込みが誤りで返った新しいロールだけを、確かめて記録する。
        let desired = dept(&["acme/eng", "acme/ops", "acme/qa"]);
        fga.fail_reads.store(1, Ordering::SeqCst);
        *fga.write_err.lock().unwrap() = Some(true);
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &desired).await);
        assert!(fga.has(&me, Relation::Member, &ns.role("acme/qa")));
        assert_eq!(sink.take(), [grant(&["acme/qa"])]);

        // 4. 同期の初めに今のタプルを読み直しても読めない: もとの集合が分からないので、付けも外しもしない（もとからあった
        //    ロールと付けたロールを見分けられない）。次の同期で付けて記録する。
        let desired = dept(&["acme/eng", "acme/ops", "acme/qa", "acme/dev"]);
        fga.fail_read.store(true, Ordering::SeqCst);
        assert!(!sync_roles(&fga, &directory, &audit, &ctx, &desired).await);
        fga.fail_read.store(false, Ordering::SeqCst);
        assert!(!fga.has(&me, Relation::Member, &ns.role("acme/dev")));
        for role in ["acme/eng", "acme/ops", "acme/qa"] {
            assert!(fga.has(&me, Relation::Member, &ns.role(role)), "{role}");
        }
        assert!(sink.take().is_empty());
        *fga.write_err.lock().unwrap() = None;
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &desired).await);
        assert_eq!(sink.take(), [grant(&["acme/dev"])]);
    })
    .await;
}
