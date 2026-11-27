//! 組織の member と部署・ロールの付け外しの監査（[`super::membership_audit`]・GAP-78）を確かめる。
//!
//! - 単体（メモリの認可と偽の記録係）: 付け外し 1 回につき 1 行で、主体・source・org・object_id が正しいこと。記録に
//!   失敗したら、付けたタプルは消え、外したタプルは戻らないこと。
//! - 結合（`STORAGE_TEST_DATABASE_URL`・台帳に実 Postgres が要る）: ロールの付与で、実際に書いたロールだけを 1 行に
//!   まとめて記録し、もともとあったロールを記録しないこと。記録できなければ付けたタプルを外すこと。
//! - 結合（`STORAGE_TEST_DATABASE_URL`・`OPENFGA_TEST_URL`・試しの Keycloak）: ログインの同期と定期の照合で付け外しした
//!   行が audit_log にハッシュチェーンでつながって残り、object_type=user・object_id で人ごとに引けること。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::large_futures,
    clippy::too_many_lines
)]

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, GroupLayout, Namespace,
    ObjectType, Principal, PrincipalKind, ReadTupleKey, Relation, Subject, TenantId,
};
use futures::FutureExt as _;
use serde_json::{json, Value};
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use storage::audit_view::AuditFilter;
use storage::{DirectoryEntry, DirectoryStore, StorageError};
use uuid::Uuid;

use super::gateway_fixture::NoRefs;
use super::membership_audit::{
    actor_context, AuditSink, MembershipAudit, MembershipSource, RECONCILE_ACTOR, SYNC_ACTOR,
};
use super::org_member::{current_orgs, revoke_stale, sync_org_member_audited};
use super::role_member::{current_roles, revoke_roles, sync_roles};
use crate::config::{AuthConfig, Tenancy};
use crate::keycloak_admin::KeycloakAdmin;
use crate::membership_reconcile::{reconcile_page, ReconcileDeps, ReconcileReport};

/// 偽の記録係が受けた 1 行。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Row {
    actor: String,
    tenant: String,
    org: String,
    action: &'static str,
    object_type: String,
    object_id: String,
    allow: bool,
    metadata: Value,
}

/// 受けた行をメモリに持つ偽の記録係（`fail` の間は記録に失敗する）。
#[derive(Default)]
pub(super) struct Rows {
    rows: Mutex<Vec<Row>>,
    fail: AtomicBool,
}

impl Rows {
    pub(super) fn take(&self) -> Vec<Row> {
        std::mem::take(&mut *self.rows.lock().unwrap())
    }
    pub(super) fn fail(&self, on: bool) {
        self.fail.store(on, Ordering::SeqCst);
    }
}

#[async_trait]
impl AuditSink for Rows {
    async fn record(&self, ctx: &AuthContext, e: AuditEntry<'_>) -> Result<(), StorageError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(StorageError::Invalid("試しの記録の失敗".into()));
        }
        self.rows.lock().unwrap().push(Row {
            actor: ctx.principal().id.clone(),
            tenant: ctx.tenant_id().to_string(),
            org: ctx.org().to_string(),
            action: e.action.as_str(),
            object_type: e.object_type.to_string(),
            object_id: e.object_id.to_string(),
            allow: e.decision == Decision::Allow,
            metadata: e.metadata,
        });
        Ok(())
    }
}

/// あるべき行（利用者は `u1`）。
pub(super) fn row(
    tenant: &str,
    actor: &str,
    org: &str,
    action: &'static str,
    metadata: Value,
) -> Row {
    Row {
        actor: actor.into(),
        tenant: tenant.into(),
        org: org.into(),
        action,
        object_type: "user".into(),
        object_id: "u1".into(),
        allow: true,
        metadata,
    }
}

/// 直接タプルをメモリに持つ認可（check は直接タプルの有無・read は relation を問わず返す）。
///
/// `write_err` を `Some(applied)` にすると、書き込みは誤りを返す（`applied` なら誤りでもタプルは足す・応答が切れた
/// 書き込みを模す）。`fail_after_write_err` の間は、書き込みが誤りを返した直後の check・read を 1 回だけ失敗させる。
/// `fail_reads` は、次の read をその回数だけ失敗させる（読み直しで読める形を模す）。
#[derive(Default)]
pub(super) struct Tuples {
    rows: Mutex<Vec<(String, Relation, String)>>,
    pub(super) fail_read: AtomicBool,
    pub(super) fail_reads: AtomicUsize,
    pub(super) write_err: Mutex<Option<bool>>,
    pub(super) fail_after_write_err: AtomicBool,
    armed: AtomicBool,
}

