//! 実 Keycloak・実 OpenFGA・実 Postgres で、アプリのゲートウェイが組織をインストールから決め、その組織の member
//! だけを通すことを確かめる（D-130）。
//!
//! `KEYCLOAK_TEST_URL`（realm `shiki` を import 済み）・`STORAGE_TEST_DATABASE_URL`・`OPENFGA_TEST_URL` のどれかが
//! 無ければスキップする。テナントは realm の試しの利用者に合わせて `a-corp`（multi）。作ったクライアント・行・
//! OpenFGA のストアは、テストの最後に消す。
//!
//! 1. 同意インストールで登録した B1 のクライアントのトークンには、所属グループ（`groups`）が載らない。
//! 2. member のタプルが無い人（ログインしていない人）はゲートウェイで 403。
//! 3. ログインの同期（[`super::org_member::sync_org_member`]）を Keycloak の所属グループで通すと 200 になる。
//! 4. 同じテナントで同期していない人（bob）は 403。別のテナントの人（charlie）は、自分のテナントで同期しても 403。
//! 5. 所属グループから外れた（グループが無い）人を同期し直すと、member が外れて 403 に戻る。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::large_futures,
    clippy::too_many_lines
)]

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use app_gateway::test_support::{claims, TestRealm};
use app_platform::{
    Budget, InstallRequest, InstallService, MiniAppCodeStore, MiniAppManifest, Registry, TrustTier,
};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, GroupLayout, Principal, PrincipalKind};
use data::RefResolver;
use futures::FutureExt as _;
use storage::{ObjectStore, ObjectStoreError};
use uuid::Uuid;

use super::org_member::sync_org_member;

/// web シェルのホスト支援 PKCE の戻り先（web/src/lib/miniapp-oauth.ts と同じ）。
const SHELL_CALLBACK: &str = "http://localhost:3000/apps/oauth/callback";
/// realm の試しの利用者（deploy/keycloak/shiki-realm.json）の sub。
const ALICE: &str = "00000000-0000-0000-0000-000000000001";
const CHARLIE: &str = "00000000-0000-0000-0000-000000000003";
/// alice・bob のテナントと組織。
const TENANT: &str = "a-corp";

/// 使わない置き場（whoami はファイルを読まない）。
struct NoObjects;

