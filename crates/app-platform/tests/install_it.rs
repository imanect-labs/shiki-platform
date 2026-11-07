//! 同意インストール／プロビジョン／署名の結合テスト（Task 9.13b 受け入れ条件）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）。authz は記録スタブ（tuple 書込/撤去を捕捉）。
//! Keycloak は未配線（client 登録スキップ経路・登録本体は 9.6 の mock IT が担う）。
//!
//! - publish → 同意インストール → data_table 自動プロビジョン（app_id 束縛）＋FGA tuple
//! - granted ⊄ requested 拒否／owner でない呼出の拒否
//! - first-party = 署名必須（trusted key 検証・改竄拒否）
//! - オフライン import（署名検証のみで登録・不変 publish）
//! - 部分失敗の補償（残骸なし）・アンインストール（失効＋archive＋tuple 撤去）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::large_futures
)]

use std::sync::{Arc, Mutex};

use app_platform::{
    sign_manifest, InstallRequest, InstallService, MiniAppCodeStore, MiniAppManifest, Registry,
    TrustTier, TrustedKeyStore,
};
use artifact::ArtifactStore;
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use data::{DataStore, FieldDef, FieldType, RefResolver, TableSchema};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

#[path = "../../gui/tests/common/mod.rs"]
mod common;

/// 記録スタブ: check は `deny` で切替・write/delete を捕捉する。
#[derive(Default)]
struct RecordingAuthz {
    deny: bool,
    writes: Mutex<Vec<(String, String, String)>>,
    deleted_objects: Mutex<Vec<String>>,
}

#[async_trait]
impl AuthzClient for RecordingAuthz {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(!self.deny)
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.writes.lock().unwrap().push((
            s.as_str().to_string(),
            r.as_str().to_string(),
            o.as_str().to_string(),
        ));
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
    async fn delete_object_tuples(&self, o: &FgaObject) -> Result<u32, AuthzError> {
        self.deleted_objects
            .lock()
            .unwrap()
            .push(o.as_str().to_string());
        Ok(1)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

struct FixedResolver;
#[async_trait]
impl RefResolver for FixedResolver {
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

async fn setup() -> Option<PgPool> {
    let url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
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

fn table_schema() -> TableSchema {
    let mut title = FieldDef::new("title", FieldType::Text);
    title.required = true;
    TableSchema {
        fields: vec![title],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

fn manifest(name: &str, tier: TrustTier, tables: Vec<&str>) -> MiniAppManifest {
    MiniAppManifest {
        name: name.into(),
        version: "1.0.0".into(),
        description: "テストアプリ".into(),
        requested_scopes: vec!["data.read".into(), "data.write".into(), "llm.invoke".into()],
        tools: vec!["doc_search".into()],
        tables: tables
            .into_iter()
            .map(|n| app_platform::ManifestTable {
                name: n.into(),
                schema: table_schema(),
            })
            .collect(),
        workflows: vec![],
        budget: app_platform::Budget {
            models: vec!["gpt-a".into()],
            daily_usd_micros: Some(1_000_000),
            max_tokens: Some(2048),
        },
        frontend: None,
        server: None,
        trust_tier: tier,
    }
}

struct Harness {
    authz: Arc<RecordingAuthz>,
    code: Arc<MiniAppCodeStore>,
    installs: InstallService,
    keys: TrustedKeyStore,
}

fn harness(pool: PgPool, deny: bool) -> Harness {
    harness_with_oauth(pool, deny, None, vec![])
}

/// Keycloak の管理（provisioner）を配線した harness（クライアントの登録まで通す・BUG-172）。
fn harness_with_oauth(
    pool: PgPool,
    deny: bool,
    oauth: Option<app_gateway::OAuthClient>,
    b1_redirects: Vec<String>,
) -> Harness {
    let authz = Arc::new(RecordingAuthz {
        deny,
        ..RecordingAuthz::default()
    });
    let authz_dyn: Arc<dyn AuthzClient> = authz.clone();
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), authz_dyn.clone()));
    let code = Arc::new(MiniAppCodeStore::new(
        Arc::clone(&artifacts),
        Registry::new(pool.clone()),
    ));
    let data = Arc::new(DataStore::new(
        pool.clone(),
        authz_dyn.clone(),
        Arc::new(FixedResolver),
    ));
    let installs = InstallService::new(
        pool.clone(),
        Registry::new(pool.clone()),
        Arc::clone(&code),
        data,
        authz_dyn,
        oauth,
        b1_redirects,
    );
    Harness {
        keys: TrustedKeyStore::new(pool),
        authz,
        code,
        installs,
    }
}

fn install_req(name: &str, granted: &[&str]) -> InstallRequest {
    InstallRequest {
        name: name.into(),
        version: "1.0.0".into(),
        granted_scopes: granted.iter().map(|s| (*s).to_string()).collect(),
        viewer_roles: vec!["sales".into()],
        editor_roles: vec![],
        // 申請のアプリで社員に渡す「追加だけ」（D-113）。
        submitter_roles: vec!["staff".into()],
    }
}

#[tokio::test]
async fn install_provisions_tables_tuples_and_pins() {
    let Some(pool) = setup().await else { return };
    let h = harness(pool.clone(), false);
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");

    let m = manifest("expense", TrustTier::InHouse, vec!["expense"]);
    let id = h.code.create(&c, &m, None).await.expect("create");
    h.code
        .publish(&c, id, None, None, None)
        .await
        .expect("publish");

    // granted ⊄ requested → 拒否。
    let err = h
        .installs
        .install(&c, install_req("expense", &["rag.query"]), None)
        .await;
    assert!(err.is_err(), "{err:?}");

    // 同意インストール。
    let installed = h
        .installs
        .install(
            &c,
            install_req("expense", &["data.read", "data.write"]),
            None,
        )
        .await
        .expect("install");
    assert_eq!(installed.installation.app_id, id);
    assert_eq!(installed.table_ids.len(), 1);
    // AiPin がマニフェスト Budget/tools から焼き込まれている。
    assert_eq!(installed.installation.ai.budget_models, vec!["gpt-a"]);
    assert_eq!(
        installed.installation.ai.budget_daily_usd_micros,
        Some(1_000_000)
    );
    assert_eq!(installed.installation.ai.agent_tools, vec!["doc_search"]);

    // data_table が app_id 束縛でプロビジョンされている。
    let (app_id,): (Option<Uuid>,) =
        sqlx::query_as("SELECT app_id FROM data_table WHERE tenant_id = $1 AND id = $2")
            .bind(&tenant)
            .bind(installed.table_ids[0])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(app_id, Some(id));

    // FGA tuple: owner@miniapp ＋ viewer@role#member ＋ submitter@role#member。
    let writes = h.authz.writes.lock().unwrap().clone();
    let table_obj = format!("data_table:{tenant}|{}", installed.table_ids[0]);
    assert!(
        writes
            .iter()
            .any(|(s, r, o)| s == &format!("miniapp:{tenant}|{id}")
                && r == "owner"
                && o == &table_obj),
        "{writes:?}"
    );
    assert!(
        writes
            .iter()
            .any(|(s, r, o)| s == &format!("role:{tenant}|sales#member")
                && r == "viewer"
                && o == &table_obj),
        "{writes:?}"
    );
    assert!(
        writes
            .iter()
            .any(|(s, r, o)| s == &format!("role:{tenant}|staff#member")
                && r == "submitter"
                && o == &table_obj),
        "追加だけのロールにも付与する: {writes:?}"
    );

    // テーブルの ReBAC を変えたので、配信の認可材料の世代が進んでいる（購読を次のバッチで引き直させる・
    // Task 15.7 の 15.6 からの申し送り）。
    let epoch_of = |pool: PgPool, tenant: String, table: Uuid| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT epoch FROM data_stream_authz_epoch WHERE tenant_id = $1 AND scope = $2",
        )
        .bind(&tenant)
        .bind(table)
        .fetch_optional(&pool)
        .await
        .unwrap()
    };
    let installed_epoch = epoch_of(pool.clone(), tenant.clone(), installed.table_ids[0]).await;
    assert!(
        installed_epoch.is_some(),
        "インストールでテーブルの世代が進む"
    );

    // outbox に app.installed が出ている。
    let (events,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM storage_event_outbox \
         WHERE tenant_id = $1 AND payload->>'event_type' = 'app.installed'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 1);

    // アンインストール: 失効＋テーブル archive＋tuple 撤去。
    h.installs.uninstall(&c, id, None).await.expect("uninstall");
    let (deleted,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM data_table \
         WHERE tenant_id = $1 AND app_id = $2 AND deleted_at IS NOT NULL",
    )
    .bind(&tenant)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(deleted, 1);
    let deleted_objs = h.authz.deleted_objects.lock().unwrap().clone();
    assert!(deleted_objs.contains(&table_obj), "{deleted_objs:?}");
    // tuple 撤去でも世代が進む（開いている購読を落とす）。
    let after = epoch_of(pool.clone(), tenant.clone(), installed.table_ids[0]).await;
    assert!(after > installed_epoch, "{after:?} > {installed_epoch:?}");
    let (active,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM app_installation \
         WHERE tenant_id = $1 AND app_id = $2 AND status = 'active'",
    )
    .bind(&tenant)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active, 0);
}

