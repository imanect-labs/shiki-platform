//! B2 の自動実行（サービスアカウントのトークン）をアプリの主体として扱う結合テスト（BUG-183）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）でインストール台帳とアプリのテーブルを用意し、RSA 署名の Bearer
//! トークンでゲートウェイを叩く。認可は記録するスタブ（評価した主体を残す）。OpenFGA の本物で「アプリの主体は
//! アプリのテーブルにだけ届く」ことは、実 Keycloak の `keycloak_b2_it.rs` が確かめる。
//!
//! - `azp` が有効なインストールの B2 で、`sub` がそのサービスアカウントの利用者のトークンだけが、アプリの主体
//!   （`miniapp:<tenant>|<app_id>`）になる。テナントはトークンのクレームではなくインストールから決める。
//! - 同じ B2 が交換した利用者のトークン（`sub` は利用者）・別のクライアントのトークン・アンインストールした
//!   アプリのトークンは、アプリの主体にならない。
//! - アプリの主体が通れるのはアプリのテーブルと通知の記録だけ（身元・文書・関数の起動は断る）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use app_gateway::{build_gateway_router, AiPin, AppInstallationStore, NewAppInstallation};
use async_trait::async_trait;
use authz::{
    AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, ReadTupleKey, Relation, Subject,
};
use axum::http::StatusCode;
use common::{ctx, get, request_json, setup, signed, state_with, AUDIENCE, ISSUER};
use data::{DataStore, FieldDef, FieldType, NewDataTable, TableSchema};
use futures::FutureExt as _;
use uuid::Uuid;