impl Tuples {
    /// 書き込みが誤りを返した直後の 1 回の check・read を失敗させるか（[`Tuples::fail_after_write_err`]）。
    fn fail_armed(&self) -> Result<(), AuthzError> {
        if self.armed.swap(false, Ordering::SeqCst) {
            return Err(AuthzError::Unexpected {
                status: 500,
                body: "after write".into(),
            });
        }
        Ok(())
    }
    pub(super) fn has(&self, s: &Subject, r: Relation, o: &FgaObject) -> bool {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .any(|(a, b, c)| a == s.as_str() && *b == r && c == o.as_str())
    }
}

#[async_trait]
impl AuthzClient for Tuples {
    async fn check(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        self.fail_armed()?;
        Ok(self.has(s, r, o))
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        let write_err = *self.write_err.lock().unwrap();
        let fresh = !self.has(s, r, o);
        if fresh && write_err != Some(false) {
            self.rows
                .lock()
                .unwrap()
                .push((s.as_str().into(), r, o.as_str().into()));
        }
        if write_err.is_some() {
            if self.fail_after_write_err.load(Ordering::SeqCst) {
                self.armed.store(true, Ordering::SeqCst);
            }
            return Err(AuthzError::Unexpected {
                status: 504,
                body: "write timed out".into(),
            });
        }
        Ok(fresh)
    }
    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|(a, b, c)| !(a == s.as_str() && *b == r && c == o.as_str()));
        Ok(rows.len() != before)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        s: &Subject,
        ty: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.fail_armed()?;
        let counted = self
            .fail_reads
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if counted || self.fail_read.load(Ordering::SeqCst) {
            return Err(AuthzError::Unexpected {
                status: 500,
                body: "read".into(),
            });
        }
        let prefix = format!("{}:", ty.as_str());
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(a, _, o)| a == s.as_str() && o.starts_with(&prefix))
            .map(|(_, _, o)| o.clone())
            .collect())
    }
}

pub(super) fn user(tenant: &str, groups: &[&str]) -> Principal {
    Principal {
        kind: PrincipalKind::User,
        id: "u1".into(),
        email: None,
        groups: groups.iter().map(|g| (*g).to_string()).collect(),
        roles: vec![],
        tenant_id: Some(tenant.into()),
    }
}

pub(super) fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// 属性 `roles` から来たロール（[`sync_roles`]・[`revoke_roles`] に渡す形）。
pub(super) fn attr(items: &[&str]) -> Vec<(String, bool)> {
    items.iter().map(|s| ((*s).to_string(), false)).collect()
}

/// 所属グループから来たロール（部署）。
pub(super) fn dept(items: &[&str]) -> Vec<(String, bool)> {
    items.iter().map(|s| ((*s).to_string(), true)).collect()
}

