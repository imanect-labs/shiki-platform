//! 実 Keycloak・実 OpenFGA・実 Postgres で、組織の member と部署の定期の照合（[`crate::membership_reconcile`]・D-136）が、
//! ログインしないままアプリのトークンだけを使う人の member を外し、ゲートウェイが 403 になることを確かめる。
//!
//! `KEYCLOAK_TEST_URL`・`STORAGE_TEST_DATABASE_URL`・`OPENFGA_TEST_URL` のどれかが無ければスキップする。Keycloak には
//! この試しだけの利用者と部署のグループを作り、最後に消す（realm の alice たちの所属は変えない）。
//!
//! 1. 所属グループに入っている人は、照合しても何も外れない（ゲートウェイは 200）。
//! 2. グループに入っていない人は、既定の組織の member が残り、ほかの組織の member が外れる（admin は残る・BUG-200）。
//!    ログインの同期も既定の組織の member を残す。
//! 3. Keycloak にいない人は外さない。Keycloak を読めない（資格情報の誤り）ときは、照合が失敗して何も外さない。
//! 4. グループから外した人は、ログインしなくても照合で組織の member と部署が外れ、同じトークンでゲートウェイが 403 になる。
//!    属性 roles から外したロールも、ログインしなくても外れる（ログインの同期と同じ決め方・D-140）。属性 roles にある
//!    ロールと、ログインの同期が記録していないロール（手で書いたタプル）は残る。
//! 5. 照合は、どのサーバーの回でも前の回から間隔が経つまで進めない（サーバーの台数で Keycloak への読みを増やさない）。
//!
//! もう 1 本は、試しの Keycloak（token と所属グループだけを返す）で、Keycloak の失敗が照合を止めないこと・外さない
//! ことを確かめる（`STORAGE_TEST_DATABASE_URL`・`OPENFGA_TEST_URL` が要る）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::large_futures,
    clippy::too_many_lines
)]

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_gateway::test_support::TestRealm;
use authz::{
    AuthzClient, Consistency, FgaObject, GroupLayout, Namespace, Principal, PrincipalKind, Relation,
};
use futures::FutureExt as _;
use serde_json::json;
use storage::DirectoryStore;
use uuid::Uuid;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use storage::DirectoryEntry;

use super::gateway_fixture::{GatewayFixture, NoRefs, SHELL_CALLBACK, TENANT};
use super::org_member::sync_org_member;
use crate::config::{AuthConfig, Tenancy};
use crate::extract::DEFAULT_ORG;
use crate::keycloak_admin::KeycloakAdmin;
use crate::membership_reconcile::{reconcile_page, run_once, ReconcileDeps, ReconcileReport};

/// 試しの利用者のパスワード（`TestRealm::user_token` が送るもの）。
const PASSWORD: &str = "password";

/// realm の provisioner で Keycloak の管理 API を呼ぶ認証の設定（multi テナント）。
fn auth_config(realm: &TestRealm, secret: &str) -> AuthConfig {
    auth_for(
        realm.issuer(),
        realm.admin_base(),
        realm.provisioner().0,
        secret,
    )
}

/// Keycloak の管理 API を呼ぶ認証の設定（multi テナント）。
fn auth_for(issuer: String, admin_base: String, client: &str, secret: &str) -> AuthConfig {
    AuthConfig {
        issuer,
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
        provisioner_client_id: Some(client.into()),
        provisioner_client_secret: Some(secret.into()),
        admin_base_url: Some(admin_base),
    }
}

/// provisioner のトークン（試しの利用者を作る・消すのに使う）。
async fn admin_token(realm: &TestRealm) -> String {
    let (id, secret) = realm.provisioner();
    let body: serde_json::Value = reqwest::Client::new()
        .post(realm.token_endpoint())
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", id),
            ("client_secret", secret),
        ])
        .send()
        .await
        .expect("admin token")
        .json()
        .await
        .expect("admin token json");
    body["access_token"].as_str().expect("access_token").into()
}

