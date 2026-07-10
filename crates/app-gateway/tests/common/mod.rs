//! app-gateway 結合テストの共通ハーネス（Task 9.6/9.8）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋モック authz/JWKS/ObjectStore/RagPort で
//! [`GatewayState`] を丸ごと構築する。OpenFGA 実体は不要（第4ゲートの真偽はスタブで制御し、
//! FGA そのものの正しさは authz/data クレートの IT が担う）。

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::duration_suboptimal_units
)]

use std::sync::Arc;
use std::time::Duration;

use app_gateway::{
    AppInstallationStore, CapabilityDeps, GatewayError, GatewayState, GatewayTokenConfig,
    KeyResolver, NotificationStore, RagHit, RagPort,
};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
};
use data::{DataStore, FsmStore, RefResolver};
use http_body_util::BodyExt;
use jsonwebtoken::{encode, Algorithm, DecodingKey, EncodingKey, Header};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::audit::AuditRecorder;
use storage::{ObjectStore, ObjectStoreError, StorageService};
use tower::ServiceExt;
use uuid::Uuid;

pub const PRIV_PEM: &str = include_str!("../fixtures/test_rsa_priv.pem");
pub const PUB_PEM: &str = include_str!("../fixtures/test_rsa_pub.pem");
pub const ISSUER: &str = "https://kc.example/realms/shiki";
pub const AUDIENCE: &str = "shiki-gateway";

/// 認可スタブ: `deny` で全 check を拒否・`roles` は read_subject_objects・
/// `objects` は list_objects（可視オブジェクト列挙）の応答。
#[derive(Default)]
pub struct StubAuthz {
    pub deny: bool,
    pub roles: Vec<String>,
    pub objects: Vec<String>,
}

impl StubAuthz {
    pub fn allow_all() -> Self {
        StubAuthz::default()
    }
    pub fn deny_all() -> Self {
        StubAuthz {
            deny: true,
            ..StubAuthz::default()
        }
    }
}

#[async_trait]
impl AuthzClient for StubAuthz {
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
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
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
        Ok(if self.deny {
            vec![]
        } else {
            self.objects.clone()
        })
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(self.roles.clone())
    }
}

/// テスト鍵（固定 RSA 公開鍵）を返す Resolver（JWKS の代替）。
pub struct StaticKey(pub DecodingKey);

#[async_trait]
impl KeyResolver for StaticKey {
    async fn resolve(&self, _kid: &str) -> Result<DecodingKey, GatewayError> {
        Ok(self.0.clone())
    }
}

/// ObjectStore スタブ（presign は固定 URL・バイト操作は未使用）。
pub struct StubObjectStore;