#[tokio::test]
async fn login_sync_records_one_row_per_org_change() {
    let fga = Tuples::default();
    let ns = Namespace::for_tenant("t");
    let me = ns.user("u1");
    for org in ["gone", "old"] {
        fga.write_tuple(&me, Relation::Member, &ns.organization(org))
            .await
            .unwrap();
    }
    let sink = Rows::default();
    let tenant = TenantId::parse("t").unwrap();
    let layout = GroupLayout::TenantScoped;
    let login = MembershipSource::Login;
    let acme = user("t", &["/tenants/t/acme/eng"]);

    let out = sync_org_member_audited(&fga, &sink, login, &acme, &tenant, layout).await;
    assert!(out.granted);
    assert_eq!(out.revoked, ["gone", "old"]);
    let meta = |source: &str, org: &str| json!({ "source": source, "org": org });
    assert_eq!(
        sink.take(),
        [
            row(
                "t",
                SYNC_ACTOR,
                "acme",
                "org.member.grant",
                meta("login", "acme")
            ),
            row(
                "t",
                SYNC_ACTOR,
                "gone",
                "org.member.revoke",
                meta("login", "gone")
            ),
            row(
                "t",
                SYNC_ACTOR,
                "old",
                "org.member.revoke",
                meta("login", "old")
            ),
        ],
        "組織ごとに 1 行・org の列は member を変えた組織・主体は同期の仕事"
    );

    // 2 回目は何も変えないので、記録もしない（変えたときだけ）。
    let again = sync_org_member_audited(&fga, &sink, login, &acme, &tenant, layout).await;
    assert!(!again.granted && again.revoked.is_empty());
    assert!(sink.take().is_empty());

    // セッションの更新で所属グループから外れた人は、source を session_refresh にして外したことを残す。
    let left = sync_org_member_audited(
        &fga,
        &sink,
        MembershipSource::SessionRefresh,
        &user("t", &[]),
        &tenant,
        layout,
    )
    .await;
    assert_eq!(left.revoked, ["acme"]);
    assert_eq!(
        sink.take(),
        [row(
            "t",
            SYNC_ACTOR,
            "acme",
            "org.member.revoke",
            meta("session_refresh", "acme")
        )]
    );
}

#[tokio::test]
async fn a_grant_that_cannot_be_recorded_is_undone_but_a_revoke_is_kept() {
    let fga = Tuples::default();
    let ns = Namespace::for_tenant("t");
    let me = ns.user("u1");
    fga.write_tuple(&me, Relation::Member, &ns.organization("old"))
        .await
        .unwrap();
    let sink = Rows::default();
    sink.fail(true);
    let tenant = TenantId::parse("t").unwrap();
    let layout = GroupLayout::TenantScoped;
    let acme = user("t", &["/tenants/t/acme"]);

    let out =
        sync_org_member_audited(&fga, &sink, MembershipSource::Login, &acme, &tenant, layout).await;
    assert!(!out.granted, "記録できない付与は付けなかったことにする");
    assert!(
        !fga.has(&me, Relation::Member, &ns.organization("acme")),
        "記録できなかった付与のタプルは外す"
    );
    assert_eq!(out.revoked, ["old"]);
    assert!(
        !fga.has(&me, Relation::Member, &ns.organization("old")),
        "記録できなくても、外したものは戻さない"
    );

    // 記録が戻れば、次の同期で付け直して記録する。
    sink.fail(false);
    let out =
        sync_org_member_audited(&fga, &sink, MembershipSource::Login, &acme, &tenant, layout).await;
    assert!(out.granted);
    assert_eq!(
        sink.take(),
        [row(
            "t",
            SYNC_ACTOR,
            "acme",
            "org.member.grant",
            json!({ "source": "login", "org": "acme" })
        )]
    );
}