/// 利用者の属性 roles を置き換える（Keycloak の user PUT は置き換えなので、読んでから roles だけを差し替える）。
async fn set_roles_attribute(realm: &TestRealm, user_id: &str, roles: &[&str]) {
    let token = admin_token(realm).await;
    let url = format!("{}/users/{user_id}", realm.admin_base());
    let mut user: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("get user")
        .json()
        .await
        .expect("user json");
    user["attributes"]["roles"] = json!(roles);
    let resp = reqwest::Client::new()
        .put(&url)
        .bearer_auth(&token)
        .json(&user)
        .send()
        .await
        .expect("put user");
    assert!(resp.status().is_success(), "put user: {}", resp.status());
}

/// パスワードでログインできる試しの利用者を作り、id を返す。
async fn create_user(
    realm: &TestRealm,
    admin: &KeycloakAdmin<'_>,
    username: &str,
    groups: &[String],
    roles: &[&str],
) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{}/users", realm.admin_base()))
        .bearer_auth(admin_token(realm).await)
        .json(&json!({
            "username": username,
            "email": format!("{username}@a-corp.example.com"),
            "firstName": "D136",
            "lastName": "Test",
            "enabled": true,
            "emailVerified": true,
            "attributes": { "tenant": [TENANT], "roles": roles },
            "groups": groups,
            "credentials": [{ "type": "password", "value": PASSWORD, "temporary": false }],
        }))
        .send()
        .await
        .expect("create user");
    assert!(resp.status().is_success(), "create user: {}", resp.status());
    admin
        .find_user_by_username(username)
        .await
        .expect("find user")
        .expect("user")
        .id
}

async fn has(
    authz: &dyn AuthzClient,
    ns: &Namespace<'_>,
    user: &str,
    r: Relation,
    o: &FgaObject,
) -> bool {
    authz
        .check(&ns.user(user), r, o, Consistency::HigherConsistency)
        .await
        .expect("check")
}

