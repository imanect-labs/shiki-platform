//! gui の結合テストの、使う人の権限の確かめの環境（実 Postgres・テストごとの OpenFGA のストア・テーブル・ドライブ）。
//!
//! 権限の確かめ（`access_check_it`）と部品の版の差（`miniapp_pins_it`）が使う。テストごとに使う物が違うので、使わない
//! 物の警告は外す。

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use artifact::ArtifactStore;
use async_trait::async_trait;
use authz::{AuthContext, AuthzClient, Principal};
use data::{DataStore, DataViewStore, FieldDef, FieldType, NewDataTable, RefResolver};
use gui::{AccessChecker, MiniAppStore, SkillStore};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::StorageService;
use uuid::Uuid;

use super::common;

pub(crate) struct AllowRefs;
#[async_trait]
impl RefResolver for AllowRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(true)
    }
}

/// フォルダーだけを作るテストのオブジェクトストア（バイトは扱わない）。
pub(crate) struct NoBytes;
#[async_trait]
impl storage::ObjectStore for NoBytes {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(
        &self,
        _: &str,
        _: Duration,
        _: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn presign_get_internal(
        &self,
        _: &str,
        _: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn read_and_hash(&self, _: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn put_object(
        &self,
        _: &str,
        _: Vec<u8>,
        _: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn get_object(&self, _: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn exists(&self, _: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("none".into()))
    }
    async fn delete(&self, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((Vec::new(), None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

pub(crate) async fn pool() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

/// テストごとの OpenFGA のストア（最後に消す）。
pub(crate) struct Fga {
    pub(crate) client: Arc<dyn AuthzClient>,
    base_url: String,
    store_id: String,
}

impl Fga {
    pub(crate) async fn connect() -> Option<Fga> {
        let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
            eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
            return None;
        };
        use authz::client::{OpenFgaClient, OpenFgaConfig};
        let client = OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: base_url.clone(),
                store_name: format!("shiki-access-check-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("OpenFGA 接続");
        let store_id = client.store_id().to_string();
        Some(Fga {
            client: Arc::new(client),
            base_url,
            store_id,
        })
    }

    pub(crate) async fn delete_store(&self) {
        let resp = reqwest::Client::new()
            .delete(format!("{}/stores/{}", self.base_url, self.store_id))
            .send()
            .await
            .expect("OpenFGA delete store");
        assert!(
            resp.status().is_success(),
            "delete store: {}",
            resp.status()
        );
    }
}

pub(crate) fn ctx(tenant: &str, user: &str) -> AuthContext {
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

/// 本体を走らせ、落ちても後片付け（テナントの行・OpenFGA のストア）をしてから結果を返す。
pub(crate) async fn run_with_cleanup<F>(pool: PgPool, tenant: String, fga: Fga, body: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    common::purge_tenant_rows(&pool, &tenant).await;
    fga.delete_store().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

pub(crate) struct Env {
    pub(crate) fga: Arc<dyn AuthzClient>,
    pub(crate) artifacts: Arc<ArtifactStore>,
    pub(crate) data: DataStore,
    pub(crate) storage: StorageService,
    pub(crate) mini_apps: MiniAppStore,
    pub(crate) skills: SkillStore,
    pub(crate) views: DataViewStore,
}

impl Env {
    pub(crate) fn new(pool: &PgPool, fga: &Arc<dyn AuthzClient>) -> Env {
        let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::clone(fga)));
        let data = DataStore::new(pool.clone(), Arc::clone(fga), Arc::new(AllowRefs));
        Env {
            fga: Arc::clone(fga),
            storage: StorageService::new(
                pool.clone(),
                Arc::new(NoBytes),
                Arc::clone(fga),
                Duration::from_secs(60),
                Duration::from_secs(60),
                1024,
            ),
            mini_apps: MiniAppStore::new(Arc::clone(&artifacts), pool.clone()),
            skills: SkillStore::new(Arc::clone(&artifacts)),
            views: DataViewStore::new(Arc::clone(&artifacts), data.clone()),
            data,
            artifacts,
        }
    }

    pub(crate) fn checker(&self) -> AccessChecker<'_> {
        AccessChecker {
            authz: self.fga.as_ref(),
            artifacts: &self.artifacts,
            data: &self.data,
            storage: &self.storage,
        }
    }

    pub(crate) async fn table(&self, owner: &AuthContext, name: &str) -> Uuid {
        self.data
            .create_table(
                owner,
                NewDataTable {
                    name: name.into(),
                    schema: data::TableSchema {
                        fields: vec![FieldDef::new("title", FieldType::Text)],
                        status_field: None,
                        row_policy: None,
                        field_policy: Vec::new(),
                        aggregate_min_rows: None,
                        fsm_ref: None,
                        rag: None,
                    },
                },
                None,
            )
            .await
            .expect("table")
            .id
    }
}
