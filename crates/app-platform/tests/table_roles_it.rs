//! アプリのテーブルを使える部署・グループを、インストールの後から足す・外す（GAP-68）の結合テスト。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）。どちらかが無ければスキップする。
//! Keycloak は使わない（クライアントの登録は省く）。作った行と OpenFGA のストアはテストの最後に消す。
//!
//! 1. インストールの要求（viewer_roles・submitter_roles）が付けた部署・グループを、アプリのすべてのテーブルについて読める。
//! 2. 足す・外すと、部署のメンバーの実際の権限（OpenFGA の check）が変わる。監査に足した・外したものが残る。
//! 3. 変えられるのはインストールできる人（アプリの owner）だけ。ほかの人は 403 で、変更の拒否は監査に deny で残る。
//! 4. ディレクトリに居ない部署は足せない。同じ部署を同じ役割で足して外すことはできない。
//! 5. インストールの要求の部署の id が壊れていれば、テーブルを作る前に断る。アンインストールしたアプリは 404。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::large_futures
)]

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use app_platform::{
    AppPlatformError, AppTableRoles, AppTableRolesChange, Budget, InstallRequest, InstallService,
    ManifestTable, MiniAppCodeStore, MiniAppManifest, Registry, TrustTier,
};
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, Consistency, Principal, Relation,
};
use data::{DataStore, FieldDef, FieldType, RefResolver, TableSchema};
use futures::FutureExt as _;
use uuid::Uuid;

/// ディレクトリに居る部署は sales・acct・staff（ghost は居ない）。
struct Directory;

