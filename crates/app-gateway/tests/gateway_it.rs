//! 公開 API ゲートウェイの二重ゲート結合テスト（Task 9.6 受け入れ条件）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）でインストール台帳を用意し、RSA 署名の
//! Bearer トークンで whoami/probe を叩く。①未認証 401 ②未インストール/失効 403
//! ③スコープ未付与 403 ④広トークンでも granted に無ければ 403（同意失効の即時反映）を検証する。
//! authz はモック（per-call FGA はハンドラ側＝PR7 の責務）。JWKS はテスト鍵の Resolver で代替。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use app_gateway::{
    build_gateway_router, AppInstallationStore, GatewayState, GatewayTokenConfig, KeyResolver,
    NewAppInstallation,
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
use http_body_util::BodyExt;
use jsonwebtoken::{encode, Algorithm, DecodingKey, EncodingKey, Header};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::audit::AuditRecorder;
use tower::ServiceExt;
use uuid::Uuid;

const PRIV_PEM: &str = include_str!("fixtures/test_rsa_priv.pem");
const PUB_PEM: &str = include_str!("fixtures/test_rsa_pub.pem");
const ISSUER: &str = "https://kc.example/realms/shiki";
const AUDIENCE: &str = "shiki-gateway";

struct AllowAll;
#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(&self, _: &Subject, _: Relation, _: &FgaObject) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(&self, _: &Subject, _: Relation, _: &FgaObject) -> Result<bool, AuthzError> {
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
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// テスト鍵（固定 RSA 公開鍵）を返す Resolver（JWKS の代替）。
struct StaticKey(DecodingKey);
#[async_trait]
impl KeyResolver for StaticKey {
    async fn resolve(&self, _kid: &str) -> Result<DecodingKey, app_gateway::GatewayError> {
        Ok(self.0.clone())
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

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "admin".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

fn state(pool: PgPool, tenant: &str) -> GatewayState {
    GatewayState {
        installations: AppInstallationStore::new(pool.clone()),
        keys: Arc::new(StaticKey(DecodingKey::from_rsa_pem(PUB_PEM.as_bytes()).unwrap())),
        token_cfg: GatewayTokenConfig {
            audience: AUDIENCE.into(),
            issuer: ISSUER.into(),
        },
        authz: Arc::new(AllowAll),
        audit: AuditRecorder::new(pool),
        default_tenant: tenant.into(),
        default_org: "acme".into(),
    }
}

/// RSA 署名の access token（sub/azp/scope/tenant を載せる）。
fn token(client_id: &str, scope: &str, tenant: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let claims = serde_json::json!({
        "sub": "alice", "azp": client_id, "tenant": tenant, "scope": scope,
        "aud": AUDIENCE, "iss": ISSUER, "exp": 9_999_999_999u64,
    });
    encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(PRIV_PEM.as_bytes()).unwrap(),
    )
    .unwrap()
}

async fn get(app: &axum::Router, path: &str, bearer: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().uri(path);
    if let Some(t) = bearer {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn dual_gate_enforces_token_installation_and_scope() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let client_b1 = format!("app-{}", Uuid::new_v4());
    let store = AppInstallationStore::new(pool.clone());

    // data.read を付与したインストール（B1 client）。
    store
        .upsert(
            &ctx(&tenant),
            NewAppInstallation {
                app_id,
                app_name: "経費",
                installed_version: "1.0.0",
                granted_scopes: &["data.read".to_string()],
                client_id_b1: Some(&client_b1),
                client_id_b2: None,
            },
        )
        .await
        .expect("install");

    let app = build_gateway_router(state(pool.clone(), &tenant));

    // ① トークン無し → 401。
    let (s, _) = get(&app, "/gw/whoami", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // ② 有効トークン＋インストール済み → whoami 200（granted_scopes を返す）。
    let tok = token(&client_b1, "openid data.read", &tenant);
    let (s, body) = get(&app, "/gw/whoami", Some(&tok)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["app_id"], app_id.to_string());
    assert_eq!(body["user_sub"], "alice");

    // ③ data.read プローブ → 200（granted かつ token scope 内）。
    let (s, _) = get(&app, "/gw/probe", Some(&tok)).await;
    assert_eq!(s, StatusCode::OK);

    // ④ token に data.read が無ければ probe 403（scope マップ強制）。
    let narrow = token(&client_b1, "openid", &tenant);
    let (s, _) = get(&app, "/gw/probe", Some(&narrow)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ⑤ 未登録 client（azp 不一致）→ 403（有効インストール無し）。
    let other = token("unknown-client", "data.read", &tenant);
    let (s, _) = get(&app, "/gw/whoami", Some(&other)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ⑥ アンインストール（revoke）→ token 有効期限内でも 403（即時失効）。
    store.revoke(&ctx(&tenant), app_id).await.expect("revoke");
    let (s, _) = get(&app, "/gw/whoami", Some(&tok)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn broad_token_without_grant_is_denied() {
    // 広いトークン scope（data.read/write）でも、granted_scopes に無ければ 403（同意が上限）。
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let client = format!("app-{}", Uuid::new_v4());
    let store = AppInstallationStore::new(pool.clone());
    store
        .upsert(
            &ctx(&tenant),
            NewAppInstallation {
                app_id,
                app_name: "narrow",
                installed_version: "1.0.0",
                // data.read を付与しない（同意は狭い）。
                granted_scopes: &["identity.read".to_string()],
                client_id_b1: Some(&client),
                client_id_b2: None,
            },
        )
        .await
        .expect("install");
    let app = build_gateway_router(state(pool.clone(), &tenant));

    let broad = token(&client, "data.read data.write identity.read", &tenant);
    // whoami（スコープ不要）は通る。
    let (s, _) = get(&app, "/gw/whoami", Some(&broad)).await;
    assert_eq!(s, StatusCode::OK);
    // probe（data.read 必要）は granted に無いので 403。
    let (s, _) = get(&app, "/gw/probe", Some(&broad)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