#[tokio::test]
async fn non_owner_cannot_install() {
    let Some(pool) = setup().await else { return };
    // publish は allow ハーネスで行い、インストールは deny（owner でない）ハーネスで試す。
    let h = harness(pool.clone(), false);
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");
    let m = manifest("hr-app", TrustTier::InHouse, vec![]);
    let id = h.code.create(&c, &m, None).await.unwrap();
    h.code.publish(&c, id, None, None, None).await.unwrap();

    let deny = harness(pool, true);
    let err = deny
        .installs
        .install(&ctx(&tenant, "mallory"), install_req("hr-app", &[]), None)
        .await;
    assert!(
        matches!(err, Err(app_platform::AppPlatformError::Forbidden)),
        "{err:?}"
    );
}

#[tokio::test]
async fn first_party_requires_trusted_signature() {
    let Some(pool) = setup().await else { return };
    let h = harness(pool.clone(), false);
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");

    // 署名なし publish → first-party はインストール不可。
    let m = manifest("fp-app", TrustTier::FirstParty, vec![]);
    let id = h.code.create(&c, &m, None).await.unwrap();
    h.code.publish(&c, id, None, None, None).await.unwrap();
    let err = h
        .installs
        .install(&c, install_req("fp-app", &[]), None)
        .await;
    assert!(err.is_err(), "{err:?}");

    // 署名付き publish ＋ 信頼鍵登録 → インストール可。
    let secret = [42u8; 32];
    let public = ed25519_public(&secret);
    h.keys.add(&c, "release-key", &public, None).await.unwrap();
    let m2 = {
        let mut m2 = manifest("fp-app2", TrustTier::FirstParty, vec![]);
        m2.version = "1.0.0".into();
        m2
    };
    let sig = sign_manifest(&m2, &secret).unwrap();
    let id2 = h.code.create(&c, &m2, None).await.unwrap();
    h.code
        .publish(&c, id2, None, Some(&sig), None)
        .await
        .unwrap();
    h.installs
        .install(&c, install_req("fp-app2", &[]), None)
        .await
        .expect("signed first-party install");

    // 鍵を失効させると以後のインストールは拒否（fail-closed）。
    h.keys.revoke(&c, "release-key").await.unwrap();
    let err = h
        .installs
        .install(&c, install_req("fp-app2", &[]), None)
        .await;
    assert!(
        matches!(err, Err(app_platform::AppPlatformError::Forbidden)),
        "{err:?}"
    );
}

