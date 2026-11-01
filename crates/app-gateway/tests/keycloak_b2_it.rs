//! 実 Keycloak で、B2（サーバーの関数）のトークンを確かめる（BUG-182・183）。
//!
//! `KEYCLOAK_TEST_URL`（realm `shiki` を import 済みの Keycloak・例 `http://localhost:8081`）が無ければスキップ。
//! 作ったクライアントはテストの最後に消す。モックの Keycloak（`keycloak_mock.rs`）は何でも通すので、Keycloak 26.2 の
//! 標準のトークンの交換の決まり（交換を頼むクライアントが元のトークンの aud に入っていること・audience はクライアント
//! の id だけ・scope は足すだけ）は、ここで本物を相手に確かめる。
//!
//! - 利用者が起点の呼び出し: 同じアプリの B2 が、B1 で取った利用者のトークンを交換できる。sub は利用者のまま、能力
//!   スコープは元のトークンより広がらない。別のアプリの B2 は交換できない。交換の相手を aud に足していない前の版の
//!   B1 は、揃えると交換できるようになる。アンインストール（B1 を無効）の後は、発行済みのトークンも交換できない。
//! - 自動実行（event・cron）: B2 のサービスアカウントのトークン（client_credentials）は、ゲートウェイでアプリの主体に
//!   なり、インストールが `owner@miniapp` を付けたアプリのテーブルにだけ届く（OpenFGA の本物で確かめる・
//!   `OPENFGA_TEST_URL` と `STORAGE_TEST_DATABASE_URL` も要る）。single と multi テナントの両方。交換した利用者の
//!   トークンは、利用者の権限のまま。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::large_futures
)]

mod common;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use app_gateway::test_support::{claims, TestRealm};
use app_gateway::{
    build_gateway_router, exchange_for_user, verify_gateway_token, AiPin, AppInstallationStore,
    ClientKind, GatewayError, GatewayTokenConfig, NewAppInstallation,
};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthzClient, CapabilityScope, Relation};
use axum::http::StatusCode;
use data::{DataStore, FieldDef, FieldType, NewDataTable, TableSchema};
use futures::FutureExt as _;
use uuid::Uuid;