#[async_trait::async_trait]
impl ObjectStore for NoObjects {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(&self, _: &str, _: Duration, _: i64) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get_internal(&self, _: &str, _: Duration) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn read_and_hash(&self, k: &str) -> Result<(String, u64), ObjectStoreError> {
        Err(ObjectStoreError::NotFound(k.into()))
    }
    async fn put_object(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn get_object(&self, k: &str) -> Result<Vec<u8>, ObjectStoreError> {
        Err(ObjectStoreError::NotFound(k.into()))
    }
    async fn exists(&self, _: &str) -> Result<bool, ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

/// 参照の確かめ（このテストのアプリはテーブルを持たない）。
struct NoRefs;

#[async_trait::async_trait]
impl RefResolver for NoRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

/// Keycloak の所属グループを claims に持つ利用者（ログインのクライアントのトークンの groups と同じ値）。
async fn principal(realm: &TestRealm, username: &str, sub: &str, tenant: &str) -> Principal {
    Principal {
        kind: PrincipalKind::User,
        id: sub.into(),
        email: None,
        groups: realm.user_group_paths(username).await,
        roles: vec![],
        tenant_id: Some(tenant.into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_admits_only_members_of_the_installing_org() {
    let (Some(realm), Ok(db_url), Ok(fga_url)) = (
        TestRealm::from_env(),
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!(
            "KEYCLOAK_TEST_URL・STORAGE_TEST_DATABASE_URL・OPENFGA_TEST_URL のどれかが無いためスキップ"
        );
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
            store_name: format!("org-member-it-{}", Uuid::new_v4()),
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
    let data = Arc::new(data::DataStore::new(
        pool.clone(),
        authz.clone(),
        Arc::new(NoRefs),
    ));
    let installs = InstallService::new(
        pool.clone(),
        Registry::new(pool.clone()),
        Arc::clone(&code),
        Arc::clone(&data),
        authz.clone(),
        Some(realm.oauth_client()),
        vec![SHELL_CALLBACK.into()],
    );
    let installer = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: format!("installer-{}", Uuid::new_v4().simple()),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(TENANT.into()),
        },
        TENANT.into(),
        TENANT.into(),
    );
    let name = format!("org-it-{}", Uuid::new_v4().simple());
    let manifest = MiniAppManifest {
        name: name.clone(),
        version: "1.0.0".into(),
        description: "組織の試し".into(),
        requested_scopes: vec!["data.read".into()],
        tools: vec![],
        tables: vec![],
        workflows: vec![],
        budget: Budget::default(),
        frontend: None,
        server: None,
        trust_tier: TrustTier::InHouse,
    };
    let app_id = code
        .create(&installer, &manifest, None)
        .await
        .expect("create");
    let b1 = format!("app-{app_id}-b1");

    let body = async {
        code.publish(&installer, app_id, None, None, None)
            .await
            .expect("publish");
        installs
            .install(
                &installer,
                InstallRequest {
                    name: name.clone(),
                    version: "1.0.0".into(),
                    granted_scopes: vec!["data.read".into()],
                    viewer_roles: vec![],
                    editor_roles: vec![],
                    submitter_roles: vec![],
                },
                None,
            )
            .await
            .expect("install");

        // ゲートウェイ（multi テナント・本物の鍵・本物の OpenFGA）。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = app_gateway::build_gateway_router(app_gateway::GatewayState {
            installations: app_gateway::AppInstallationStore::new(pool.clone()),
            keys: Arc::new(realm.key_resolver()),
            token_cfg: app_gateway::GatewayTokenConfig {
                audience: "shiki-gateway".into(),
                issuer: realm.issuer(),
            },
            authz: authz.clone(),
            audit: storage::audit::AuditRecorder::new(pool.clone()),
            tenant_source: authz::TenantSource::Claim,
            caps: app_gateway::CapabilityDeps {
                db: pool.clone(),
                storage: Arc::new(storage::StorageService::new(
                    pool.clone(),
                    Arc::new(NoObjects),
                    authz.clone(),
                    Duration::from_mins(1),
                    Duration::from_mins(1),
                    1024 * 1024,
                )),
                data: Arc::clone(&data),
                fsms: Arc::new(data::FsmStore::new(Arc::clone(&artifacts), (*data).clone())),
                rag: Arc::new(app_gateway::NoRag),
                notifications: app_gateway::NotificationStore::new(pool.clone()),
                llm: None,
                agent: Arc::new(app_gateway::NoAgent),
                ai_daily_cap_usd_micros: 0,
                functions: Arc::new(app_gateway::NoFunctions),
            },
        });
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let http = reqwest::Client::new();
        let whoami = |token: String| {
            let http = http.clone();
            let url = format!("{origin}/gw/whoami");
            async move {
                let resp = http.get(url).bearer_auth(token).send().await.unwrap();
                let status = resp.status().as_u16();
                let body: serde_json::Value = resp.json().await.unwrap_or_default();
                (status, body)
            }
        };
        let token_of = |username: &'static str| {
            let realm = &realm;
            let b1 = b1.clone();
            async move {
                realm
                    .user_token(&b1, SHELL_CALLBACK, username, "openid data.read")
                    .await
                    .expect("token")
            }
        };

        // 1. アプリのトークンに所属グループは載らない（テナントと sub は載る）。
        let alice = token_of("alice").await;
        let raw = claims(&alice);
        assert!(raw.get("groups").is_none(), "{raw}");
        assert_eq!(raw["tenant"], TENANT);
        assert_eq!(raw["sub"], ALICE);

        // 2. member のタプルが無ければ、同じ組織の人でも 403。
        let (s, body) = whoami(alice.clone()).await;
        assert_eq!(s, 403, "{body}");
        assert_eq!(
            body["error"],
            "権限がありません: このアプリを使えるのは、アプリをインストールした組織のメンバーだけです"
        );

        // 3. ログインの同期（Keycloak の所属グループ）で member が付き、通る。
        let synced = sync_org_member(
            &*authz,
            &principal(&realm, "alice", ALICE, TENANT).await,
            TENANT,
            GroupLayout::TenantScoped,
        )
        .await;
        assert_eq!(synced.desired.as_deref(), Some(TENANT));
        assert!(synced.granted);
        let (s, body) = whoami(alice.clone()).await;
        assert_eq!(s, 200, "{body}");
        assert_eq!(body["user_sub"], ALICE);
        assert_eq!(body["app_id"], app_id.to_string());

        // 4. 同期していない bob は 403。別のテナントの charlie は、自分のテナントで同期しても 403
        //    （そのテナントにこのアプリのインストールが無い）。
        let (s, _) = whoami(token_of("bob").await).await;
        assert_eq!(s, 403, "bob（member でない）");
        let charlie = principal(&realm, "charlie", CHARLIE, "b-corp").await;
        assert!(
            sync_org_member(&*authz, &charlie, "b-corp", GroupLayout::TenantScoped)
                .await
                .granted
        );
        let charlie_token = token_of("charlie").await;
        assert_eq!(claims(&charlie_token)["tenant"], "b-corp");
        let (s, body) = whoami(charlie_token).await;
        assert_eq!(s, 403, "charlie（別のテナント）: {body}");

        // 5. 所属グループから外れた alice を同期し直すと、member が外れて 403 に戻る。
        let mut left = principal(&realm, "alice", ALICE, TENANT).await;
        left.groups.clear();
        let revoked = sync_org_member(&*authz, &left, TENANT, GroupLayout::TenantScoped).await;
        assert_eq!(revoked.revoked, [TENANT]);
        let (s, _) = whoami(alice).await;
        assert_eq!(s, 403, "組織を離れた人は通さない");
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;

    let _ = installs.uninstall(&installer, app_id, None).await;
    realm.delete_client(&b1).await;
    for sql in [
        "DELETE FROM app_installation WHERE tenant_id = $1 AND app_id = $2",
        "DELETE FROM app_capability_usage WHERE tenant_id = $1 AND app_id = $2",
        "DELETE FROM registry_entry WHERE tenant_id = $1 AND artifact_id = $2",
        "DELETE FROM artifact_version WHERE tenant_id = $1 AND artifact_id = $2",
        "DELETE FROM artifact WHERE tenant_id = $1 AND id = $2",
        "DELETE FROM storage_event_outbox WHERE tenant_id = $1 AND node_id = $2",
    ] {
        sqlx::query(sql)
            .bind(TENANT)
            .bind(app_id)
            .execute(&pool)
            .await
            .expect(sql);
    }
    sqlx::query("DELETE FROM audit_log WHERE tenant_id = $1 AND object_id = $2")
        .bind(TENANT)
        .bind(app_id.to_string())
        .execute(&pool)
        .await
        .expect("audit cleanup");
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