/// 頭に戻るまで照合を回す（試しの DB にほかの試しの人がいても 1 周する）。
async fn sweep(deps: &ReconcileDeps) -> ReconcileReport {
    let mut total = ReconcileReport::default();
    for _ in 0..50 {
        let r = run_once(deps).await.expect("run_once").expect("lock");
        total.read += r.read;
        total.checked += r.checked;
        total.not_found += r.not_found;
        total.failed += r.failed;
        total.revoked_orgs += r.revoked_orgs;
        total.revoked_roles += r.revoked_roles;
        if r.wrapped {
            total.wrapped = true;
            return total;
        }
    }
    panic!("照合が 1 周しません: {total:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_revokes_members_who_left_their_groups_without_logging_in() {
    let Some((realm, fx)) = GatewayFixture::new("membership-reconcile-it").await else {
        return;
    };
    let http = reqwest::Client::new();
    let auth = auth_config(&realm, realm.provisioner().1);
    let admin = KeycloakAdmin::from_config(&http, &auth).expect("admin");
    let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
    let leaver_name = format!("d136-leaver-{tag}");
    let loner_name = format!("d136-loner-{tag}");
    let dept = format!("d136-{tag}");
    let dept_role = format!("{TENANT}/{dept}");
    let manual_role = format!("d136-manual-{tag}");
    let attr_role = format!("d140-attr-{tag}");
    let ghost = Uuid::new_v4().to_string();
    let directory = Arc::new(DirectoryStore::new(fx.pool.clone()));
    let authz = fx.authz.clone();
    let ns = Namespace::for_tenant(TENANT);
    let org = ns.organization(TENANT);
    let deps = ReconcileDeps {
        db: fx.pool.clone(),
        authz: authz.clone(),
        directory: Arc::clone(&directory),
        data: Arc::clone(&fx.data),
        http: http.clone(),
        auth: auth.clone(),
        batch_size: 1_000,
        // 試しでは続けて回す（間隔の決まりは別に確かめる）。
        interval: Duration::ZERO,
    };
    // 片付けるもの（作ったときに足す）。
    let kc_users: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let dept_group: Mutex<Option<String>> = Mutex::new(None);
    let directory_users: Mutex<Vec<String>> = Mutex::new(vec![ghost.clone()]);

    let body = async {
        fx.install_and_serve(&realm).await;
        let org_path = authz::org_group::org_group_path(TENANT, TENANT);
        let dept_path = format!("{org_path}/{dept}");
        let dept_id = admin
            .ensure_group_path(&["tenants", TENANT, TENANT, &dept])
            .await
            .expect("dept group");
        *dept_group.lock().unwrap() = Some(dept_id.clone());
        let org_id = admin
            .find_group_path(&["tenants", TENANT, TENANT])
            .await
            .expect("org group")
            .expect("org group")
            .id;

        // 組織と部署のグループに入り、属性 roles に engineering と attr_role を持つ人（あとでグループと attr_role から外す）。
        let leaver = create_user(
            &realm,
            &admin,
            &leaver_name,
            &[org_path.clone(), dept_path.clone()],
            &["engineering", &attr_role],
        )
        .await;
        kc_users.lock().unwrap().push(leaver.clone());
        directory_users.lock().unwrap().push(leaver.clone());
        // グループに入っていない人（既定の組織の member を手で書いた構成・前は a-corp にいた）。
        let loner = create_user(&realm, &admin, &loner_name, &[], &[]).await;
        kc_users.lock().unwrap().push(loner.clone());
        directory_users.lock().unwrap().push(loner.clone());

        // leaver のログインの同期（Keycloak の所属グループ）。部署はログインの同期と同じく台帳と写しに記録する。
        // engineering は属性 roles のロールだが、写しには部署と記録しておく（dev の seed と同じ形・属性で守られる）。
        // attr_role は属性 roles 由来と記録する（ログインの同期が属性 roles から付けたもの）。
        let groups = realm.user_group_paths(&leaver_name).await;
        assert!(groups.contains(&dept_path), "{groups:?}");
        let principal = Principal {
            kind: PrincipalKind::User,
            id: leaver.clone(),
            email: None,
            groups,
            roles: vec!["engineering".into(), attr_role.clone()],
            tenant_id: Some(TENANT.into()),
        };
        assert!(
            sync_org_member(&*authz, &principal, TENANT, GroupLayout::TenantScoped)
                .await
                .granted
        );
        directory
            .ensure_user(&leaver, TENANT, TENANT, None)
            .await
            .unwrap();
        directory
            .replace_user_roles(
                TENANT,
                TENANT,
                &leaver,
                &[
                    (dept_role.clone(), true),
                    ("engineering".into(), true),
                    (attr_role.clone(), false),
                ],
            )
            .await
            .unwrap();
        for role in [
            dept_role.as_str(),
            "engineering",
            manual_role.as_str(),
            attr_role.as_str(),
        ] {
            authz
                .write_tuple(&ns.user(&leaver), Relation::Member, &ns.role(role))
                .await
                .unwrap();
        }
        // loner: 既定の組織と a-corp の member、a-corp の admin。ghost: Keycloak にいない人の a-corp の member。
        directory
            .ensure_user(&loner, TENANT, TENANT, None)
            .await
            .unwrap();
        for (r, o) in [
            (Relation::Member, ns.organization(DEFAULT_ORG)),
            (Relation::Member, org.clone()),
            (Relation::Admin, org.clone()),
        ] {
            authz.write_tuple(&ns.user(&loner), r, &o).await.unwrap();
        }
        directory
            .ensure_user(&ghost, TENANT, TENANT, None)
            .await
            .unwrap();
        authz
            .write_tuple(&ns.user(&ghost), Relation::Member, &org)
            .await
            .unwrap();

        // アプリのトークン（leaver はこの後 shiki にログインしない）。
        let token = realm
            .user_token(&fx.b1, SHELL_CALLBACK, &leaver_name, "openid data.read")
            .await
            .expect("token");
        let (s, body) = fx.whoami(&token).await;
        assert_eq!(s, 200, "{body}");

        // 1・2・3. グループに入っている人は何も外れない。グループの無い人は既定の組織だけ残る。いない人は外さない。
        sqlx::query("DELETE FROM membership_reconcile_cursor")
            .execute(&fx.pool)
            .await
            .unwrap();
        let report = sweep(&deps).await;
        assert!(report.not_found >= 1, "{report:?}");
        assert!(report.revoked_orgs >= 1, "{report:?}");
        assert!(has(&*authz, &ns, &leaver, Relation::Member, &org).await);
        assert!(
            has(
                &*authz,
                &ns,
                &leaver,
                Relation::Member,
                &ns.role(&dept_role)
            )
            .await
        );
        assert!(
            has(
                &*authz,
                &ns,
                &leaver,
                Relation::Member,
                &ns.role(&attr_role)
            )
            .await,
            "属性 roles にあるロールは、所属グループに無くても残す"
        );
        assert!(
            has(
                &*authz,
                &ns,
                &loner,
                Relation::Member,
                &ns.organization(DEFAULT_ORG)
            )
            .await
        );
        assert!(
            !has(&*authz, &ns, &loner, Relation::Member, &org).await,
            "グループの無い人の a-corp は外れる"
        );
        assert!(
            has(&*authz, &ns, &loner, Relation::Admin, &org).await,
            "admin には触れない"
        );
        assert!(
            has(&*authz, &ns, &ghost, Relation::Member, &org).await,
            "Keycloak にいない人は外さない"
        );
        let (s, body) = fx.whoami(&token).await;
        assert_eq!(s, 200, "{body}");
        // ログインの同期も、グループの無い人の既定の組織の member を残す（BUG-200）。
        let loner_principal = Principal {
            kind: PrincipalKind::User,
            id: loner.clone(),
            email: None,
            groups: realm.user_group_paths(&loner_name).await,
            roles: vec![],
            tenant_id: Some(TENANT.into()),
        };
        let synced =
            sync_org_member(&*authz, &loner_principal, TENANT, GroupLayout::TenantScoped).await;
        assert_eq!(synced.desired, None);
        assert!(synced.revoked.is_empty(), "{synced:?}");
        assert!(
            has(
                &*authz,
                &ns,
                &loner,
                Relation::Member,
                &ns.organization(DEFAULT_ORG)
            )
            .await
        );

        // 4. leaver を組織と部署のグループから外し、属性 roles から attr_role を外す（shiki にはログインしない）。
        admin
            .remove_user_from_group(&leaver, &org_id)
            .await
            .unwrap();
        admin
            .remove_user_from_group(&leaver, &dept_id)
            .await
            .unwrap();
        set_roles_attribute(&realm, &leaver, &["engineering"]).await;
        // Keycloak を読めない（provisioner の資格情報の誤り）ときは、照合が失敗して何も外さない。
        let broken = ReconcileDeps {
            auth: auth_config(&realm, "wrong-secret"),
            ..deps.clone()
        };
        assert!(
            run_once(&broken).await.is_err(),
            "トークンを取れない回は失敗にする"
        );
        assert!(has(&*authz, &ns, &leaver, Relation::Member, &org).await);
        let (s, body) = fx.whoami(&token).await;
        assert_eq!(s, 200, "{body}");

        // 照合で外れる。同じトークンでもゲートウェイは 403。
        let report = sweep(&deps).await;
        assert!(
            report.revoked_orgs >= 1 && report.revoked_roles >= 2,
            "{report:?}"
        );
        assert!(
            !has(
                &*authz,
                &ns,
                &leaver,
                Relation::Member,
                &ns.role(&attr_role)
            )
            .await,
            "属性 roles から外したロールは、ログインしなくても外れる（D-140）"
        );
        assert!(!has(&*authz, &ns, &leaver, Relation::Member, &org).await);
        assert!(
            !has(
                &*authz,
                &ns,
                &leaver,
                Relation::Member,
                &ns.role(&dept_role)
            )
            .await
        );
        assert!(
            has(
                &*authz,
                &ns,
                &leaver,
                Relation::Member,
                &ns.role("engineering")
            )
            .await,
            "属性 roles にあるロールは残す"
        );
        assert!(
            has(
                &*authz,
                &ns,
                &leaver,
                Relation::Member,
                &ns.role(&manual_role)
            )
            .await,
            "記録の無いロール（手で書いたタプル）は残す"
        );
        let recorded: Vec<String> = sqlx::query_scalar(
            "SELECT role_id FROM directory_user_role WHERE tenant_id = $1 AND user_id = $2 ORDER BY role_id",
        )
        .bind(TENANT)
        .bind(&leaver)
        .fetch_all(&fx.pool)
        .await
        .unwrap();
        assert_eq!(recorded, ["engineering"], "外した部署は写しからも消す");
        let (s, body) = fx.whoami(&token).await;
        assert_eq!(s, 403, "{body}");
        assert_eq!(
            body["error"],
            "権限がありません: このアプリを使えるのは、アプリをインストールした組織のメンバーだけです"
        );

        // 5. サーバーが何台あっても、照合は全体で間隔に 1 回だけ進める。最後まで読んだ後も行は残り（位置だけ外す）、
        //    前の回（どのサーバーの回でも）から間隔が経つまでは何もしない・位置も動かさない。
        let position = || async {
            sqlx::query_as::<_, (Option<String>, Option<String>)>(
                "SELECT cursor_tenant_id, cursor_user_id FROM membership_reconcile_cursor WHERE id = 1",
            )
            .fetch_one(&fx.pool)
            .await
            .expect("最後まで読んでも行は残る")
        };
        assert_eq!(position().await, (None, None), "最後まで読んだら頭から");
        let hourly = ReconcileDeps {
            interval: Duration::from_secs(3_600),
            ..deps.clone()
        };
        assert!(
            run_once(&hourly).await.expect("run_once").is_none(),
            "前の回のすぐ後は進めない"
        );
        assert_eq!(position().await, (None, None));
        sqlx::query(
            "UPDATE membership_reconcile_cursor SET last_run_at = now() - interval '2 hours'",
        )
        .execute(&fx.pool)
        .await
        .unwrap();
        let report = run_once(&hourly).await.expect("run_once");
        assert!(report.is_some_and(|r| r.read > 0), "間隔が経ったら進める");
        assert!(
            run_once(&hourly).await.expect("run_once").is_none(),
            "進めた回から、また間隔を待つ"
        );
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;

    // 片付け（互いに独立させる）。
    let created = kc_users.lock().unwrap().clone();
    for id in &created {
        if let Err(e) = admin.delete_user(id).await {
            eprintln!("Keycloak の利用者を消せません: {e}");
        }
    }
    let group = dept_group.lock().unwrap().clone();
    if let Some(id) = group {
        if let Err(e) = admin.delete_group(&id).await {
            eprintln!("Keycloak のグループを消せません: {e}");
        }
    }
    let users = directory_users.lock().unwrap().clone();
    for sql in [
        "DELETE FROM directory_user_role WHERE tenant_id = $1 AND user_id = ANY($2)",
        "DELETE FROM directory_user WHERE tenant_id = $1 AND user_id = ANY($2)",
    ] {
        if let Err(e) = sqlx::query(sql)
            .bind(TENANT)
            .bind(&users)
            .execute(&fx.pool)
            .await
        {
            eprintln!("{sql}: {e}");
        }
    }
    if let Err(e) = sqlx::query("DELETE FROM membership_reconcile_cursor")
        .execute(&fx.pool)
        .await
    {
        eprintln!("cursor: {e}");
    }
    fx.cleanup(&realm).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// 試しの Keycloak（token と所属グループだけ）。`bad-` で始まる人は 500、`forbidden-` は 403、`gone-` は 404、ほかは
/// グループ無し。
/// token は `token_ok` が false のあいだ 503。base の URL を返す。
async fn mock_keycloak(token_ok: Arc<AtomicBool>) -> String {
    use axum::{extract::Path, http::StatusCode, routing, Json, Router};
    let app = Router::new()
        .route(
            "/realms/shiki/protocol/openid-connect/token",
            routing::post(move || {
                let ok = token_ok.load(Ordering::SeqCst);
                async move {
                    if ok {
                        (StatusCode::OK, Json(json!({ "access_token": "t" })))
                    } else {
                        (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})))
                    }
                }
            }),
        )
        .route(
            "/admin/realms/shiki/users/{id}/groups",
            routing::get(|Path(id): Path<String>| async move {
                if id.starts_with("bad-") {
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})))
                } else if id.starts_with("forbidden-") {
                    (StatusCode::FORBIDDEN, Json(json!({})))
                } else if id.starts_with("gone-") {
                    (StatusCode::NOT_FOUND, Json(json!({})))
                } else {
                    (StatusCode::OK, Json(json!([])))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keycloak_failures_neither_revoke_nor_stall_the_sweep() {
    let (Ok(db_url), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL・OPENFGA_TEST_URL のどちらかが無いためスキップ");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("membership-reconcile-mock-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA");
    let store_id = fga.store_id().to_string();
    // 続きの位置はテナントの持ち物ではない（tenant_id の列を持たない）。テナントの撤去と移行が集めると、照合が
    // そのテナントの人を読んでいる間は retenant が移行を断ってしまう（migration 0144）。
    let scoped = storage::tenant_scope::tenant_scoped_tables(&pool)
        .await
        .expect("テナントの表");
    assert!(
        !scoped.iter().any(|t| t == "membership_reconcile_cursor"),
        "{scoped:?}"
    );
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let token_ok = Arc::new(AtomicBool::new(true));
    let base = mock_keycloak(Arc::clone(&token_ok)).await;
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
        auth: auth_for(
            format!("{base}/realms/shiki"),
            format!("{base}/admin/realms/shiki"),
            "p",
            "s",
        ),
        batch_size: 10,
        interval: Duration::ZERO,
    };
    let ns = Namespace::for_tenant(TENANT);
    let org = ns.organization("x");
    let page = |names: &[&str]| -> Vec<DirectoryEntry> {
        names
            .iter()
            .map(|n| DirectoryEntry {
                tenant_id: TENANT.into(),
                user_id: (*n).into(),
            })
            .collect()
    };

    let body = async {
        for user in ["bad-1", "bad-2", "bad-3", "good-1", "gone-1"] {
            authz
                .write_tuple(&ns.user(user), Relation::Member, &org)
                .await
                .unwrap();
        }
        let admin = KeycloakAdmin::from_config(&deps.http, &deps.auth).unwrap();

        // 1. 3 人続けて失敗しても、Keycloak に届く（トークンを取り直せる）なら先へ進む。失敗した人といない人は外さない。
        let mut kc = admin.session().await.unwrap();
        let mut report = ReconcileReport::default();
        let users = page(&["bad-1", "bad-2", "bad-3", "good-1", "gone-1"]);
        let done = reconcile_page(&deps, &mut kc, &users, &mut report)
            .await
            .unwrap();
        assert_eq!(done, 5, "{report:?}");
        assert_eq!(
            (report.failed, report.not_found, report.revoked_orgs),
            (3, 1, 1)
        );
        for user in ["bad-1", "bad-2", "bad-3", "gone-1"] {
            assert!(
                has(&*authz, &ns, user, Relation::Member, &org).await,
                "{user}"
            );
        }
        assert!(!has(&*authz, &ns, "good-1", Relation::Member, &org).await);

        // 2. ページの終わりで失敗しても、届くなら先へ進む（同じ人で毎回止まらない）。
        authz
            .write_tuple(&ns.user("good-1"), Relation::Member, &org)
            .await
            .unwrap();
        let mut report = ReconcileReport::default();
        let done = reconcile_page(&deps, &mut kc, &page(&["good-1", "bad-1"]), &mut report)
            .await
            .unwrap();
        assert_eq!(done, 2, "{report:?}");
        assert!(!has(&*authz, &ns, "good-1", Relation::Member, &org).await);

        // 2b. 403（provisioner に利用者を読む権限が無い）は、1 人目でその回を止める（位置を進めない・誰も外さない）。
        for user in ["forbidden-1", "good-1"] {
            authz
                .write_tuple(&ns.user(user), Relation::Member, &org)
                .await
                .unwrap();
        }
        let mut report = ReconcileReport::default();
        let done = reconcile_page(
            &deps,
            &mut kc,
            &page(&["forbidden-1", "good-1"]),
            &mut report,
        )
        .await
        .unwrap();
        assert_eq!((done, report.failed), (0, 1), "{report:?}");
        for user in ["forbidden-1", "good-1"] {
            assert!(
                has(&*authz, &ns, user, Relation::Member, &org).await,
                "{user}"
            );
        }

        // 3. Keycloak に届かなくなったら（トークンも取れない）その回を止め、1 人も終えたことにしない。後ろの人も外さない。
        authz
            .write_tuple(&ns.user("good-1"), Relation::Member, &org)
            .await
            .unwrap();
        token_ok.store(false, Ordering::SeqCst);
        let mut report = ReconcileReport::default();
        let users = page(&["bad-1", "bad-2", "bad-3", "good-1"]);
        let done = reconcile_page(&deps, &mut kc, &users, &mut report)
            .await
            .unwrap();
        assert_eq!(done, 0, "{report:?}");
        assert_eq!(report.failed, 3);
        assert!(has(&*authz, &ns, "good-1", Relation::Member, &org).await);
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
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