/// テストの本体を走らせ、落ちても片付け（`cleanup`）をしてから落とす。
async fn run_then<F, C>(body: F, cleanup: C)
where
    F: std::future::Future<Output = ()>,
    C: std::future::Future<Output = ()>,
{
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    cleanup.await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// web シェルのホスト支援 PKCE の戻り先（web/src/lib/miniapp-oauth.ts と同じ）。
const SHELL_CALLBACK: &str = "http://localhost:3000/apps/oauth/callback";
/// realm の試しの利用者の sub（deploy/keycloak/shiki-realm.json）。
const ALICE: &str = "00000000-0000-0000-0000-000000000001";

fn gateway_cfg(realm: &TestRealm) -> GatewayTokenConfig {
    GatewayTokenConfig {
        audience: "shiki-gateway".into(),
        issuer: realm.issuer(),
    }
}

/// トークンの aud（1 つでも配列でも）。
fn audiences(token: &str) -> Vec<String> {
    match &claims(token)["aud"] {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => vec![],
    }
}

#[tokio::test]
async fn b2_exchanges_its_own_b1_users_tokens_without_widening() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let app = Uuid::new_v4();
    let (b1, b2) = (format!("app-{app}-b1"), format!("app-{app}-b2"));
    let other_b2 = format!("app-{}-b2", Uuid::new_v4());
    let oauth = realm.oauth_client();
    let http = reqwest::Client::new();
    let redirects = [SHELL_CALLBACK.to_string()];
    let body = async {
        oauth
            .register(ClientKind::PublicPkce, &b1, "経費", &redirects, Some(&b2))
            .await
            .expect("register b1");
        let secret = oauth
            .register(ClientKind::Confidential, &b2, "経費", &[], None)
            .await
            .expect("register b2")
            .client_secret
            .expect("b2 secret");
        let other_secret = oauth
            .register(ClientKind::Confidential, &other_b2, "別のアプリ", &[], None)
            .await
            .expect("register other b2")
            .client_secret
            .expect("other secret");

        // B1 のトークンの aud には、ゲートウェイと同じアプリの B2 が入る。
        let token = realm
            .user_token(&b1, SHELL_CALLBACK, "alice", "openid data.read")
            .await
            .expect("token");
        let aud = audiences(&token);
        assert!(aud.contains(&"shiki-gateway".to_string()), "{aud:?}");
        assert!(aud.contains(&b2), "{aud:?}");
        assert!(!aud.contains(&other_b2), "{aud:?}");

        // 同じアプリの B2 は交換できる。sub は利用者のまま、能力スコープは元のトークンのものだけ（B2 は能力スコープを
        // すべて任意のスコープに持つが、求めていないものは付かない）。ゲートウェイが受け付ける。
        let exchanged = exchange_for_user(&http, &realm.token_endpoint(), &b2, &secret, &token)
            .await
            .expect("exchange");
        let got = claims(&exchanged.access_token);
        assert_eq!(got["sub"], ALICE, "{got}");
        assert_eq!(got["azp"], b2.as_str(), "{got}");
        assert_eq!(got["tenant"], "a-corp", "{got}");
        let identity = verify_gateway_token(
            &exchanged.access_token,
            &realm.key_resolver(),
            &gateway_cfg(&realm),
        )
        .await
        .expect("verify exchanged");
        assert_eq!(identity.user_sub, ALICE);
        assert_eq!(identity.client_id, b2);
        assert_eq!(identity.token_scopes, [CapabilityScope::DataRead]);

        // 別のアプリの B2 は、このアプリの利用者のトークンを交換できない。
        let refused = exchange_for_user(
            &http,
            &realm.token_endpoint(),
            &other_b2,
            &other_secret,
            &token,
        )
        .await;
        assert!(
            matches!(refused, Err(GatewayError::Forbidden(_))),
            "{refused:?}"
        );

        // アンインストール（B1 を無効）の後は、発行済みのトークンも交換できない。
        oauth.set_enabled(&b1, false).await.expect("disable b1");
        let after_uninstall =
            exchange_for_user(&http, &realm.token_endpoint(), &b2, &secret, &token).await;
        assert!(
            matches!(after_uninstall, Err(GatewayError::Forbidden(_))),
            "{after_uninstall:?}"
        );
    };
    run_then(body, async {
        for c in [&b1, &b2, &other_b2] {
            realm.delete_client(c).await;
        }
    })
    .await;
}

/// 交換の相手を aud に足していない前の版の B1 のトークンは交換できず、起動時の照合と同じ直し方で揃えると交換できる。
#[tokio::test]
async fn repairing_a_previous_b1_lets_its_b2_exchange() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let app = Uuid::new_v4();
    let (b1, b2) = (format!("app-{app}-b1"), format!("app-{app}-b2"));
    let oauth = realm.oauth_client();
    let http = reqwest::Client::new();
    let redirects = [SHELL_CALLBACK.to_string()];
    let body = async {
        // 前の版の形（交換の相手を aud に足さない）。
        oauth
            .register(ClientKind::PublicPkce, &b1, "経費", &redirects, None)
            .await
            .expect("register b1");
        let secret = oauth
            .register(ClientKind::Confidential, &b2, "経費", &[], None)
            .await
            .expect("register b2")
            .client_secret
            .expect("b2 secret");
        let token = realm
            .user_token(&b1, SHELL_CALLBACK, "alice", "openid data.read")
            .await
            .expect("token");
        let refused = exchange_for_user(&http, &realm.token_endpoint(), &b2, &secret, &token).await;
        assert!(
            matches!(refused, Err(GatewayError::Forbidden(_))),
            "{refused:?}"
        );

        assert!(oauth
            .repair(ClientKind::PublicPkce, &b1, "経費", &redirects, Some(&b2))
            .await
            .expect("repair")
            .is_some());
        let token = realm
            .user_token(&b1, SHELL_CALLBACK, "alice", "openid data.read")
            .await
            .expect("token after repair");
        let exchanged = exchange_for_user(&http, &realm.token_endpoint(), &b2, &secret, &token)
            .await
            .expect("exchange after repair");
        assert_eq!(claims(&exchanged.access_token)["sub"], ALICE);
    };
    run_then(body, async {
        realm.delete_client(&b1).await;
        realm.delete_client(&b2).await;
    })
    .await;
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