#[async_trait::async_trait]
impl RefResolver for Directory {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(matches!(id, "sales" | "acct" | "staff"))
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

fn manifest(name: &str) -> MiniAppManifest {
    let table = |n: &str| ManifestTable {
        name: n.into(),
        schema: TableSchema {
            fields: vec![FieldDef::new("title", FieldType::Text)],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        },
    };
    MiniAppManifest {
        name: name.into(),
        version: "1.0.0".into(),
        description: "経費の申請".into(),
        requested_scopes: vec!["data.read".into(), "data.write".into()],
        tools: vec![],
        tables: vec![table("経費"), table("経費の明細")],
        workflows: vec![],
        budget: Budget::default(),
        frontend: None,
        server: None,
        trust_tier: TrustTier::InHouse,
    }
}

fn req(name: &str, viewer: &[&str], submitter: &[&str]) -> InstallRequest {
    let v = |x: &[&str]| x.iter().map(|s| (*s).to_string()).collect();
    InstallRequest {
        name: name.into(),
        version: "1.0.0".into(),
        granted_scopes: vec!["data.read".into()],
        viewer_roles: v(viewer),
        editor_roles: vec![],
        submitter_roles: v(submitter),
    }
}

fn roles(viewer: &[&str], editor: &[&str], submitter: &[&str]) -> AppTableRoles {
    let v = |x: &[&str]| x.iter().map(|s| (*s).to_string()).collect();
    AppTableRoles {
        viewer_roles: v(viewer),
        editor_roles: v(editor),
        submitter_roles: v(submitter),
    }
}

#[tokio::test]
async fn table_roles_can_be_changed_after_install() {
    let (Ok(db_url), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL・OPENFGA_TEST_URL のどちらかが無いためスキップ");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
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
            store_name: format!("table-roles-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA");
    let store_id = fga.store_id().to_string();
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), authz.clone()));
    let code = Arc::new(MiniAppCodeStore::new(
        Arc::clone(&artifacts),
        Registry::new(pool.clone()),
    ));
    let data = Arc::new(DataStore::new(
        pool.clone(),
        authz.clone(),
        Arc::new(Directory),
    ));
    let installs = InstallService::new(
        pool.clone(),
        Registry::new(pool.clone()),
        Arc::clone(&code),
        Arc::clone(&data),
        authz.clone(),
        None,
        vec![],
    );
    let tenant = format!("t-{}", Uuid::new_v4());
    let owner = ctx(&tenant, "owner");
    let other = ctx(&tenant, "someone");
    let ns = authz::Namespace::for_tenant(&tenant);

    let body = async {
        let name = format!("expense-{}", Uuid::new_v4().simple());
        let app_id = code.create(&owner, &manifest(&name), None).await.unwrap();
        code.publish(&owner, app_id, None, None, None)
            .await
            .unwrap();
        // 5. 部署の id が壊れていれば、テーブルを作る前に断る。
        let bad = installs
            .install(&owner, req(&name, &["a#b"], &[]), None)
            .await;
        assert!(matches!(bad, Err(AppPlatformError::Invalid(_))), "{bad:?}");
        assert!(data
            .tables_owned_by_app(&owner, app_id)
            .await
            .unwrap()
            .is_empty());

        let installed = installs
            .install(&owner, req(&name, &["sales"], &["staff"]), None)
            .await
            .expect("install");
        assert_eq!(installed.table_ids.len(), 2);

        // 1. インストールが付けた部署・グループを、アプリのテーブルと一緒に読める。
        let access = installs.table_roles(&owner, app_id).await.unwrap();
        let names: Vec<&str> = access.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["経費", "経費の明細"]);
        assert_eq!(access.roles, roles(&["sales"], &[], &["staff"]));

        // 部署のメンバー（実際の権限は OpenFGA の check で見る）。
        for (user, role) in [
            ("u-sales", "sales"),
            ("u-acct", "acct"),
            ("u-staff", "staff"),
        ] {
            authz
                .write_tuple(&ns.user(user), Relation::Member, &ns.role(role))
                .await
                .unwrap();
        }
        let can = |user: &'static str, relation: Relation| {
            let authz = authz.clone();
            let tables = installed.table_ids.clone();
            let ns_tenant = tenant.clone();
            async move {
                let ns = authz::Namespace::for_tenant(&ns_tenant);
                let mut all = true;
                for t in tables {
                    all &= authz
                        .check(
                            &ns.user(user),
                            relation,
                            &ns.data_table(&t.to_string()),
                            Consistency::HigherConsistency,
                        )
                        .await
                        .unwrap();
                }
                all
            }
        };
        assert!(can("u-sales", Relation::Viewer).await);
        assert!(!can("u-acct", Relation::Editor).await);

        // 3. 変えられるのはアプリの owner だけ。ほかの人の変更は 403 で、監査に deny が残る。
        assert!(matches!(
            installs.table_roles(&other, app_id).await,
            Err(AppPlatformError::Forbidden)
        ));
        let change = AppTableRolesChange {
            add: roles(&[], &["acct"], &[]),
            remove: roles(&["sales"], &[], &[]),
        };
        assert!(matches!(
            installs
                .update_table_roles(&other, app_id, change.clone(), None)
                .await,
            Err(AppPlatformError::Forbidden)
        ));
        let denied: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = 'someone' \
               AND action = 'app.table_roles.update' AND decision = 'deny'",
        )
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(denied, 1);
        assert!(
            can("u-sales", Relation::Viewer).await,
            "断った変更は効かない"
        );

        // 2. 足す（acct を編集できる人に）・外す（sales を見られる人から）。すべてのテーブルに効く。
        let after = installs
            .update_table_roles(&owner, app_id, change, Some("trace-gap68"))
            .await
            .expect("update");
        assert_eq!(after.roles, roles(&[], &["acct"], &["staff"]));
        assert!(can("u-acct", Relation::Editor).await);
        assert!(!can("u-sales", Relation::Viewer).await);
        assert!(
            can("u-staff", Relation::Submitter).await,
            "触らない部署はそのまま"
        );
        let (meta, trace): (serde_json::Value, Option<String>) = sqlx::query_as(
            "SELECT metadata, trace_id FROM audit_log WHERE tenant_id = $1 AND actor = 'owner' \
               AND action = 'app.table_roles.update' AND decision = 'allow'",
        )
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(trace.as_deref(), Some("trace-gap68"));
        assert_eq!(meta["add"]["editor_roles"], serde_json::json!(["acct"]));
        assert_eq!(meta["remove"]["viewer_roles"], serde_json::json!(["sales"]));
        assert_eq!(meta["tables"].as_array().unwrap().len(), 2);

        // 4. ディレクトリに居ない部署は足せない。同じ部署を同じ役割で足して外せない。どちらも何も変えない。
        for bad in [
            AppTableRolesChange {
                add: roles(&["ghost"], &[], &[]),
                remove: AppTableRoles::default(),
            },
            AppTableRolesChange {
                add: roles(&["sales"], &[], &[]),
                remove: roles(&["sales"], &[], &[]),
            },
        ] {
            assert!(matches!(
                installs.update_table_roles(&owner, app_id, bad, None).await,
                Err(AppPlatformError::Invalid(_))
            ));
        }
        assert_eq!(
            installs.table_roles(&owner, app_id).await.unwrap().roles,
            roles(&[], &["acct"], &["staff"])
        );

        // アプリのテーブルは、テーブルの共有では変えられないまま（変える口はここだけ）。
        let shared = data
            .share_table(
                &owner,
                installed.table_ids[0],
                &storage::ShareTarget::Role { id: "sales".into() },
                data::TableShareRole::Viewer,
                None,
            )
            .await;
        assert!(
            matches!(shared, Err(data::DataError::Forbidden)),
            "{shared:?}"
        );

        // 5. アンインストールしたアプリは 404。
        installs.uninstall(&owner, app_id, None).await.unwrap();
        assert!(matches!(
            installs.table_roles(&owner, app_id).await,
            Err(AppPlatformError::NotFound)
        ));
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;

    for sql in [
        "DELETE FROM app_installation WHERE tenant_id = $1",
        "DELETE FROM registry_entry WHERE tenant_id = $1",
        "DELETE FROM artifact_version WHERE tenant_id = $1",
        "DELETE FROM artifact WHERE tenant_id = $1",
        "DELETE FROM storage_event_outbox WHERE tenant_id = $1",
        "DELETE FROM audit_log WHERE tenant_id = $1",
    ] {
        sqlx::query(sql)
            .bind(&tenant)
            .execute(&pool)
            .await
            .expect(sql);
    }
    data.purge_tenant(&tenant).await.expect("purge");
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