#[tokio::test]
async fn offline_import_verifies_signature() {
    let Some(pool) = setup().await else { return };
    let h = harness(pool.clone(), false);
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");

    let secret = [5u8; 32];
    let public = ed25519_public(&secret);
    h.keys.add(&c, "airgap", &public, None).await.unwrap();

    let m = manifest("offline-app", TrustTier::FirstParty, vec![]);
    let sig = sign_manifest(&m, &secret).unwrap();

    // 未知の key_id → 拒否。
    assert!(h
        .installs
        .import_signed(&c, m.clone(), &sig, "unknown-key", None)
        .await
        .is_err());
    // 改竄署名 → 拒否（登録されない）。
    let mut bad = sig.clone();
    bad[3] ^= 0x55;
    assert!(h
        .installs
        .import_signed(&c, m.clone(), &bad, "airgap", None)
        .await
        .is_err());
    // 正しい署名 → 登録される。
    let entry = h
        .installs
        .import_signed(&c, m.clone(), &sig, "airgap", None)
        .await
        .expect("import");
    assert_eq!(entry.name, "offline-app");
    // 不変 publish: 同名 version の再 import は 409。
    assert!(h
        .installs
        .import_signed(&c, m, &sig, "airgap", None)
        .await
        .is_err());
}

#[tokio::test]
async fn partial_failure_compensates_created_tables() {
    let Some(pool) = setup().await else { return };
    let h = harness(pool.clone(), false);
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");

    // 2 番目のテーブル名を先取りしておく → プロビジョンが途中で Conflict する。
    let blocker = manifest("blocker", TrustTier::InHouse, vec!["t2"]);
    let bid = h.code.create(&c, &blocker, None).await.unwrap();
    h.code.publish(&c, bid, None, None, None).await.unwrap();
    h.installs
        .install(&c, install_req("blocker", &[]), None)
        .await
        .expect("blocker install");

    let m = manifest("victim", TrustTier::InHouse, vec!["t1", "t2"]);
    let id = h.code.create(&c, &m, None).await.unwrap();
    h.code.publish(&c, id, None, None, None).await.unwrap();
    let err = h
        .installs
        .install(&c, install_req("victim", &[]), None)
        .await;
    assert!(
        matches!(err, Err(app_platform::AppPlatformError::Conflict(_))),
        "{err:?}"
    );

    // 補償: victim 名義の生きたテーブルは残らない・installation 行も無い。
    let (live,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM data_table \
         WHERE tenant_id = $1 AND app_id = $2 AND deleted_at IS NULL",
    )
    .bind(&tenant)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, 0);
    let (rows,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM app_installation WHERE tenant_id = $1 AND app_id = $2",
    )
    .bind(&tenant)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 0);
}

/// ほかの組織の有効なインストールは、この組織から入れ直せず（テーブルとクライアントを作る前に断る）、アンインストール
/// もできない（理由を言って 409・PIT-45・BUG-212）。入れた組織のアプリはそのまま動き続ける。
#[tokio::test]
async fn another_orgs_installation_is_left_alone() {
    let Some(pool) = setup().await else { return };
    let h = harness(pool.clone(), false);
    let tenant = format!("t-{}", Uuid::new_v4());
    let acme = ctx(&tenant, "admin");
    let beta = AuthContext::new(
        acme.principal().clone(),
        "beta".into(),
        authz::TenantId::parse(tenant.clone()).unwrap(),
    );
    let body = async {
        let m = manifest("org-app", TrustTier::InHouse, vec!["records"]);
        let id = h.code.create(&acme, &m, None).await.unwrap();
        h.code.publish(&acme, id, None, None, None).await.unwrap();
        h.installs
            .install(&acme, install_req("org-app", &["data.read"]), None)
            .await
            .expect("acme のインストール");

        let other_org = |r: Result<(), app_platform::AppPlatformError>| matches!(r, Err(app_platform::AppPlatformError::Conflict(m)) if m.contains("ほかの組織でインストールされています"));
        let refused = h
            .installs
            .install(
                &beta,
                install_req("org-app", &["data.read", "data.write"]),
                None,
            )
            .await
            .map(|_| ());
        assert!(other_org(refused), "ほかの組織からの入れ直しは断る");
        let refused = h.installs.uninstall(&beta, id, None).await;
        assert!(other_org(refused), "ほかの組織からのアンインストールも断る");

        // テーブルは acme の 1 つだけ（beta の分を作っていない）。インストールは acme のまま有効で、同意も変わらない。
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT org FROM data_table WHERE tenant_id = $1 AND app_id = $2 AND deleted_at IS NULL",
        )
        .bind(&tenant)
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(tables, ["acme"]);
        let (org, status, scopes): (String, String, Vec<String>) = sqlx::query_as(
            "SELECT org, status, granted_scopes FROM app_installation WHERE tenant_id = $1 AND app_id = $2",
        )
        .bind(&tenant)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (org.as_str(), status.as_str(), scopes),
            ("acme", "active", vec!["data.read".to_string()])
        );
    };
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(body)).await;
    common::purge_tenant_rows(&pool, &tenant).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn ed25519_public(secret: &[u8; 32]) -> Vec<u8> {
    use ed25519_dalek::SigningKey;
    SigningKey::from_bytes(secret)
        .verifying_key()
        .to_bytes()
        .to_vec()
}