/// B2 のクライアントの client_credentials のトークン（自動実行と同じ求め方・能力スコープを明示する）。
async fn service_token(realm: &TestRealm, client_id: &str, secret: &str, scope: &str) -> String {
    let resp = reqwest::Client::new()
        .post(realm.token_endpoint())
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", secret),
            ("scope", scope),
        ])
        .send()
        .await
        .expect("service token");
    assert!(resp.status().is_success(), "{}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    body["access_token"].as_str().unwrap().to_string()
}

/// [`service_tokens_reach_only_app_tables_in_single_and_multi_tenancy`] のテーブル。
struct Tables {
    app_id: Uuid,
    /// アプリのテーブル（インストールと同じく owner@miniapp を付ける）。
    app: Uuid,
    /// alice の表で、アプリに束縛したが owner@miniapp の無いもの。
    alice_bound: Uuid,
    /// alice の表で、アプリと関係の無いもの。
    alice_own: Uuid,
}

async fn tables(
    data: &DataStore,
    authz: &Arc<dyn AuthzClient>,
    installer: &authz::AuthContext,
    app_id: Uuid,
) -> Tables {
    let ns = installer.ns();
    let new_table = |name: &str| NewDataTable {
        name: name.into(),
        schema: title_schema(),
    };
    let app = data
        .create_table_for_app(installer, new_table("経費"), app_id, None)
        .await
        .expect("app table")
        .id;
    authz
        .write_tuple(
            &ns.miniapp_principal(&app_id.to_string()),
            Relation::Owner,
            &ns.data_table(&app.to_string()),
        )
        .await
        .expect("owner@miniapp");
    let alice = common::ctx_as(&installer.tenant_id, ALICE);
    let alice_bound = data
        .create_table_for_app(&alice, new_table("alice の控え"), app_id, None)
        .await
        .expect("alice bound table")
        .id;
    let alice_own = data
        .create_table(&alice, new_table("alice の表"), None)
        .await
        .expect("alice table")
        .id;
    Tables {
        app_id,
        app,
        alice_bound,
        alice_own,
    }
}

