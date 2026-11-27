//! コードアプリの公開の取り下げ（`InstallService::yank`・D-203・GAP-69b）の結合テスト。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）と実 OpenFGA（`OPENFGA_TEST_URL`）。持ち主の relation は
//! アーティファクトを作ったときに張られるもの、組織の管理者は `organization#admin` のタプルで確かめる（モデルは変えない）。
//!
//! - 持ち主と組織の管理者は取り下げられ、ほかの人は 403（deny を監査に残す）
//! - ほかの組織の行と skill の行は 404（取り下げの口はコードアプリだけ）
//! - 2 回目も成功し、`app.yank` の監査は 1 行だけ（チェーンにつながる）
//! - 取り下げた版は新しくインストールできず、入れてあるアプリは動き続ける

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::large_futures
)]

use std::sync::Arc;

use app_platform::{
    AppPlatformError, InstallRequest, InstallService, MiniAppCodeStore, MiniAppManifest,
    NewRegistryEntry, Registry, RegistryEntry, TrustTier,
};
use artifact::{ArtifactKind, ArtifactStore, NewArtifact};
use async_trait::async_trait;
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, Principal, Relation,
};
use data::{DataStore, RefResolver};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

#[path = "../../gui/tests/common/mod.rs"]
mod common;

const KIND: &str = "mini_app_code";

