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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    let url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
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
        tenant.into(),
    )
}

fn table_schema() -> TableSchema {
    let mut title = FieldDef {
        name: "title".into(),
        field_type: FieldType::Text,
        required: true,
        unique: false,
        indexed: false,
        options: vec![],
        ref_table: None,
        lookup: None,
        computed: None,
    };
    title.required = true;
    TableSchema {
        fields: vec![title],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
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
        None,
        vec![],
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

    // FGA tuple: owner@miniapp ＋ viewer@role#member。
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

fn ed25519_public(secret: &[u8; 32]) -> Vec<u8> {
    use ed25519_dalek::SigningKey;
    SigningKey::from_bytes(secret)
        .verifying_key()
        .to_bytes()
        .to_vec()
}