#[tokio::test]
async fn reconcile_records_under_its_own_actor_and_counts_failures() {
    let fga = Tuples::default();
    let ns = Namespace::for_tenant("t");
    let me = ns.user("u1");
    for (r, o) in [
        (Relation::Member, ns.organization("acme")),
        (Relation::Member, ns.organization("old")),
        (Relation::Member, ns.role("acme/eng")),
        (Relation::Member, ns.role("sales")),
    ] {
        fga.write_tuple(&me, r, &o).await.unwrap();
    }
    let sink = Rows::default();
    let tenant = TenantId::parse("t").unwrap();
    let audit = MembershipAudit::new(&sink, MembershipSource::Reconcile, &tenant, "u1");

    let current = current_orgs(&fga, &me, &ns).await.unwrap();
    let revoked = revoke_stale(&fga, &audit, &me, &ns, &current, &v(&["acme"])).await;
    assert_eq!(revoked, ["old"]);
    let stale = [dept(&["acme/eng"]), attr(&["sales", "none"])].concat();
    let roles = revoke_roles(&fga, &audit, "acme", &me, &ns, &stale).await;
    assert_eq!(roles, ["acme/eng", "sales"]);
    assert_eq!(
        sink.take(),
        [
            row(
                "t",
                RECONCILE_ACTOR,
                "old",
                "org.member.revoke",
                json!({ "source": "reconcile", "org": "old" })
            ),
            row(
                "t",
                RECONCILE_ACTOR,
                "acme",
                "role.member.revoke",
                json!({ "source": "reconcile", "roles": ["acme/eng", "sales"] })
            ),
        ],
        "ロールは持ち主の組織ごとに 1 行（acme の部署と、接頭辞の無いロールは利用者の組織 acme）・実際に外したロールだけ"
    );
    assert_eq!(audit.failed(), 0);

    // 外すものが無ければ記録しない。
    assert!(
        revoke_roles(&fga, &audit, "acme", &me, &ns, &attr(&["none"]))
            .await
            .is_empty()
    );
    assert!(sink.take().is_empty());

    // 記録できなくても外したものは戻さず、記録できなかった行を数える。
    for o in [ns.organization("old"), ns.role("sales")] {
        fga.write_tuple(&me, Relation::Member, &o).await.unwrap();
    }
    sink.fail(true);
    let failing = MembershipAudit::new(&sink, MembershipSource::Reconcile, &tenant, "u1");
    let current = current_orgs(&fga, &me, &ns).await.unwrap();
    assert_eq!(
        revoke_stale(&fga, &failing, &me, &ns, &current, &v(&["acme"])).await,
        ["old"]
    );
    assert_eq!(
        revoke_roles(&fga, &failing, "acme", &me, &ns, &attr(&["sales"])).await,
        ["sales"]
    );
    assert_eq!(failing.failed(), 2);
    assert!(!fga.has(&me, Relation::Member, &ns.organization("old")));
    assert!(!fga.has(&me, Relation::Member, &ns.role("sales")));
    assert!(fga.has(&me, Relation::Member, &ns.organization("acme")));
}

