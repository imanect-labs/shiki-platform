//! 定期の照合（[`super::reconcile_page`]）が、id（sub か tenant_id）が識別子に使えない人を数えて飛ばすことの試し
//! （D-205・GAP-77）。
//!
//! 外の依存は要らない。Keycloak はトークンだけを返す試しのもの、OpenFGA は呼ばれた数を数える置き換え、DB は届かない
//! 接続にする。飛ばす人だけのページなら、ロールの記録（DB）も OpenFGA も Keycloak の所属も読まないことを、呼ばれた
//! 数と、届かない DB で落ちないことで確かめる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, ReadTupleKey,
    Relation, Subject,
};
use storage::{DirectoryEntry, DirectoryStore};
use uuid::Uuid;

use super::{reconcile_page, ReconcileDeps, ReconcileReport};
use crate::config::{AuthConfig, Tenancy};
use crate::keycloak_admin::KeycloakAdmin;

/// 呼ばれた数を数える OpenFGA の置き換え（タプルは 1 つも無い）。
#[derive(Default)]
struct Counting(AtomicUsize);

impl Counting {
    fn bump<T>(&self, value: T) -> Result<T, AuthzError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(value)
    }
}

#[async_trait]
impl AuthzClient for Counting {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        self.bump(false)
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.bump(false)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.bump(false)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        self.bump(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.bump(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        self.bump(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.bump(vec![])
    }
}

/// 構造化データの参照を引かない置き換え（照合は使わない）。
struct NoRefs;

#[async_trait]
impl data::RefResolver for NoRefs {
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

/// トークンだけを返す試しの Keycloak。ほかの道（所属グループ・属性）を読まれたら数えて 500 を返す。base の URL を返す。
async fn mock_keycloak(other_reads: Arc<AtomicUsize>) -> String {
    use axum::{http::StatusCode, routing, Json, Router};
    let app = Router::new()
        .route(
            "/realms/shiki/protocol/openid-connect/token",
            routing::post(|| async { Json(serde_json::json!({ "access_token": "t" })) }),
        )
        .fallback(move || {
            other_reads.fetch_add(1, Ordering::SeqCst);
            async { StatusCode::INTERNAL_SERVER_ERROR }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn auth(base: &str) -> AuthConfig {
    AuthConfig {
        issuer: format!("{base}/realms/shiki"),
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
        provisioner_client_id: Some("p".into()),
        provisioner_client_secret: Some("s".into()),
        admin_base_url: Some(format!("{base}/admin/realms/shiki")),
    }
}

#[tokio::test]
async fn users_with_unusable_ids_are_counted_and_skipped_without_reading_anything() {
    let other_reads = Arc::new(AtomicUsize::new(0));
    let base = mock_keycloak(Arc::clone(&other_reads)).await;
    let fga = Arc::new(Counting::default());
    let authz: Arc<dyn AuthzClient> = fga.clone();
    // 届かない DB（ロールの記録を読みに行けば、照合が失敗して下の unwrap で落ちる）。
    let db = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy("postgres://postgres@127.0.0.1:1/none")
        .unwrap();
    let deps = ReconcileDeps {
        db: db.clone(),
        authz: Arc::clone(&authz),
        directory: Arc::new(DirectoryStore::new(db.clone())),
        data: Arc::new(data::DataStore::new(db, authz, Arc::new(NoRefs))),
        http: reqwest::Client::new(),
        auth: auth(&base),
        batch_size: 10,
        interval: Duration::ZERO,
    };
    let admin = KeycloakAdmin::from_config(&deps.http, &deps.auth).unwrap();
    let mut kc = admin.session().await.unwrap();

    // 入口が断る sub（フェデレーションで Import を切った人・userset の区切り・テナントの区切り・空白）と、使えない
    // tenant_id。
    let page: Vec<DirectoryEntry> = [
        ("acme", "f:ldap:bob"),
        ("acme", "a#b"),
        ("acme", "a|b"),
        ("acme", "a b"),
        ("ac|me", "alice"),
    ]
    .into_iter()
    .map(|(tenant_id, user_id)| DirectoryEntry {
        tenant_id: tenant_id.into(),
        user_id: user_id.into(),
    })
    .collect();
    let mut report = ReconcileReport::default();
    let done = reconcile_page(&deps, &mut kc, &page, &mut report)
        .await
        .expect("飛ばす人だけのページは DB を読まない");

    // 飛ばした人も終えた人に数える（続きの位置がこの人たちで止まらない）。
    assert_eq!(done, page.len());
    assert_eq!(
        report,
        ReconcileReport {
            skipped_invalid_id: 5,
            ..ReconcileReport::default()
        }
    );
    assert_eq!(fga.0.load(Ordering::SeqCst), 0, "OpenFGA は読まない");
    assert_eq!(
        other_reads.load(Ordering::SeqCst),
        0,
        "Keycloak の所属は読まない"
    );
}