/// サービスアカウントのトークン（`svc`）はアプリの主体としてアプリのテーブルにだけ届き、交換した利用者のトークン
/// （`exchanged`）は利用者のまま。`multi` はゲートウェイが tenant のクレームでテナントを決めるか。
async fn check_principals(app: &axum::Router, multi: bool, t: &Tables, svc: &str, exchanged: &str) {
    let records = |table: Uuid| format!("/gw/data/tables/{table}/records");
    // テナントのクレームが無くても、テナントはインストールのもの。
    let (s, who) = common::get(app, "/gw/whoami", Some(svc)).await;
    assert_eq!(s, StatusCode::OK, "{who}");
    assert_eq!(who["principal"], "app", "{who}");
    let (s, body) = common::get(app, &records(t.app), Some(svc)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let (s, body) = common::request_json(
        app,
        "POST",
        &records(t.app),
        Some(svc),
        &serde_json::json!({ "data": { "title": "自動で足した行" } }),
    )
    .await;
    assert!(s.is_success(), "{s} {body}");
    assert_eq!(body["owner"], t.app_id.to_string());
    let (s, list) = common::get(app, "/gw/data/tables", Some(svc)).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let listed: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert_eq!(listed, [t.app.to_string()]);
    // 利用者のテーブルには届かない（アプリに束縛していても、owner@miniapp が無ければ届かない）。
    for table in [t.alice_bound, t.alice_own] {
        let (s, body) = common::get(app, &records(table), Some(svc)).await;
        assert!(
            matches!(s, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
            "{s} {body}"
        );
    }

    // 交換した利用者のトークンは利用者のまま（アプリの主体として扱わない）。
    let (s, who) = common::get(app, "/gw/whoami", Some(exchanged)).await;
    if multi {
        // alice は a-corp の人。このアプリは別のテナントに入れてあるので断る（交換してもテナントは越えない）。
        assert_eq!(claims(exchanged)["tenant"], "a-corp");
        assert_eq!(s, StatusCode::FORBIDDEN, "{who}");
        return;
    }
    assert_eq!(s, StatusCode::OK, "{who}");
    assert_eq!(who["principal"], "user");
    assert_eq!(who["user_sub"], ALICE);
    // 利用者の権限で見る: 自分の控えは読め、アプリのテーブル（alice に共有していない）は読めない。
    assert_eq!(
        common::get(app, &records(t.alice_bound), Some(exchanged))
            .await
            .0,
        StatusCode::OK
    );
    let (s, body) = common::get(app, &records(t.app), Some(exchanged)).await;
    assert!(
        matches!(s, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
        "{s} {body}"
    );
}

/// 実 Keycloak のサービスアカウントのトークンは、アプリの主体としてアプリのテーブルにだけ届く（single・multi）。
/// 同じ B2 が交換した利用者のトークンは、利用者の権限のまま（BUG-183・182）。
#[tokio::test]
async fn service_tokens_reach_only_app_tables_in_single_and_multi_tenancy() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let Some(pool) = common::setup().await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let Ok(fga_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("keycloak-b2-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA");
    let store_id = fga.store_id().to_string();
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let (b1, b2) = (format!("app-{app_id}-b1"), format!("app-{app_id}-b2"));
    let oauth = realm.oauth_client();
    let data = DataStore::new(pool.clone(), authz.clone(), Arc::new(common::FixedResolver));
    let body = async {
        oauth
            .register(
                ClientKind::PublicPkce,
                &b1,
                "経費",
                &[SHELL_CALLBACK.to_string()],
                Some(&b2),
            )
            .await
            .expect("register b1");
        let registered = oauth
            .register(ClientKind::Confidential, &b2, "経費", &[], None)
            .await
            .expect("register b2");
        let secret = registered.client_secret.expect("secret");
        let installations = AppInstallationStore::new(pool.clone());
        let installer = common::ctx(&tenant);
        installations
            .upsert(
                &installer,
                NewAppInstallation {
                    app_id,
                    app_name: "経費",
                    installed_version: "1.0.0",
                    granted_scopes: &["data.read".to_string(), "data.write".to_string()],
                    client_id_b1: Some(&b1),
                    client_id_b2: Some(&b2),
                    ai: AiPin::default(),
                    frontend_bundle: None,
                    server_bundle: Some("sha"),
                    server_spec: Some(serde_json::json!({ "functions": ["f"] })),
                },
            )
            .await
            .expect("install");
        installations
            .mark_clients_repaired(
                &tenant,
                app_id,
                "spec",
                registered.service_account_sub.as_deref(),
            )
            .await
            .expect("service account");

        let tables = tables(&data, &authz, &installer, app_id).await;
        let svc = service_token(&realm, &b2, &secret, "data.read data.write").await;
        let user = realm
            .user_token(&b1, SHELL_CALLBACK, "alice", "openid data.read")
            .await
            .expect("user token");
        let exchanged = exchange_for_user(
            &reqwest::Client::new(),
            &realm.token_endpoint(),
            &b2,
            &secret,
            &user,
        )
        .await
        .expect("exchange")
        .access_token;
        for source in [
            authz::TenantSource::Claim,
            authz::TenantSource::Fixed(tenant.clone()),
        ] {
            let mut gw = common::state_with(pool.clone(), &tenant, authz.clone());
            gw.keys = Arc::new(realm.key_resolver());
            gw.token_cfg = gateway_cfg(&realm);
            let multi = matches!(source, authz::TenantSource::Claim);
            gw.tenant_source = source;
            let app = build_gateway_router(gw);
            check_principals(&app, multi, &tables, &svc, &exchanged).await;
        }
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    realm.delete_client(&b1).await;
    realm.delete_client(&b2).await;
    for sql in [
        "DELETE FROM app_installation WHERE tenant_id = $1",
        "DELETE FROM app_capability_usage WHERE tenant_id = $1",
        "DELETE FROM audit_log WHERE tenant_id = $1",
    ] {
        sqlx::query(sql)
            .bind(&tenant)
            .execute(&pool)
            .await
            .expect(sql);
    }
    data.purge_tenant(&tenant).await.expect("purge");
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