/// テストの本体を走らせ、落ちても片付けてから落とす。
async fn run_then<F: std::future::Future<Output = ()>>(f: &Fixture, body: F) {
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    cleanup(f).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// すべて許し、check と ListObjects で評価した主体を残す認可。
#[derive(Default)]
struct Recording {
    subjects: Mutex<Vec<String>>,
}

impl Recording {
    fn seen(&self) -> Vec<String> {
        self.subjects.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuthzClient for Recording {
    async fn check(
        &self,
        subject: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        self.subjects
            .lock()
            .unwrap()
            .push(subject.as_str().to_string());
        Ok(true)
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
        subject: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.subjects
            .lock()
            .unwrap()
            .push(subject.as_str().to_string());
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

fn title_schema() -> TableSchema {
    TableSchema {
        fields: vec![FieldDef::new("title", FieldType::Text)],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

/// サービスアカウントのトークン（client_credentials と同じ形・tenant も groups も無い）。
fn service_token(client_id: &str, sub: &str, scope: &str) -> String {
    signed(&serde_json::json!({
        "sub": sub, "azp": client_id, "client_id": client_id, "scope": scope,
        "aud": AUDIENCE, "iss": ISSUER, "exp": 9_999_999_999u64,
    }))
}

struct Fixture {
    pool: sqlx::PgPool,
    tenant: String,
    app_id: Uuid,
    b2: String,
    sa: String,
    table: Uuid,
}

/// B2 を持つインストール（サービスアカウントの利用者つき）と、アプリのテーブルを 1 つ用意する。
async fn fixture(pool: sqlx::PgPool) -> Fixture {
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let b2 = format!("app-{app_id}-b2");
    let sa = Uuid::new_v4().to_string();
    let store = AppInstallationStore::new(pool.clone());
    let scopes: Vec<String> = [
        "data.read",
        "data.write",
        "notify.send",
        "identity.read",
        "rag.query",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    store
        .upsert(
            &ctx(&tenant),
            NewAppInstallation {
                app_id,
                app_name: "経費",
                installed_version: "1.0.0",
                granted_scopes: &scopes,
                client_id_b1: Some(&format!("app-{app_id}-b1")),
                client_id_b2: Some(&b2),
                ai: AiPin::default(),
                frontend_bundle: None,
                server_bundle: Some("sha"),
                server_spec: Some(serde_json::json!({ "functions": ["f"] })),
            },
        )
        .await
        .expect("install");
    store
        .mark_clients_repaired(&tenant, app_id, "spec", Some(&sa), false)
        .await
        .expect("service account");
    let data = DataStore::new(
        pool.clone(),
        Arc::new(Recording::default()),
        Arc::new(common::FixedResolver),
    );
    let table = data
        .create_table_for_app(
            &ctx(&tenant),
            NewDataTable {
                name: "経費".into(),
                schema: title_schema(),
            },
            app_id,
            None,
        )
        .await
        .expect("table")
        .id;
    Fixture {
        pool,
        tenant,
        app_id,
        b2,
        sa,
        table,
    }
}

async fn cleanup(f: &Fixture) {
    for sql in [
        "DELETE FROM app_installation WHERE tenant_id = $1",
        "DELETE FROM app_capability_usage WHERE tenant_id = $1",
        "DELETE FROM app_notification WHERE tenant_id = $1",
        "DELETE FROM audit_log WHERE tenant_id = $1",
    ] {
        sqlx::query(sql)
            .bind(&f.tenant)
            .execute(&f.pool)
            .await
            .expect(sql);
    }
    DataStore::new(
        f.pool.clone(),
        Arc::new(Recording::default()),
        Arc::new(common::FixedResolver),
    )
    .purge_tenant(&f.tenant)
    .await
    .expect("purge");
}

#[tokio::test]
async fn service_tokens_act_as_the_app_with_the_installation_tenant() {
    let Some(pool) = setup().await else { return };
    let f = fixture(pool.clone()).await;
    run_then(&f, service_body(&f)).await;
}

async fn service_body(f: &Fixture) {
    let pool = f.pool.clone();
    let authz = Arc::new(Recording::default());
    let mut gw = state_with(pool.clone(), &f.tenant, authz.clone());
    // multi テナント: 利用者のトークンは tenant のクレームが要る。サービスアカウントのトークンには無い。
    gw.tenant_source = authz::TenantSource::Claim;
    let app = build_gateway_router(gw);
    let scope = "data.read data.write notify.send identity.read rag.query";
    let svc = service_token(&f.b2, &f.sa, scope);

    let (s, body) = get(&app, "/gw/whoami", Some(&svc)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["principal"], "app");
    assert_eq!(body["app_id"], f.app_id.to_string());

    // アプリのテーブルを、インストールのテナントで、アプリの主体として読む。
    authz.subjects.lock().unwrap().clear();
    let (s, body) = get(
        &app,
        &format!("/gw/data/tables/{}/records", f.table),
        Some(&svc),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let app_subject = format!("miniapp:{}|{}", f.tenant, f.app_id);
    let seen = authz.seen();
    assert!(
        !seen.is_empty() && seen.iter().all(|s| *s == app_subject),
        "{seen:?}"
    );
    let (s, body) = request_json(
        &app,
        "POST",
        &format!("/gw/data/tables/{}/records", f.table),
        Some(&svc),
        &serde_json::json!({ "data": { "title": "自動で足した行" } }),
    )
    .await;
    assert!(s.is_success(), "{s} {body}");
    // 追加した主体はアプリ（利用者ではない）。
    assert_eq!(body["owner"], f.app_id.to_string(), "{body}");
    let (s, body) = request_json(
        &app,
        "POST",
        "/gw/notify/send",
        Some(&svc),
        &serde_json::json!({ "recipient": "alice", "title": "締め切り" }),
    )
    .await;
    assert!(s.is_success(), "{s} {body}");

    // トークンに別のテナントを書いても、テナントはインストールのもの（クレームでテナントを選ばせない）。
    let claimed = signed(&serde_json::json!({
        "sub": f.sa, "azp": f.b2, "tenant": "other-tenant", "scope": scope,
        "aud": AUDIENCE, "iss": ISSUER, "exp": 9_999_999_999u64,
    }));
    authz.subjects.lock().unwrap().clear();
    let (s, body) = get(
        &app,
        &format!("/gw/data/tables/{}/records", f.table),
        Some(&claimed),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(authz.seen().iter().all(|s| *s == app_subject));

    // アプリの主体は、利用者の身元・文書・関数の起動には届かない（スコープが付与されていても）。
    for (method, path) in [
        ("GET", "/gw/identity/me"),
        ("POST", "/gw/rag/query"),
        ("POST", "/gw/apps/functions/f/invoke"),
    ] {
        let (s, body) = request_json(
            &app,
            method,
            path,
            Some(&svc),
            &serde_json::json!({ "query": "経費" }),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {path}: {body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("アプリのテーブルと通知"),
            "{body}"
        );
    }
}

#[tokio::test]
async fn other_tokens_are_not_taken_for_the_app() {
    let Some(pool) = setup().await else { return };
    let f = fixture(pool.clone()).await;
    run_then(&f, others_body(&f)).await;
}

async fn others_body(f: &Fixture) {
    let pool = f.pool.clone();
    let store = AppInstallationStore::new(pool.clone());
    let authz = Arc::new(Recording::default());
    let mut multi = state_with(pool.clone(), &f.tenant, authz.clone());
    multi.tenant_source = authz::TenantSource::Claim;
    let multi = build_gateway_router(multi);
    let single = build_gateway_router(state_with(pool.clone(), &f.tenant, authz.clone()));

    // 同じ B2 が交換した利用者のトークン（sub は利用者・tenant あり）は、利用者のまま。
    let exchanged = signed(&serde_json::json!({
        "sub": "alice", "azp": f.b2, "tenant": f.tenant, "scope": "data.read",
        "aud": AUDIENCE, "iss": ISSUER, "exp": 9_999_999_999u64,
    }));
    let (s, body) = get(&multi, "/gw/whoami", Some(&exchanged)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["principal"], "user");
    assert_eq!(body["user_sub"], "alice");
    authz.subjects.lock().unwrap().clear();
    let (s, _) = get(
        &multi,
        &format!("/gw/data/tables/{}/records", f.table),
        Some(&exchanged),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let user_subject = format!("user:{}|alice", f.tenant);
    assert!(
        authz.seen().iter().all(|s| *s == user_subject),
        "{:?}",
        authz.seen()
    );

    // サービスアカウントの利用者でも、別のクライアントのトークンはアプリの主体にならない（multi は tenant が無く 401）。
    let other_client = service_token("app-other-b2", &f.sa, "data.read");
    assert_eq!(
        get(&multi, "/gw/whoami", Some(&other_client)).await.0,
        StatusCode::UNAUTHORIZED
    );
    // B2 のクライアントでも、サービスアカウントの利用者でない sub は利用者として扱う（single では利用者の経路）。
    let wrong_sub = service_token(&f.b2, &Uuid::new_v4().to_string(), "data.read");
    let (s, body) = get(&single, "/gw/whoami", Some(&wrong_sub)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["principal"], "user");

    // アンインストールしたアプリのサービスアカウントのトークンは、アプリの主体にならない（multi は 401・single は 403）。
    store
        .revoke(&ctx(&f.tenant), f.app_id)
        .await
        .expect("revoke");
    let svc = service_token(&f.b2, &f.sa, "data.read");
    assert_eq!(
        get(&multi, "/gw/whoami", Some(&svc)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&single, "/gw/whoami", Some(&svc)).await.0,
        StatusCode::FORBIDDEN
    );
}