/// web シェルのホスト支援 PKCE の戻り先（api の `AppConfig::miniapp_b1_redirect_uris` が dev で求めるもの）。
const SHELL_CALLBACK: &str = "http://localhost:3000/apps/oauth/callback";

/// B1 のアプリ（テーブル無し）を作って publish し、artifact の id を返す。
async fn publish_b1(h: &Harness, c: &AuthContext, name: &str) -> Uuid {
    let m = manifest(name, TrustTier::InHouse, vec![]);
    let id = h.code.create(c, &m, None).await.expect("create");
    h.code
        .publish(c, id, None, None, None)
        .await
        .expect("publish");
    id
}

fn sorted_names(list: &serde_json::Value) -> Vec<String> {
    let mut v: Vec<String> = list
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.as_str()
                        .or_else(|| x["name"].as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

async fn spec_of(pool: &PgPool, tenant: &str, app_id: Uuid) -> Option<String> {
    sqlx::query_scalar(
        "SELECT oauth_client_spec FROM app_installation WHERE tenant_id = $1 AND app_id = $2",
    )
    .bind(tenant)
    .bind(app_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Keycloak を使うテストを 1 本ずつ流す。起動時の照合（[`InstallService::repair_oauth_clients`]）は DB の有効な
/// インストールをすべて揃えるので、並べて流すと、別のテストがクライアントを作り・消している途中に重なる。
static KEYCLOAK_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// テストの本体を走らせ、落ちても片付け（`cleanup`）をしてから落とす（Keycloak を使うテストは 1 本ずつ）。
async fn run_then<F, C>(body: F, cleanup: C)
where
    F: std::future::Future<Output = ()>,
    C: std::future::Future<Output = ()>,
{
    use futures::FutureExt as _;
    let _serial = KEYCLOAK_SERIAL.lock().await;
    let outcome = std::panic::AssertUnwindSafe(body).catch_unwind().await;
    cleanup.await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// インストールは B1 のクライアントを、シェルの戻り先・sub を載せる既定のスコープ・tenant のマッパーで登録する
/// （所属グループのマッパーは付けない・D-130）。アンインストールで無効になり、再インストールで有効に戻る（BUG-172 の ③）。
#[tokio::test]
async fn install_registers_b1_client_and_reinstall_enables_it() {
    let Some(pool) = setup().await else { return };
    let Some(realm) = app_gateway::test_support::TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let h = harness_with_oauth(
        pool.clone(),
        false,
        Some(realm.oauth_client()),
        vec![SHELL_CALLBACK.into()],
    );
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");
    let name = format!("b1-{}", Uuid::new_v4().simple());
    let id = publish_b1(&h, &c, &name).await;
    let client_id = format!("app-{id}-b1");
    let body = async {
        let installed = h
            .installs
            .install(&c, install_req(&name, &["llm.invoke"]), None)
            .await
            .expect("install");
        assert_eq!(
            installed.installation.client_id_b1.as_deref(),
            Some(client_id.as_str())
        );
        let client = realm.client(&client_id).await.expect("client");
        assert_eq!(client["enabled"], true);
        assert_eq!(client["redirectUris"], serde_json::json!([SHELL_CALLBACK]));
        assert_eq!(sorted_names(&client["defaultClientScopes"]), ["basic"]);
        assert_eq!(
            sorted_names(&client["protocolMappers"]),
            ["shiki-gateway-audience", "tenant"]
        );
        // インストールは、今の形で揃えた印を付ける（起動時の照合が揃え直さない）。
        assert_eq!(
            spec_of(&pool, &tenant, id).await.as_deref(),
            Some(
                app_gateway::client_spec_fingerprint(
                    &[SHELL_CALLBACK.into()],
                    app_gateway::DEFAULT_GATEWAY_AUDIENCE
                )
                .as_str()
            )
        );
        realm
            .user_token(&client_id, SHELL_CALLBACK, "alice", "openid llm.invoke")
            .await
            .expect("token");

        // アンインストールは無効にする（トークンが出ない）。
        h.installs.uninstall(&c, id, None).await.expect("uninstall");
        assert_eq!(realm.client(&client_id).await.unwrap()["enabled"], false);
        assert!(realm
            .user_token(&client_id, SHELL_CALLBACK, "alice", "openid")
            .await
            .is_err());

        // 再インストールで有効に戻り、トークンが出る（これまでは 409 を冪等として扱い、無効のまま残った）。
        h.installs
            .install(&c, install_req(&name, &["llm.invoke"]), None)
            .await
            .expect("reinstall");
        assert_eq!(realm.client(&client_id).await.unwrap()["enabled"], true);
        realm
            .user_token(&client_id, SHELL_CALLBACK, "alice", "openid llm.invoke")
            .await
            .expect("token after reinstall");
    };
    run_then(body, async {
        let _ = h.installs.uninstall(&c, id, None).await;
        realm.delete_client(&client_id).await;
        sqlx::query("DELETE FROM app_installation WHERE tenant_id = $1")
            .bind(&tenant)
            .execute(&pool)
            .await
            .expect("cleanup installation");
    })
    .await;
}

/// 起動時の照合は、前の版の形のクライアントを今の形に揃え、印を付ける。印が今の形なら触らない。アンインストール
/// したアプリのクライアントは無効のまま（BUG-172）。
#[tokio::test]
async fn startup_repair_fixes_previous_clients_once() {
    let Some(pool) = setup().await else { return };
    let Some(realm) = app_gateway::test_support::TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let redirects = vec![SHELL_CALLBACK.to_string()];
    let h = harness_with_oauth(
        pool.clone(),
        false,
        Some(realm.oauth_client()),
        redirects.clone(),
    );
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");
    let active_name = format!("b1-{}", Uuid::new_v4().simple());
    let revoked_name = format!("b1-{}", Uuid::new_v4().simple());
    let active = publish_b1(&h, &c, &active_name).await;
    let revoked = publish_b1(&h, &c, &revoked_name).await;
    let (active_client, revoked_client) = (format!("app-{active}-b1"), format!("app-{revoked}-b1"));
    let previous = |redirect: &str| {
        serde_json::json!({
            "clientId": "",
            "redirectUris": [redirect],
            // 7d の版が付けた所属グループのマッパー（D-130 の前）も模す。照合で外れる。
            "protocolMappers": [{
                "name": "shiki-gateway-audience",
                "protocol": "openid-connect",
                "protocolMapper": "oidc-audience-mapper",
                "config": { "included.custom.audience": "shiki-gateway", "access.token.claim": "true" },
            }, {
                "name": "groups",
                "protocol": "openid-connect",
                "protocolMapper": "oidc-group-membership-mapper",
                "config": { "full.path": "true", "claim.name": "groups", "access.token.claim": "true" },
            }],
        })
    };
    let body = async {
        for name in [&active_name, &revoked_name] {
            h.installs
                .install(&c, install_req(name, &["llm.invoke"]), None)
                .await
                .expect("install");
        }
        h.installs
            .uninstall(&c, revoked, None)
            .await
            .expect("uninstall");
        // 前の版でインストールしたことにする（BFF の戻り先・既定のスコープ無し・aud のマッパーだけ・印が無い）。
        for client in [&active_client, &revoked_client] {
            let mut rep = previous("http://localhost:3000/auth/callback");
            rep["clientId"] = serde_json::json!(client);
            realm.update_client_raw(client, &rep).await;
            realm.remove_default_scopes(client).await;
            assert_eq!(
                sorted_names(&realm.client(client).await.unwrap()["protocolMappers"]),
                ["groups", "shiki-gateway-audience"]
            );
        }
        sqlx::query("UPDATE app_installation SET oauth_client_spec = NULL WHERE tenant_id = $1")
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
        assert!(realm
            .user_token(&active_client, SHELL_CALLBACK, "alice", "openid")
            .await
            .is_err());

        let progress = h.installs.repair_oauth_clients().await.expect("repair");
        assert!(progress.repaired >= 1, "{progress:?}");
        assert_eq!(progress.failed, 0, "{progress:?}");
        let fixed = realm.client(&active_client).await.unwrap();
        assert_eq!(fixed["enabled"], true);
        assert_eq!(fixed["redirectUris"], serde_json::json!([SHELL_CALLBACK]));
        assert_eq!(sorted_names(&fixed["defaultClientScopes"]), ["basic"]);
        assert_eq!(
            sorted_names(&fixed["protocolMappers"]),
            ["shiki-gateway-audience", "tenant"]
        );
        let token = realm
            .user_token(&active_client, SHELL_CALLBACK, "alice", "openid llm.invoke")
            .await
            .expect("token after repair");
        let claims = app_gateway::test_support::claims(&token);
        assert!(
            claims["sub"].is_string() && claims["tenant"] == "a-corp",
            "{claims}"
        );
        assert!(
            claims.get("groups").is_none(),
            "所属グループは載らない: {claims}"
        );
        let spec =
            app_gateway::client_spec_fingerprint(&redirects, app_gateway::DEFAULT_GATEWAY_AUDIENCE);
        assert_eq!(
            spec_of(&pool, &tenant, active).await.as_deref(),
            Some(spec.as_str())
        );
        // アンインストールしたアプリは揃えない（無効のまま・印も付けない）。
        let still = realm.client(&revoked_client).await.unwrap();
        assert_eq!(still["enabled"], false);
        assert_eq!(
            still["redirectUris"],
            serde_json::json!(["http://localhost:3000/auth/callback"])
        );
        assert_eq!(spec_of(&pool, &tenant, revoked).await, None);

        // 印が今の形なら、次の起動では触らない（Keycloak を呼ばない）。
        let mut rep = previous("http://localhost:3000/auth/callback");
        rep["clientId"] = serde_json::json!(&active_client);
        realm.update_client_raw(&active_client, &rep).await;
        h.installs
            .repair_oauth_clients()
            .await
            .expect("repair again");
        assert_eq!(
            realm.client(&active_client).await.unwrap()["redirectUris"],
            serde_json::json!(["http://localhost:3000/auth/callback"])
        );
    };
    run_then(body, async {
        let _ = h.installs.uninstall(&c, active, None).await;
        realm.delete_client(&active_client).await;
        realm.delete_client(&revoked_client).await;
        sqlx::query("DELETE FROM app_installation WHERE tenant_id = $1")
            .bind(&tenant)
            .execute(&pool)
            .await
            .expect("cleanup installation");
    })
    .await;
}

/// サーバーの関数を持つアプリ（B2）のインストールは、B1 の aud に同じアプリの B2 を足し（利用者が起点の交換・
/// BUG-182）、B2 のサービスアカウントの利用者（自動実行のトークンの sub）をインストールに書く（BUG-183）。前の版の
/// インストール（印もサービスアカウントも無い）は、起動時の照合が書き足す。
#[tokio::test]
async fn install_with_server_records_the_service_account_and_exchange_audience() {
    let Some(pool) = setup().await else { return };
    let Some(realm) = app_gateway::test_support::TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let redirects = vec![SHELL_CALLBACK.to_string()];
    let h = harness_with_oauth(
        pool.clone(),
        false,
        Some(realm.oauth_client()),
        redirects.clone(),
    );
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");
    let name = format!("b2-{}", Uuid::new_v4().simple());
    let mut m = manifest(&name, TrustTier::InHouse, vec![]);
    m.requested_scopes = vec!["data.read".into()];
    m.server = Some(app_platform::ServerSpec {
        functions: vec!["remind".into()],
        ..app_platform::ServerSpec::default()
    });
    let id = h.code.create(&c, &m, None).await.expect("create");
    h.code
        .publish(&c, id, None, None, None)
        .await
        .expect("publish");
    let (b1, b2) = (format!("app-{id}-b1"), format!("app-{id}-b2"));
    let service_sub = |pool: PgPool, tenant: String| async move {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT b2_service_sub FROM app_installation WHERE tenant_id = $1 AND app_id = $2",
        )
        .bind(tenant)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let body = async {
        let installed = h
            .installs
            .install(&c, install_req(&name, &["data.read"]), None)
            .await
            .expect("install");
        assert_eq!(
            installed.installation.client_id_b2.as_deref(),
            Some(b2.as_str())
        );
        let client = realm.client(&b1).await.expect("b1");
        let exchange_aud = client["protocolMappers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "shiki-function-audience")
            .expect("交換の相手の aud");
        assert_eq!(
            exchange_aud["config"]["included.custom.audience"],
            b2.as_str()
        );

        // 書いたサービスアカウントの利用者は、B2 の client_credentials のトークンの sub と同じ。
        let secret = installed.client_secret_b2.expect("secret");
        let resp = reqwest::Client::new()
            .post(realm.token_endpoint())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", b2.as_str()),
                ("client_secret", secret.as_str()),
            ])
            .send()
            .await
            .expect("service token");
        let token: serde_json::Value = resp.json().await.unwrap();
        let sub = app_gateway::test_support::claims(token["access_token"].as_str().unwrap())["sub"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            service_sub(pool.clone(), tenant.clone()).await,
            Some(sub.clone())
        );

        // 前の版のインストール（印・サービスアカウント・交換の相手の aud が無い）を、起動時の照合が直す。
        sqlx::query(
            "UPDATE app_installation SET oauth_client_spec = NULL, b2_service_sub = NULL \
             WHERE tenant_id = $1",
        )
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
        let mut previous = client.clone();
        if let Some(mappers) = previous["protocolMappers"].as_array_mut() {
            mappers.retain(|m| m["name"] != "shiki-function-audience");
        }
        realm.update_client_raw(&b1, &previous).await;
        assert!(realm.client(&b1).await.unwrap()["protocolMappers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] != "shiki-function-audience"));
        let progress = h.installs.repair_oauth_clients().await.expect("repair");
        assert_eq!(progress.failed, 0, "{progress:?}");
        assert_eq!(service_sub(pool.clone(), tenant.clone()).await, Some(sub));
        assert!(realm.client(&b1).await.unwrap()["protocolMappers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "shiki-function-audience"));
    };
    run_then(body, async {
        let _ = h.installs.uninstall(&c, id, None).await;
        realm.delete_client(&b1).await;
        realm.delete_client(&b2).await;
        sqlx::query("DELETE FROM app_installation WHERE tenant_id = $1")
            .bind(&tenant)
            .execute(&pool)
            .await
            .expect("cleanup installation");
    })
    .await;
}

/// 起動時の照合は、Keycloak に無くなった B1 を作り直す。B2（secret を持つ）は作り直さず、入れ直しが要る印を付ける
/// （管理者が漏れた secret を無効にするために消したのなら、同じ secret で戻さない）。印は起動のたびに試し直しても
/// 外れず、入れ直すと新しい secret の B2 ができて外れる（GAP-70）。
#[tokio::test]
async fn startup_repair_recreates_a_missing_b1_but_not_a_missing_b2() {
    let Some(pool) = setup().await else { return };
    let Some(realm) = app_gateway::test_support::TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let redirects = vec![SHELL_CALLBACK.to_string()];
    let Harness { code, installs, .. } = harness_with_oauth(
        pool.clone(),
        false,
        Some(realm.oauth_client()),
        redirects.clone(),
    );
    let authz: Arc<dyn AuthzClient> = Arc::new(RecordingAuthz::default());
    let secrets = Arc::new(secrets::SecretStore::new(
        pool.clone(),
        authz,
        Arc::new(secrets::LocalKeyFileProvider::from_bytes([9u8; 32])),
    ));
    let installs = installs.with_secrets(Some(secrets), Some("localhost".into()));
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");
    let name = format!("b2-{}", Uuid::new_v4().simple());
    let mut m = manifest(&name, TrustTier::InHouse, vec![]);
    m.requested_scopes = vec!["data.read".into()];
    m.server = Some(app_platform::ServerSpec {
        functions: vec!["remind".into()],
        ..app_platform::ServerSpec::default()
    });
    let id = code.create(&c, &m, None).await.expect("create");
    code.publish(&c, id, None, None, None)
        .await
        .expect("publish");
    let (b1, b2) = (format!("app-{id}-b1"), format!("app-{id}-b2"));
    let row = || client_row(&pool, id);
    let service_sub = |secret: String| service_token_sub(&realm, &b2, secret);
    let body = async {
        let installed = installs
            .install(&c, install_req(&name, &["data.read"]), None)
            .await
            .expect("install");
        let leaked = installed.client_secret_b2.expect("secret");
        let before = row().await;
        assert_eq!(before.0, Some(service_sub(leaked.clone()).await));

        // 2 つとも Keycloak から消える（管理者が漏れた secret を無効にした・realm を作り直したなど）。照合は登録の形が
        // 変わった後の起動で走るので、前の版のインストールにする（印を消す）。
        realm.delete_client(&b1).await;
        realm.delete_client(&b2).await;
        sqlx::query("UPDATE app_installation SET oauth_client_spec = NULL WHERE tenant_id = $1")
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();

        // B1 だけを作り直し、B2 は作らずに入れ直しが要る印を付ける（自動実行の主体も外す）。
        let progress = installs.repair_oauth_clients().await.expect("repair");
        assert_eq!(progress.failed, 0, "{progress:?}");
        assert!(progress.missing >= 1, "{progress:?}");
        assert!(realm.client(&b1).await.is_some(), "B1 は作り直す");
        let client = realm.client(&b1).await.expect("b1");
        assert!(client["protocolMappers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "shiki-function-audience"));
        assert!(realm.client(&b2).await.is_none(), "B2 は作り直さない");
        assert_eq!(
            row().await,
            (None, true),
            "入れ直しが要る印・自動実行の主体を外す"
        );
        let listed = installs.installations().list(&c).await.expect("list");
        assert!(listed[0].oauth_client_missing);

        // 印のある行を試し直しても、B2 は作り直さない。
        installs.repair_oauth_clients().await.expect("repair again");
        assert!(
            realm.client(&b2).await.is_none(),
            "試し直しても作り直さない"
        );
        assert!(row().await.1);

        // 入れ直すと新しい secret の B2 ができ、印が外れる。漏れた secret ではトークンを取れない。
        let reinstalled = installs
            .install(&c, install_req(&name, &["data.read"]), None)
            .await
            .expect("reinstall");
        let fresh = reinstalled.client_secret_b2.expect("new secret");
        assert_ne!(fresh, leaked, "入れ直すと新しい secret になる");
        let after = row().await;
        assert!(!after.1, "印を外す");
        assert_eq!(after.0, Some(service_sub(fresh).await));
        assert_ne!(
            after.0, before.0,
            "サービスアカウントの利用者は作り直すと変わる"
        );
        assert!(
            !token_issued(&realm, &b2, &leaked).await,
            "漏れた secret ではトークンを取れない"
        );
    };
    run_then(body, async {
        let _ = installs.uninstall(&c, id, None).await;
        realm.delete_client(&b1).await;
        realm.delete_client(&b2).await;
        for sql in [
            "DELETE FROM app_installation WHERE tenant_id = $1",
            "DELETE FROM secret WHERE tenant_id = $1",
        ] {
            sqlx::query(sql)
                .bind(&tenant)
                .execute(&pool)
                .await
                .expect(sql);
        }
    })
    .await;
}

/// 起動時の照合は、Keycloak で無効にされた B2 を有効に戻さず、印を付けて自動実行の主体を外す（管理者が漏れた
/// secret を止めるために無効にしたのなら、同じ secret のまま戻さない・BUG-201・D-137）。無効にされた B1 は
/// これまでどおり有効に戻す。印は起動のたびに試し直しても外れず、管理者が Keycloak で有効に戻すと外れる。
/// 入れ直し（利用者の明示の操作）は、無効の B2 の secret を作り直してから有効に戻す。
#[tokio::test]
async fn startup_repair_keeps_a_disabled_b2_disabled() {
    let Some(pool) = setup().await else { return };
    let Some(realm) = app_gateway::test_support::TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let Harness { code, installs, .. } = harness_with_oauth(
        pool.clone(),
        false,
        Some(realm.oauth_client()),
        vec![SHELL_CALLBACK.to_string()],
    );
    let authz: Arc<dyn AuthzClient> = Arc::new(RecordingAuthz::default());
    let secrets = Arc::new(secrets::SecretStore::new(
        pool.clone(),
        authz,
        Arc::new(secrets::LocalKeyFileProvider::from_bytes([9u8; 32])),
    ));
    let installs = installs.with_secrets(Some(secrets), Some("localhost".into()));
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant, "admin");
    let name = format!("b2-{}", Uuid::new_v4().simple());
    let mut m = manifest(&name, TrustTier::InHouse, vec![]);
    m.requested_scopes = vec!["data.read".into()];
    m.server = Some(app_platform::ServerSpec {
        functions: vec!["remind".into()],
        ..app_platform::ServerSpec::default()
    });
    let id = code.create(&c, &m, None).await.expect("create");
    code.publish(&c, id, None, None, None)
        .await
        .expect("publish");
    let (b1, b2) = (format!("app-{id}-b1"), format!("app-{id}-b2"));
    let oauth = realm.oauth_client();
    let disabled_mark = || async {
        sqlx::query_scalar::<_, bool>(
            "SELECT oauth_client_disabled FROM app_installation WHERE app_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let forget_spec = || async {
        sqlx::query("UPDATE app_installation SET oauth_client_spec = NULL WHERE tenant_id = $1")
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
    };
    let body = async {
        let installed = installs
            .install(&c, install_req(&name, &["data.read"]), None)
            .await
            .expect("install");
        let secret = installed.client_secret_b2.expect("secret");
        let service_sub = client_row(&pool, id).await.0;
        assert!(service_sub.is_some());

        // 管理者が B2（と B1）を Keycloak で無効にする。照合は登録の形が変わった後の起動で走るので、印を消す。
        oauth.set_enabled(&b2, false).await.expect("disable b2");
        oauth.set_enabled(&b1, false).await.expect("disable b1");
        assert!(!token_issued(&realm, &b2, &secret).await);
        forget_spec().await;

        let progress = installs.repair_oauth_clients().await.expect("repair");
        assert_eq!(progress.failed, 0, "{progress:?}");
        assert!(progress.disabled >= 1, "{progress:?}");
        assert_eq!(
            realm.client(&b2).await.expect("b2")["enabled"],
            false,
            "B2 は有効に戻さない"
        );
        assert!(
            !token_issued(&realm, &b2, &secret).await,
            "同じ secret でアプリの主体のトークンを取れない"
        );
        assert_eq!(
            realm.client(&b1).await.expect("b1")["enabled"],
            true,
            "B1 はこれまでどおり有効に戻す"
        );
        assert!(disabled_mark().await, "印を付ける");
        assert_eq!(
            client_row(&pool, id).await,
            (None, false),
            "自動実行の主体を外す"
        );
        let listed = installs.installations().list(&c).await.expect("list");
        assert!(listed[0].oauth_client_disabled);
        assert!(!listed[0].oauth_client_missing);

        // 印のある行を試し直しても、有効に戻さない。
        installs.repair_oauth_clients().await.expect("repair again");
        assert_eq!(realm.client(&b2).await.expect("b2")["enabled"], false);
        assert!(disabled_mark().await);

        // 管理者が Keycloak で有効に戻すと、次の照合で揃えて印を外し、自動実行の主体を書き戻す。
        oauth.set_enabled(&b2, true).await.expect("enable b2");
        installs
            .repair_oauth_clients()
            .await
            .expect("repair after enable");
        assert!(!disabled_mark().await, "印を外す");
        assert_eq!(client_row(&pool, id).await.0, service_sub);

        // 入れ直し（利用者の明示の操作）は、無効の B2 の secret を作り直してから有効に戻し、印を外す。持ち主の入れ直しや
        // 版の更新で、管理者が止めた secret のまま戻さない（BUG-201）。
        oauth
            .set_enabled(&b2, false)
            .await
            .expect("disable b2 again");
        forget_spec().await;
        installs.repair_oauth_clients().await.expect("repair");
        assert!(disabled_mark().await);
        reinstall_rotates_a_disabled_b2(&installs, &c, &name, &realm, &b2, &secret).await;
        assert!(!disabled_mark().await);
    };
    run_then(body, async {
        let _ = installs.uninstall(&c, id, None).await;
        realm.delete_client(&b1).await;
        realm.delete_client(&b2).await;
        for sql in [
            "DELETE FROM app_installation WHERE tenant_id = $1",
            "DELETE FROM secret WHERE tenant_id = $1",
        ] {
            sqlx::query(sql)
                .bind(&tenant)
                .execute(&pool)
                .await
                .expect(sql);
        }
    })
    .await;
}

/// 無効の B2 を入れ直すと、secret を作り直してから有効に戻す: 止めた secret ではアプリの主体のトークンを取れず、
/// 新しい secret では取れる。有効な B2 の入れ直し（版の更新）は secret を変えない。
async fn reinstall_rotates_a_disabled_b2(
    installs: &InstallService,
    c: &AuthContext,
    name: &str,
    realm: &app_gateway::test_support::TestRealm,
    b2: &str,
    stopped: &str,
) {
    let fresh = installs
        .install(c, install_req(name, &["data.read"]), None)
        .await
        .expect("reinstall")
        .client_secret_b2
        .expect("new secret");
    assert_eq!(realm.client(b2).await.expect("b2")["enabled"], true);
    assert_ne!(fresh, stopped, "無効だった B2 の secret は作り直す");
    assert!(
        !token_issued(realm, b2, stopped).await,
        "止めた secret ではアプリの主体のトークンを取れない"
    );
    assert!(token_issued(realm, b2, &fresh).await);
    let again = installs
        .install(c, install_req(name, &["data.read"]), None)
        .await
        .expect("reinstall an enabled b2")
        .client_secret_b2
        .expect("secret");
    assert_eq!(again, fresh, "有効な B2 の入れ直しは secret を変えない");
}

/// B2 の client_credentials で、この secret のトークンが取れるか。
async fn token_issued(
    realm: &app_gateway::test_support::TestRealm,
    b2: &str,
    secret: &str,
) -> bool {
    reqwest::Client::new()
        .post(realm.token_endpoint())
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", b2),
            ("client_secret", secret),
        ])
        .send()
        .await
        .expect("token endpoint")
        .status()
        .is_success()
}

/// インストールの B2 のサービスアカウントの利用者と、入れ直しが要る印。
async fn client_row(pool: &PgPool, app_id: Uuid) -> (Option<String>, bool) {
    sqlx::query_as(
        "SELECT b2_service_sub, oauth_client_missing FROM app_installation WHERE app_id = $1",
    )
    .bind(app_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// B2 の client_credentials のトークンの sub（サービスアカウントの利用者）。
async fn service_token_sub(
    realm: &app_gateway::test_support::TestRealm,
    b2: &str,
    secret: String,
) -> String {
    let resp = reqwest::Client::new()
        .post(realm.token_endpoint())
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", b2),
            ("client_secret", secret.as_str()),
        ])
        .send()
        .await
        .expect("service token");
    assert!(resp.status().is_success(), "{}", resp.status());
    let token: serde_json::Value = resp.json().await.unwrap();
    app_gateway::test_support::claims(token["access_token"].as_str().unwrap())["sub"]
        .as_str()
        .unwrap()
        .to_string()
}