struct AnyRef;
#[async_trait]
impl RefResolver for AnyRef {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

struct Env {
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    artifacts: Arc<ArtifactStore>,
    code: Arc<MiniAppCodeStore>,
    installs: InstallService,
}

async fn setup() -> Option<Env> {
    let db_url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL")
        .ok()
        .or_else(|| {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            None
        })?;
    let fga_url = shiki_testenv::var("OPENFGA_TEST_URL").ok().or_else(|| {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        None
    })?;
    let pool = PgPoolOptions::new()
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
            base_url: fga_url,
            store_name: format!("yank-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), authz.clone()));
    let code = Arc::new(MiniAppCodeStore::new(
        Arc::clone(&artifacts),
        Registry::new(pool.clone()),
    ));
    let installs = InstallService::new(
        pool.clone(),
        Registry::new(pool.clone()),
        Arc::clone(&code),
        Arc::new(DataStore::new(
            pool.clone(),
            authz.clone(),
            Arc::new(AnyRef),
        )),
        authz.clone(),
        None,
        vec![],
    );
    Some(Env {
        pool,
        authz,
        artifacts,
        code,
        installs,
    })
}

fn ctx(tenant: &str, org: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 呼ぶ人を自分の組織（`ctx.org()`）の `relation`（member・admin）にする。
async fn join(env: &Env, c: &AuthContext, relation: Relation) {
    env.authz
        .write_tuple(&c.subject(), relation, &c.ns().organization(c.org()))
        .await
        .expect("組織のタプル");
}

/// テーブルも画面も持たないコードアプリを作って 1.0.0 を公開する（呼ぶ人がアプリの持ち主になる）。
async fn publish(env: &Env, c: &AuthContext, name: &str) -> (Uuid, RegistryEntry) {
    let manifest = MiniAppManifest {
        name: name.into(),
        version: "1.0.0".into(),
        description: "取り下げのテスト".into(),
        requested_scopes: vec!["data.read".into()],
        tools: vec![],
        tables: vec![],
        workflows: vec![],
        budget: app_platform::Budget::default(),
        frontend: None,
        server: None,
        trust_tier: TrustTier::InHouse,
    };
    let id = env.code.create(c, &manifest, None).await.expect("create");
    let entry = env
        .code
        .publish(c, id, None, None, None)
        .await
        .expect("publish");
    (id, entry)
}

async fn yankable(env: &Env, c: &AuthContext, entries: &[RegistryEntry]) -> Vec<bool> {
    env.installs.yankable(c, entries).await.expect("yankable")
}

type AuditRow = (String, String, String, String, bool, serde_json::Value);

/// テナントの `app.yank` の監査の行（actor・decision・object_type・object_id・chained・metadata）。
async fn yank_audit(pool: &PgPool, tenant: &str) -> Vec<AuditRow> {
    sqlx::query_as(
        "SELECT actor, decision, object_type, object_id, chained, metadata FROM audit_log \
         WHERE tenant_id = $1 AND action = 'app.yank' ORDER BY id",
    )
    .bind(tenant)
    .fetch_all(pool)
    .await
    .expect("監査の行")
}

fn row(
    actor: &str,
    decision: &str,
    app: Uuid,
    chained: bool,
    metadata: serde_json::Value,
) -> AuditRow {
    (
        actor.into(),
        decision.into(),
        "miniapp".into(),
        app.to_string(),
        chained,
        metadata,
    )
}

/// 本体を走らせ、落ちてもテナントの行を片付けてから落とす。
async fn run_then_purge<F: std::future::Future<Output = ()>>(pool: &PgPool, tenant: &str, body: F) {
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(body)).await;
    common::purge_tenant_rows(pool, tenant).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// 持ち主と組織の管理者は取り下げられ、ほかの人（組織のメンバー）は 403 で deny を残す。2 回目も成功で、`app.yank`
/// は取り下げたときだけ 1 行（チェーンにつなぐ・対象は公開の監査と同じ `miniapp` のアプリの id）。取り下げたものは
/// 公開の一覧から外れ、行は残る。
#[tokio::test]
async fn owner_and_org_admin_can_yank_and_others_are_refused() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = ctx(&tenant, "acme", "alice");
    let boss = ctx(&tenant, "acme", "boss");
    let bob = ctx(&tenant, "acme", "bob");
    let body = async {
        for c in [&alice, &boss, &bob] {
            join(&env, c, Relation::Member).await;
        }
        join(&env, &boss, Relation::Admin).await;
        let (app_a, entry_a) = publish(&env, &alice, "yank-a").await;
        let (app_b, entry_b) = publish(&env, &alice, "yank-b").await;
        let registry = env.installs.registry();

        // 画面の出し分け: 持ち主と管理者には出し、ほかの人には出さない。
        let listed = registry.list_installable(&bob, KIND, 200).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(yankable(&env, &alice, &listed).await, [true, true]);
        assert_eq!(yankable(&env, &boss, &listed).await, [true, true]);
        assert_eq!(yankable(&env, &bob, &listed).await, [false, false]);
        assert!(yankable(&env, &bob, &[]).await.is_empty());

        // ほかの人は 403。deny を残し、取り下げない。
        let refused = env.installs.yank(&bob, entry_a.id, None).await;
        assert!(
            matches!(refused, Err(AppPlatformError::Forbidden)),
            "{refused:?}"
        );
        assert_eq!(
            yank_audit(&env.pool, &tenant).await,
            [row(
                "bob",
                "deny",
                app_a,
                false,
                serde_json::json!({ "security": true })
            )]
        );
        let still = registry.get(&alice, KIND, "yank-a", "1.0.0").await.unwrap();
        assert!(!still.expect("行").yanked);

        // 持ち主は取り下げられる。2 回目も成功で、もう変えない（監査も残さない）。
        assert!(env
            .installs
            .yank(&alice, entry_a.id, None)
            .await
            .expect("持ち主"));
        assert!(!env
            .installs
            .yank(&alice, entry_a.id, None)
            .await
            .expect("2 回目"));
        // 組織の管理者は、ほかの人のアプリも取り下げられる。
        assert!(env
            .installs
            .yank(&boss, entry_b.id, None)
            .await
            .expect("管理者"));
        // 取り下げた後も、ほかの人は 403（取り下げたかどうかで答えを変えない）。
        let again = env.installs.yank(&bob, entry_b.id, None).await;
        assert!(
            matches!(again, Err(AppPlatformError::Forbidden)),
            "{again:?}"
        );

        let audit = yank_audit(&env.pool, &tenant).await;
        let meta = |name: &str| serde_json::json!({ "name": name, "version": "1.0.0" });
        assert_eq!(
            audit,
            [
                row(
                    "bob",
                    "deny",
                    app_a,
                    false,
                    serde_json::json!({ "security": true })
                ),
                row("alice", "allow", app_a, true, meta("yank-a")),
                row("boss", "allow", app_b, true, meta("yank-b")),
                row(
                    "bob",
                    "deny",
                    app_b,
                    false,
                    serde_json::json!({ "security": true })
                ),
            ]
        );

        // 公開の一覧から外れ、名前での解決（最新）にも出ない。行は残り、取り下げた印が付く。
        assert!(registry
            .list_installable(&alice, KIND, 200)
            .await
            .unwrap()
            .is_empty());
        assert!(registry
            .latest(&alice, KIND, "yank-a")
            .await
            .unwrap()
            .is_none());
        for name in ["yank-a", "yank-b"] {
            let kept = registry.get(&alice, KIND, name, "1.0.0").await.unwrap();
            assert!(kept.expect("行は残る").yanked, "{name}");
        }

        // チェーンは先頭から崩れていない（公開と取り下げの行）。
        let report = storage::audit_chain::AuditChainVerifier::new(env.pool.clone())
            .verify(&alice, None, 1000)
            .await
            .expect("verify");
        assert!(
            report.broken.is_none() && report.complete && report.checked >= 4,
            "{report:?}"
        );
    };
    run_then_purge(&env.pool, &tenant, body).await;
}

/// 取り下げの口は、呼ぶ人の組織のコードアプリの登録だけに届く。ほかの組織の行（アプリの持ち主・その組織の管理者でも）
/// と skill の行は 404 で、取り下げず、監査も残さない（D-203）。
#[tokio::test]
async fn yank_only_reaches_code_apps_of_the_callers_org() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = ctx(&tenant, "acme", "alice");
    let boss = ctx(&tenant, "acme", "boss");
    let alice_beta = ctx(&tenant, "beta", "alice");
    let boss_beta = ctx(&tenant, "beta", "boss");
    let body = async {
        join(&env, &boss, Relation::Admin).await;
        join(&env, &boss_beta, Relation::Admin).await;
        let (_, entry) = publish(&env, &alice, "org-app").await;

        // ほかの組織からは、持ち主でも管理者でも見つからない。
        for c in [&alice_beta, &boss_beta] {
            let r = env.installs.yank(c, entry.id, None).await;
            assert!(matches!(r, Err(AppPlatformError::NotFound)), "{r:?}");
        }

        // skill の登録は、持ち主でも組織の管理者でも見つからない（画面にも出さない）。
        let skill = env
            .artifacts
            .create(
                &alice,
                NewArtifact {
                    kind: ArtifactKind::Skill,
                    name: "yank-skill".into(),
                    body: serde_json::json!({ "description": "skill", "instructions": "手順" }),
                },
                None,
            )
            .await
            .expect("skill");
        let registry = env.installs.registry();
        let skill_entry = registry
            .publish(
                &alice,
                NewRegistryEntry {
                    artifact_kind: "skill",
                    name: "yank-skill",
                    version: "1",
                    artifact_id: skill.id,
                    artifact_version: 1,
                    manifest_digest: "digest",
                    trust_tier: "in_house",
                    signature: None,
                },
            )
            .await
            .expect("skill の登録");
        for c in [&alice, &boss] {
            let r = env.installs.yank(c, skill_entry.id, None).await;
            assert!(matches!(r, Err(AppPlatformError::NotFound)), "{r:?}");
        }
        assert_eq!(
            yankable(&env, &boss, std::slice::from_ref(&skill_entry)).await,
            [false]
        );
        assert_eq!(
            yankable(&env, &alice, std::slice::from_ref(&skill_entry)).await,
            [false]
        );

        // 無い id も 404。
        let r = env.installs.yank(&alice, Uuid::new_v4(), None).await;
        assert!(matches!(r, Err(AppPlatformError::NotFound)), "{r:?}");

        // どれも取り下げず、監査も残さない。
        let app = registry
            .get(&alice, KIND, "org-app", "1.0.0")
            .await
            .unwrap();
        assert!(!app.expect("行").yanked);
        let s = registry
            .get(&alice, "skill", "yank-skill", "1")
            .await
            .unwrap();
        assert!(!s.expect("skill の行").yanked);
        assert!(yank_audit(&env.pool, &tenant).await.is_empty());
    };
    run_then_purge(&env.pool, &tenant, body).await;
}

/// 取り下げた版は新しくインストールできない（409）。入れてあるアプリは動き続け（ゲートウェイと B1 の配信が引く有効な
/// インストールが残る）、インストール済みの一覧に「取り下げた版」と添える材料が引ける。
#[tokio::test]
async fn yanked_version_cannot_be_installed_and_the_installed_app_keeps_running() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = ctx(&tenant, "acme", "alice");
    let body = async {
        let (app, entry) = publish(&env, &alice, "running-app").await;
        let request = || InstallRequest {
            name: "running-app".into(),
            version: "1.0.0".into(),
            granted_scopes: vec!["data.read".into()],
            viewer_roles: vec![],
            editor_roles: vec![],
            submitter_roles: vec![],
        };
        env.installs
            .install(&alice, request(), None)
            .await
            .expect("install");
        let registry = env.installs.registry();
        let installed = [(app, "running-app".to_string(), "1.0.0".to_string())];
        assert!(registry
            .yanked_installs(&alice, &installed)
            .await
            .unwrap()
            .is_empty());
        assert!(registry
            .yanked_installs(&alice, &[])
            .await
            .unwrap()
            .is_empty());

        assert!(env
            .installs
            .yank(&alice, entry.id, None)
            .await
            .expect("yank"));

        // 新しいインストール（入れ直し）は取り下げた版を断る。
        let again = env.installs.install(&alice, request(), None).await;
        assert!(
            matches!(again, Err(AppPlatformError::Conflict(_))),
            "{again:?}"
        );

        // 入れてあるアプリは動き続ける（ゲートウェイは registry_entry を読まない）。
        let installations = env.installs.installations();
        let active = installations
            .resolve_active_by_app(&tenant, app)
            .await
            .unwrap()
            .expect("有効なインストールが残る");
        assert_eq!(active.installed_version, "1.0.0");
        let (served_tenant, _) = installations
            .resolve_active_by_app_global(app)
            .await
            .unwrap()
            .expect("B1 の配信が引くインストールも残る");
        assert_eq!(served_tenant, tenant);
        let listed = installations.list(&alice).await.unwrap();
        assert_eq!(listed.iter().map(|i| i.app_id).collect::<Vec<_>>(), [app]);

        // インストール済みの一覧に添える「取り下げた版」。ほかの版・ほかのアプリは含めない。
        let other = (
            Uuid::new_v4(),
            "running-app".to_string(),
            "1.0.0".to_string(),
        );
        let older = (app, "running-app".to_string(), "0.9.0".to_string());
        let yanked = registry
            .yanked_installs(&alice, &[installed[0].clone(), other, older])
            .await
            .unwrap();
        assert_eq!(yanked.into_iter().collect::<Vec<_>>(), installed);
    };
    run_then_purge(&env.pool, &tenant, body).await;
}