#[async_trait]
impl ObjectStore for StubObjectStore {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(&self, _: &str, _: Duration, _: i64) -> Result<String, ObjectStoreError> {
        Ok("https://obj.example/put".into())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        Ok("https://obj.example/get".into())
    }
    async fn presign_get_internal(&self, _: &str, _: Duration) -> Result<String, ObjectStoreError> {
        Ok("https://obj.internal/get".into())
    }
    async fn read_and_hash(&self, key: &str) -> Result<(String, u64), ObjectStoreError> {
        Err(ObjectStoreError::NotFound(key.into()))
    }
    async fn put_object(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, ObjectStoreError> {
        Err(ObjectStoreError::NotFound(key.into()))
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

/// 参照リゾルバのスタブ（alice/bob と role sales のみ存在・file は不可視）。
pub struct FixedResolver;

#[async_trait]
impl RefResolver for FixedResolver {
    async fn user_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(matches!(id, "alice" | "bob"))
    }
    async fn role_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(id == "sales")
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

/// RagPort スタブ（固定 1 ヒット。可読性の post-filter は SearchService 側の責務）。
pub struct StubRag;

#[async_trait]
impl RagPort for StubRag {
    async fn query(
        &self,
        _ctx: &AuthContext,
        query: &str,
        _top_k: Option<u32>,
        _trace_id: Option<&str>,
    ) -> Result<Vec<RagHit>, GatewayError> {
        Ok(vec![RagHit {
            chunk_id: Uuid::new_v4(),
            file_id: Uuid::new_v4(),
            file_name: "readable.pdf".into(),
            page: Some(1),
            heading_path: vec!["§1".into()],
            content: format!("hit for {query}"),
            score: 0.9,
        }])
    }
}

pub async fn setup() -> Option<PgPool> {
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

pub fn ctx(tenant: &str) -> AuthContext {
    ctx_as(tenant, "admin")
}

pub fn ctx_as(tenant: &str, user: &str) -> AuthContext {
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

/// フル装備の [`GatewayState`]（能力アダプタ込み・authz は指定スタブ）。
pub fn state_with(pool: PgPool, tenant: &str, authz: Arc<dyn AuthzClient>) -> GatewayState {
    let data = Arc::new(DataStore::new(
        pool.clone(),
        authz.clone(),
        Arc::new(FixedResolver),
    ));
    let storage_service = Arc::new(StorageService::new(
        pool.clone(),
        Arc::new(StubObjectStore),
        authz.clone(),
        Duration::from_secs(60),
        Duration::from_secs(60),
        1024 * 1024,
    ));
    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), authz.clone()));
    let fsms = Arc::new(FsmStore::new(artifacts, (*data).clone()));
    GatewayState {
        installations: AppInstallationStore::new(pool.clone()),
        keys: Arc::new(StaticKey(
            DecodingKey::from_rsa_pem(PUB_PEM.as_bytes()).unwrap(),
        )),
        token_cfg: GatewayTokenConfig {
            audience: AUDIENCE.into(),
            issuer: ISSUER.into(),
        },
        authz: authz.clone(),
        audit: AuditRecorder::new(pool.clone()),
        require_tenant_claim: false,
        default_tenant: tenant.into(),
        default_org: "acme".into(),
        caps: CapabilityDeps {
            db: pool.clone(),
            storage: storage_service,
            data,
            fsms,
            rag: Arc::new(StubRag),
            notifications: NotificationStore::new(pool),
        },
    }
}

pub fn state(pool: PgPool, tenant: &str) -> GatewayState {
    state_with(pool, tenant, Arc::new(StubAuthz::allow_all()))
}

/// RSA 署名の access token（sub/azp/scope/tenant を載せる）。
pub fn token(client_id: &str, scope: &str, tenant: &str) -> String {
    token_as(client_id, scope, tenant, "alice")
}

pub fn token_as(client_id: &str, scope: &str, tenant: &str, sub: &str) -> String {
    signed(&serde_json::json!({
        "sub": sub, "azp": client_id, "tenant": tenant, "scope": scope,
        "aud": AUDIENCE, "iss": ISSUER, "exp": 9_999_999_999u64,
    }))
}

/// tenant クレームを含まないトークン（multi テナンシーの拒否検証用）。
pub fn token_without_tenant(client_id: &str, scope: &str) -> String {
    signed(&serde_json::json!({
        "sub": "alice", "azp": client_id, "scope": scope,
        "aud": AUDIENCE, "iss": ISSUER, "exp": 9_999_999_999u64,
    }))
}

pub fn signed(claims: &serde_json::Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    encode(
        &header,
        claims,
        &EncodingKey::from_rsa_pem(PRIV_PEM.as_bytes()).unwrap(),
    )
    .unwrap()
}

pub async fn get(
    app: &axum::Router,
    path: &str,
    bearer: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().uri(path);
    if let Some(t) = bearer {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    send(app, req.body(Body::empty()).unwrap()).await
}

pub async fn request_json(
    app: &axum::Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: &serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    send(app, req.body(Body::from(body.to_string())).unwrap()).await
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}