pub(super) async fn test_pool(db_url: &str) -> sqlx::PgPool {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .connect(db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    pool
}

/// ロールの付与は、実際に書いたロール（`write_tuple` が `true`）だけを 1 行にまとめて記録する。台帳（`directory_role`）
/// に実 Postgres が要るので結合テストにする（認可はメモリ）。
#[tokio::test]
async fn role_grants_record_only_the_roles_actually_written() {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = test_pool(&db_url).await;
    let tenant = format!("t-maudit-{}", Uuid::new_v4().simple());
    let ctx = AuthContext::new(
        user(&tenant, &[]),
        "acme".into(),
        TenantId::parse(tenant.as_str()).unwrap(),
    );
    let (me, ns) = (ctx.subject(), ctx.ns());
    let directory = DirectoryStore::new(pool.clone());
    let fga = Tuples::default();
    let sink = Rows::default();
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

    let body = async {
        // もともとあったロール。
        fga.write_tuple(&me, Relation::Member, &ns.role("eng"))
            .await
            .unwrap();

        // 1. 足りないロールだけを書き、もとからあった eng は記録しない。
        assert!(sync_roles(&fga, &directory, &audit, &ctx, &attr(&["eng", "sales"])).await);
        assert_eq!(sink.take(), [grant(&["sales"])]);

        // 2. 足りないロールだけを書き、付与と剥奪をそれぞれ 1 行にまとめる（剥奪が先）。
        assert!(
            sync_roles(
                &fga,
                &directory,
                &audit,
                &ctx,
                &attr(&["sales", "ops", "qa"])
            )
            .await
        );
        assert_eq!(
            sink.take(),
            [
                row(
                    &tenant,
                    SYNC_ACTOR,
                    "acme",
                    "role.member.revoke",
                    json!({ "source": "login", "roles": ["eng"] })
                ),
                grant(&["ops", "qa"]),
            ]
        );

        // 3. 変えないなら記録しない。
        assert!(
            !sync_roles(
                &fga,
                &directory,
                &audit,
                &ctx,
                &attr(&["sales", "ops", "qa"])
            )
            .await
        );
        assert!(sink.take().is_empty());

        // 4. 記録できなければ、付けたタプルを外す（台帳の行は残す）。
        sink.fail(true);
        sync_roles(
            &fga,
            &directory,
            &audit,
            &ctx,
            &attr(&["sales", "ops", "qa", "dev"]),
        )
        .await;
        let mut held = current_roles(&fga, &me, &ns).await.unwrap();
        held.sort();
        assert_eq!(held, ["ops", "qa", "sales"], "記録できなかった dev は外す");
        assert_eq!(audit.failed(), 1);
        assert!(directory.role_exists(&ctx, "dev").await.unwrap());
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    sqlx::query("DELETE FROM directory_role WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .expect("cleanup");
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// 試しの Keycloak（token・所属グループ・属性）。どの利用者もグループ無し・属性 roles 無し。base の URL を返す。
async fn mock_keycloak() -> String {
    use axum::{routing, Json, Router};
    let app = Router::new()
        .route(
            "/realms/shiki/protocol/openid-connect/token",
            routing::post(|| async { Json(json!({ "access_token": "t" })) }),
        )
        .route(
            "/admin/realms/shiki/users/{id}/groups",
            routing::get(|| async { Json(json!([])) }),
        )
        .route(
            "/admin/realms/shiki/users/{id}",
            routing::get(|| async { Json(json!({ "attributes": {} })) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// Keycloak の管理 API を試しの Keycloak に向けた認証の設定（multi テナント）。
fn mock_auth(base: &str) -> AuthConfig {
    AuthConfig {
        issuer: format!("{base}/realms/shiki"),
        internal_base_url: None,
        jwks_uri: None,
        audience: "shiki-api".into(),
        jwks_ttl_secs: 300,
        client_id: "shiki-web".into(),
        client_secret: None,
        redirect_uri: "http://localhost:3000/auth/callback".into(),
        post_logout_redirect_uri: "http://localhost:3000/".into(),
        scopes: "openid profile".into(),
        tenancy: Tenancy::Multi,
        tenant_id: None,
        provisioner_client_id: Some("p".into()),
        provisioner_client_secret: Some("s".into()),
        admin_base_url: Some(format!("{base}/admin/realms/shiki")),
    }
}

/// ログインの同期で付け、照合で外した行が、audit_log にハッシュチェーンでつながって残り、人ごとに引ける。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_and_reconcile_rows_are_chained_and_found_by_user() {
    let (Ok(db_url), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL・OPENFGA_TEST_URL のどちらかが無いためスキップ");
        return;
    };
    let pool = test_pool(&db_url).await;
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("membership-audit-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA");
    let store_id = fga.store_id().to_string();
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let tenant_id = format!("t-maudit-{}", Uuid::new_v4().simple());
    let tenant = TenantId::parse(tenant_id.as_str()).unwrap();
    let base = mock_keycloak().await;
    let deps = ReconcileDeps {
        db: pool.clone(),
        authz: authz.clone(),
        directory: Arc::new(DirectoryStore::new(pool.clone())),
        data: Arc::new(data::DataStore::new(
            pool.clone(),
            authz.clone(),
            Arc::new(NoRefs),
        )),
        http: reqwest::Client::new(),
        auth: mock_auth(&base),
        batch_size: 10,
        interval: Duration::ZERO,
    };
    let recorder = AuditRecorder::new(pool.clone());
    let ns = Namespace::for_tenant(&tenant_id);
    let me = ns.user("u1");

    let body = async {
        // ログインの同期で acme の member を付ける（主体は同期の仕事・source は login）。
        let org_group = format!("/tenants/{tenant_id}/acme");
        let principal = user(&tenant_id, &[org_group.as_str()]);
        let synced = sync_org_member_audited(
            &*authz,
            &recorder,
            MembershipSource::Login,
            &principal,
            &tenant,
            GroupLayout::TenantScoped,
        )
        .await;
        assert!(synced.granted, "{synced:?}");
        // 前の組織の member と、ログインの同期が記録した部署。
        authz
            .write_tuple(&me, Relation::Member, &ns.organization("old"))
            .await
            .unwrap();
        authz
            .write_tuple(&me, Relation::Member, &ns.role("acme/eng"))
            .await
            .unwrap();
        deps.directory
            .ensure_user("u1", &tenant_id, "acme", None)
            .await
            .unwrap();
        deps.directory
            .replace_user_roles(&tenant_id, "acme", "u1", &[("acme/eng".into(), true)])
            .await
            .unwrap();
        // 台帳の部署の行（ログインの同期が部署の組織で書いたもの・照合が外した行の組織を決める）。
        deps.directory
            .upsert_role("acme/eng", &tenant_id, "acme", "acme/eng")
            .await
            .unwrap();

        // Keycloak の所属グループから外れた（グループも属性 roles も無い）ので、照合が組織と部署を外す。
        let admin = KeycloakAdmin::from_config(&deps.http, &deps.auth).unwrap();
        let mut kc = admin.session().await.unwrap();
        let mut report = ReconcileReport::default();
        let page = [DirectoryEntry {
            tenant_id: tenant_id.clone(),
            user_id: "u1".into(),
        }];
        let done = reconcile_page(&deps, &mut kc, &page, &mut report)
            .await
            .unwrap();
        assert_eq!(done, 1);
        assert_eq!(
            (
                report.revoked_orgs,
                report.revoked_roles,
                report.audit_failed
            ),
            (2, 1, 0),
            "{report:?}"
        );

        // 監査の画面と同じ絞り込み（object_type=user・object_id=sub）で、組織ごとに引ける。
        let viewer = recorder.viewer();
        let filter = AuditFilter {
            from: chrono::Utc::now() - chrono::Duration::hours(1),
            to: chrono::Utc::now() + chrono::Duration::hours(1),
            object_type: Some("user".into()),
            object_id: Some("u1".into()),
            ..AuditFilter::default()
        };
        let mut seen = Vec::new();
        for (org, expected) in [
            (
                // 部署の行の組織は、部署を持つ組織（利用者の組織の既定ではない）。
                "acme",
                vec![
                    (
                        RECONCILE_ACTOR,
                        "role.member.revoke",
                        json!({ "source": "reconcile", "roles": ["acme/eng"] }),
                    ),
                    (
                        RECONCILE_ACTOR,
                        "org.member.revoke",
                        json!({ "source": "reconcile", "org": "acme" }),
                    ),
                    (
                        SYNC_ACTOR,
                        "org.member.grant",
                        json!({ "source": "login", "org": "acme" }),
                    ),
                ],
            ),
            (
                "old",
                vec![(
                    RECONCILE_ACTOR,
                    "org.member.revoke",
                    json!({ "source": "reconcile", "org": "old" }),
                )],
            ),
        ] {
            let ctx = actor_context(MembershipSource::Reconcile, &tenant, org);
            let events = viewer.search(&ctx, &filter, None, 50).await.unwrap();
            let got: Vec<(String, String, Value, bool)> = events
                .iter()
                .map(|e| {
                    (
                        e.actor.clone(),
                        e.action.clone(),
                        e.metadata.clone(),
                        e.chained,
                    )
                })
                .collect();
            let want: Vec<(String, String, Value, bool)> = expected
                .into_iter()
                .map(|(a, act, m)| (a.to_string(), act.to_string(), m, true))
                .collect();
            assert_eq!(got, want, "org={org}（新しい順・どれもチェーンにつなぐ）");
            assert!(events.iter().all(|e| e.object_type == "user"
                && e.object_id == "u1"
                && e.decision == storage::audit_view::AuditDecision::Allow));

            // ハッシュチェーンを先頭から検証できる（record_chained が直前の行につなぐ）。
            let chain = recorder
                .chain_verifier()
                .verify(&ctx, None, 100)
                .await
                .unwrap();
            assert!(chain.broken.is_none(), "org={org}: {chain:?}");
            assert_eq!(chain.checked, want.len() as u64, "org={org}: {chain:?}");
            seen.push((org, chain.checked));
        }
        eprintln!("membership audit: 組織ごとの連結行 {seen:?}");
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    for sql in [
        "DELETE FROM audit_log WHERE tenant_id = $1",
        "DELETE FROM directory_user_role WHERE tenant_id = $1",
        "DELETE FROM directory_user WHERE tenant_id = $1",
        "DELETE FROM directory_role WHERE tenant_id = $1",
    ] {
        if let Err(e) = sqlx::query(sql).bind(&tenant_id).execute(&pool).await {
            eprintln!("{sql}: {e}");
        }
    }
    let deleted = reqwest::Client::new()
        .delete(format!("{fga_url}/stores/{store_id}"))
        .send()
        .await
        .expect("delete store");
    assert!(deleted.status().is_success(), "{}", deleted.status());
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
