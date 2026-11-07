//! 実 Keycloak・実 OpenFGA・実 Postgres で、B1 のアプリを入れてゲートウェイを立てる試しの手助け
//! （[`super::org_member_it`]・[`super::membership_reconcile_it`]）。
//!
//! テナントは realm の試しの利用者に合わせて `a-corp`（multi）。OpenFGA のストアは試しごとに作り、作ったクライアント・
//! 行・ストアは [`GatewayFixture::cleanup`] で消す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::large_futures,
    clippy::too_many_lines
)]

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use app_gateway::test_support::TestRealm;
use app_platform::{
    Budget, InstallRequest, InstallService, MiniAppCodeStore, MiniAppManifest, Registry, TrustTier,
};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind};
use data::RefResolver;
use storage::{ObjectStore, ObjectStoreError};
use uuid::Uuid;

/// web シェルのホスト支援 PKCE の戻り先（web/src/lib/miniapp-oauth.ts と同じ）。
pub(super) const SHELL_CALLBACK: &str = "http://localhost:3000/apps/oauth/callback";
/// alice・bob のテナントと組織（deploy/keycloak/shiki-realm.json）。
pub(super) const TENANT: &str = "a-corp";

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

/// 参照の確かめ（この試しのアプリはテーブルを持たない）。
pub(super) struct NoRefs;

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

/// B1 のアプリを `a-corp` に入れたゲートウェイ（本物の鍵・本物の OpenFGA）。
pub(super) struct GatewayFixture {
    pub(super) pool: sqlx::PgPool,
    pub(super) authz: Arc<dyn AuthzClient>,
    pub(super) data: Arc<data::DataStore>,
    pub(super) app_id: Uuid,
    /// アプリの B1 のクライアント（`app-<id>-b1`）。
    pub(super) b1: String,
    fga_url: String,
    store_id: String,
    installs: InstallService,
    code: Arc<MiniAppCodeStore>,
    installer: AuthContext,
    name: String,
    origin: OnceLock<String>,
    http: reqwest::Client,
}

impl GatewayFixture {
    /// 試しの DB・OpenFGA のストア・アプリ（未公開）を用意する。`KEYCLOAK_TEST_URL`・`STORAGE_TEST_DATABASE_URL`・
    /// `OPENFGA_TEST_URL` のどれかが無ければ `None`（試しはスキップする）。
    pub(super) async fn new(label: &str) -> Option<(TestRealm, Self)> {
        let (Some(realm), Ok(db_url), Ok(fga_url)) = (
            TestRealm::from_env(),
            std::env::var("STORAGE_TEST_DATABASE_URL"),
            std::env::var("OPENFGA_TEST_URL"),
        ) else {
            eprintln!(
                "KEYCLOAK_TEST_URL・STORAGE_TEST_DATABASE_URL・OPENFGA_TEST_URL のどれかが無いためスキップ"
            );
            return None;
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
                store_name: format!("{label}-{}", Uuid::new_v4()),
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
            authz::TenantId::parse(TENANT).unwrap(),
        );
        let name = format!("{label}-{}", Uuid::new_v4().simple());
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
        let fixture = GatewayFixture {
            pool,
            authz,
            data,
            app_id,
            b1: format!("app-{app_id}-b1"),
            fga_url,
            store_id,
            installs,
            code,
            installer,
            name,
            origin: OnceLock::new(),
            http: reqwest::Client::new(),
        };
        Some((realm, fixture))
    }

    /// アプリを公開してインストールし（B1 のクライアントを Keycloak に登録する）、ゲートウェイを立てる。
    pub(super) async fn install_and_serve(&self, realm: &TestRealm) {
        self.code
            .publish(&self.installer, self.app_id, None, None, None)
            .await
            .expect("publish");
        self.installs
            .install(
                &self.installer,
                InstallRequest {
                    name: self.name.clone(),
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let pool = self.pool.clone();
        let authz = self.authz.clone();
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
                data: Arc::clone(&self.data),
                fsms: Arc::new(data::FsmStore::new(
                    Arc::new(artifact::ArtifactStore::new(pool.clone(), authz.clone())),
                    (*self.data).clone(),
                )),
                rag: Arc::new(app_gateway::NoRag),
                notifications: app_gateway::NotificationStore::new(pool.clone()),
                llm: None,
                agent: Arc::new(app_gateway::NoAgent),
                ai_daily_cap_usd_micros: 0,
                functions: Arc::new(app_gateway::NoFunctions),
            },
        });
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        self.origin.set(origin).expect("一度だけ立てる");
    }

    /// `GET /gw/whoami` の状態コードと本文。
    pub(super) async fn whoami(&self, token: &str) -> (u16, serde_json::Value) {
        let origin = self.origin.get().expect("install_and_serve の後に呼ぶ");
        let resp = self
            .http
            .get(format!("{origin}/gw/whoami"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        (status, body)
    }

    /// アンインストールし、B1 のクライアント・アプリの行・監査の行・OpenFGA のストアを消す。
    pub(super) async fn cleanup(self, realm: &TestRealm) {
        let _ = self
            .installs
            .uninstall(&self.installer, self.app_id, None)
            .await;
        realm.delete_client(&self.b1).await;
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
                .bind(self.app_id)
                .execute(&self.pool)
                .await
                .expect(sql);
        }
        sqlx::query("DELETE FROM audit_log WHERE tenant_id = $1 AND object_id = $2")
            .bind(TENANT)
            .bind(self.app_id.to_string())
            .execute(&self.pool)
            .await
            .expect("audit cleanup");
        let deleted = reqwest::Client::new()
            .delete(format!("{}/stores/{}", self.fga_url, self.store_id))
            .send()
            .await
            .expect("delete store");
        assert!(deleted.status().is_success(), "{}", deleted.status());
    }
}
