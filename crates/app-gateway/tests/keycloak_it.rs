//! 実 Keycloak で、アプリのクライアントの登録がゲートウェイに届くトークンを出すことを確かめる（BUG-172）。
//!
//! `KEYCLOAK_TEST_URL`（realm `shiki` を import 済みの Keycloak・例 `http://localhost:8081`）が無ければスキップ。
//! ゲートウェイまで通すテストは `STORAGE_TEST_DATABASE_URL` も要る。作ったクライアントとインストールの行は
//! テストの最後に消す。
//!
//! - シェルのポップアップの戻り先（`/apps/oauth/callback`）でコードを受け取れる。
//! - トークンに sub・tenant が載り、ゲートウェイが multi テナントで受け付ける。名前・メール・ロール・所属グループは
//!   載らない（所属グループは D-130）。別のテナントの人のトークンは、そのテナントにインストールが無いので断られる。
//! - 無効にしたクライアント（アンインストール）は、登録し直すと有効に戻る。
//! - 前の版で登録したクライアント（BFF の戻り先・既定のスコープ無し・マッパー無し、7d の版の groups のマッパー）を
//!   今の形に直す。

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
    build_gateway_router, verify_gateway_token, AiPin, AppInstallationStore, ClientKind,
    GatewayTokenConfig, NewAppInstallation,
};
use authz::CapabilityScope;
use axum::http::StatusCode;
use common::{ctx, get, setup, state};
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
/// 前の版のインストールが登録していた戻り先（BFF のログインの戻り先）。
const BFF_CALLBACK: &str = "http://localhost:3000/auth/callback";
/// realm の試しの利用者の sub（deploy/keycloak/shiki-realm.json）。
const ALICE: &str = "00000000-0000-0000-0000-000000000001";

fn gateway_cfg(realm: &TestRealm) -> GatewayTokenConfig {
    GatewayTokenConfig {
        audience: "shiki-gateway".into(),
        issuer: realm.issuer(),
    }
}

/// 前の版の `client_representation` が登録していた形（BUG-172 の前・B1）。
fn previous_b1_representation(client_id: &str) -> serde_json::Value {
    let capability: Vec<&str> = CapabilityScope::ALL.iter().map(|s| s.as_str()).collect();
    serde_json::json!({
        "clientId": client_id,
        "name": "前の版",
        "enabled": true,
        "protocol": "openid-connect",
        "publicClient": true,
        "standardFlowEnabled": true,
        "serviceAccountsEnabled": false,
        "directAccessGrantsEnabled": false,
        "redirectUris": [BFF_CALLBACK],
        "webOrigins": ["+"],
        "optionalClientScopes": capability,
        "attributes": { "pkce.code.challenge.method": "S256", "access.token.lifespan": "300" },
        "protocolMappers": [{
            "name": "shiki-gateway-audience",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-audience-mapper",
            "config": {
                "included.custom.audience": "shiki-gateway",
                "access.token.claim": "true",
                "id.token.claim": "false",
            }
        }],
    })
}

fn names(list: &serde_json::Value) -> Vec<String> {
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

/// 今の形のクライアントか（有効・シェルの戻り先・既定のスコープは basic だけ・マッパーは aud・tenant）。
fn assert_current_shape(client: &serde_json::Value) {
    assert_eq!(client["enabled"], true, "{client}");
    assert_eq!(client["redirectUris"], serde_json::json!([SHELL_CALLBACK]));
    assert_eq!(names(&client["defaultClientScopes"]), ["basic"]);
    assert_eq!(
        names(&client["optionalClientScopes"]).len(),
        CapabilityScope::ALL.len()
    );
    assert_eq!(
        names(&client["protocolMappers"]),
        ["shiki-gateway-audience", "tenant"]
    );
}

#[tokio::test]
async fn shell_callback_tokens_carry_sub_tenant_and_reach_the_gateway() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let client_id = format!("app-{}-b1", Uuid::new_v4());
    let oauth = realm.oauth_client();
    let result = async {
        oauth
            .register(
                ClientKind::PublicPkce,
                &client_id,
                "経費",
                &[SHELL_CALLBACK.to_string()],
                None,
            )
            .await
            .expect("register");
        assert_current_shape(&realm.client(&client_id).await.expect("client"));

        // シェルのポップアップと同じ要求（戻り先・PKCE・能力スコープ）でトークンが出る。
        let token = realm
            .user_token(&client_id, SHELL_CALLBACK, "alice", "openid llm.invoke")
            .await
            .expect("token");
        // BFF のログインの戻り先は、アプリのクライアントには登録しない。
        let bff = realm
            .user_token(&client_id, BFF_CALLBACK, "alice", "openid")
            .await;
        assert!(bff.is_err(), "{bff:?}");

        // ゲートウェイの検証を本物の鍵で通す: sub・tenant・能力スコープが取れる。
        let identity = verify_gateway_token(&token, &realm.key_resolver(), &gateway_cfg(&realm))
            .await
            .expect("verify");
        assert_eq!(identity.user_sub, ALICE);
        assert_eq!(identity.client_id, client_id);
        assert_eq!(identity.tenant.as_deref(), Some("a-corp"));
        assert_eq!(identity.token_scopes, [CapabilityScope::LlmInvoke]);
        assert_eq!(
            authz::TenantSource::Claim
                .resolve(identity.tenant.as_deref())
                .unwrap(),
            "a-corp"
        );
        // アプリに渡すトークンに、名前・メール・ロール・所属グループは載らない（所属グループは D-130）。
        let raw = claims(&token);
        for absent in ["roles", "email", "name", "preferred_username", "groups"] {
            assert!(raw.get(absent).is_none(), "{absent}: {raw}");
        }

        // テナントは利用者に束縛された値で、アプリのクライアントでは選べない（別のテナントの人は自分のテナント）。
        let charlie = realm
            .user_token(&client_id, SHELL_CALLBACK, "charlie", "openid llm.invoke")
            .await
            .expect("charlie token");
        assert_eq!(claims(&charlie)["tenant"], "b-corp");

        // ゲートウェイ（multi テナント）まで通す。DB が無ければここまで。
        let Some(pool) = setup().await else { return };
        let store = AppInstallationStore::new(pool.clone());
        let app_id = Uuid::new_v4();
        store
            .upsert(
                &ctx("a-corp"),
                NewAppInstallation {
                    app_id,
                    app_name: "経費",
                    installed_version: "1.0.0",
                    granted_scopes: &["llm.invoke".to_string()],
                    client_id_b1: Some(&client_id),
                    client_id_b2: None,
                    ai: AiPin::default(),
                    frontend_bundle: None,
                    server_bundle: None,
                    server_spec: None,
                },
            )
            .await
            .expect("install");
        let mut gw = state(pool.clone(), "a-corp");
        gw.keys = Arc::new(realm.key_resolver());
        gw.token_cfg = gateway_cfg(&realm);
        gw.tenant_source = authz::TenantSource::Claim;
        let app = build_gateway_router(gw);
        let (s, body) = get(&app, "/gw/whoami", Some(&token)).await;
        let (other, _) = get(&app, "/gw/whoami", Some(&charlie)).await;
        sqlx::query("DELETE FROM app_installation WHERE tenant_id = 'a-corp' AND app_id = $1")
            .bind(app_id)
            .execute(&pool)
            .await
            .expect("cleanup installation");
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(body["user_sub"], ALICE);
        assert_eq!(body["app_id"], app_id.to_string());
        // 別のテナント（b-corp）にはこのアプリのインストールが無いので断る。
        assert_eq!(other, StatusCode::FORBIDDEN);
    };
    run_then(result, realm.delete_client(&client_id)).await;
}

#[tokio::test]
async fn registering_again_enables_a_disabled_client() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let client_id = format!("app-{}-b1", Uuid::new_v4());
    let oauth = realm.oauth_client();
    let redirects = [SHELL_CALLBACK.to_string()];
    let result = async {
        oauth
            .register(ClientKind::PublicPkce, &client_id, "経費", &redirects, None)
            .await
            .expect("register");
        // アンインストールはクライアントを無効にする。無効のクライアントではトークンが出ない。
        oauth.set_enabled(&client_id, false).await.expect("disable");
        assert_eq!(realm.client(&client_id).await.unwrap()["enabled"], false);
        let refused = realm
            .user_token(&client_id, SHELL_CALLBACK, "alice", "openid")
            .await;
        assert!(refused.is_err(), "{refused:?}");

        // 再インストール（同じ client_id の登録）で有効に戻る。
        oauth
            .register(ClientKind::PublicPkce, &client_id, "経費", &redirects, None)
            .await
            .expect("register again");
        assert_current_shape(&realm.client(&client_id).await.unwrap());
        realm
            .user_token(&client_id, SHELL_CALLBACK, "alice", "openid llm.invoke")
            .await
            .expect("token after reinstall");
    };
    run_then(result, realm.delete_client(&client_id)).await;
}

#[tokio::test]
async fn previous_clients_are_repaired_in_place() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let client_id = format!("app-{}-b1", Uuid::new_v4());
    let missing = format!("app-{}-b1", Uuid::new_v4());
    let oauth = realm.oauth_client();
    let redirects = [SHELL_CALLBACK.to_string()];
    let result = async {
        // 前の版の形（BFF の戻り先・既定のスコープ無し・aud のマッパーだけ）。E2E が手で足したロールのマッパーと、7d の
        // 版が付けた所属グループのマッパー（D-130 の前）も模す。
        let mut previous = previous_b1_representation(&client_id);
        let mappers = previous["protocolMappers"].as_array_mut().unwrap();
        mappers.push(serde_json::json!({
            "name": "roles",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-usermodel-attribute-mapper",
            "config": { "user.attribute": "roles", "claim.name": "roles", "multivalued": "true",
                        "access.token.claim": "true" },
        }));
        mappers.push(serde_json::json!({
            "name": "groups",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-group-membership-mapper",
            "config": { "full.path": "true", "claim.name": "groups", "access.token.claim": "true",
                        "id.token.claim": "false", "userinfo.token.claim": "false" },
        }));
        realm.create_client_raw(&previous).await;
        let before = realm.client(&client_id).await.unwrap();
        assert_eq!(names(&before["defaultClientScopes"]), Vec::<String>::new());
        let refused = realm
            .user_token(&client_id, SHELL_CALLBACK, "alice", "openid")
            .await;
        assert!(
            refused.is_err(),
            "前の版はシェルの戻り先を断る: {refused:?}"
        );

        // 起動時の照合と同じ直し方。既にある内部の id は変えない。
        assert!(oauth
            .repair(ClientKind::PublicPkce, &client_id, "経費", &redirects, None)
            .await
            .expect("repair")
            .is_some());
        let after = realm.client(&client_id).await.unwrap();
        assert_eq!(after["id"], before["id"]);
        assert_current_shape(&after);
        let token = realm
            .user_token(&client_id, SHELL_CALLBACK, "alice", "openid llm.invoke")
            .await
            .expect("token after repair");
        let identity = verify_gateway_token(&token, &realm.key_resolver(), &gateway_cfg(&realm))
            .await
            .expect("verify");
        assert_eq!(identity.user_sub, ALICE);
        assert_eq!(identity.tenant.as_deref(), Some("a-corp"));
        // 照合で前の版のマッパーが外れ、トークンにロールも所属グループも載らない。
        assert!(claims(&token).get("roles").is_none());
        assert!(claims(&token).get("groups").is_none(), "{}", claims(&token));

        // 無いクライアントは作らない（confidential の secret を保管し直す経路が照合に無いため）。
        assert!(oauth
            .repair(ClientKind::PublicPkce, &missing, "経費", &redirects, None)
            .await
            .expect("repair missing")
            .is_none());
        assert!(realm.client(&missing).await.is_none());
    };
    run_then(result, async {
        realm.delete_client(&client_id).await;
        realm.delete_client(&missing).await;
    })
    .await;
}

#[tokio::test]
async fn confidential_clients_get_the_same_scopes_and_rotate_a_disabled_secret() {
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let client_id = format!("app-{}-b2", Uuid::new_v4());
    let oauth = realm.oauth_client();
    let result = async {
        let first = oauth
            .register(ClientKind::Confidential, &client_id, "経費", &[], None)
            .await
            .expect("register");
        let client = realm.client(&client_id).await.unwrap();
        assert_eq!(
            names(&client["defaultClientScopes"]),
            ["basic", "service_account"]
        );
        assert_eq!(
            names(&client["protocolMappers"]),
            ["shiki-gateway-audience", "tenant"]
        );
        // 有効なクライアントの再インストール（版の更新）は secret を作り直さない（保管済みの secret のまま使える）。
        let kept = oauth
            .register(ClientKind::Confidential, &client_id, "経費", &[], None)
            .await
            .expect("register again");
        assert_eq!(kept.client_secret, first.client_secret);
        // 無効にされたクライアント（管理者が漏れた secret を止めた・アンインストール）の再インストールは、secret を
        // 作り直してから有効に戻す（同じ secret のまま戻さない・BUG-201）。
        oauth.set_enabled(&client_id, false).await.expect("disable");
        let rotated = oauth
            .register(ClientKind::Confidential, &client_id, "経費", &[], None)
            .await
            .expect("register after disable");
        assert!(rotated.client_secret.is_some());
        assert_ne!(rotated.client_secret, first.client_secret);
        let again = realm.client(&client_id).await.unwrap();
        assert_eq!(again["enabled"], true);
        assert_eq!(
            names(&again["defaultClientScopes"]),
            ["basic", "service_account"]
        );
        let repaired = oauth
            .repair(ClientKind::Confidential, &client_id, "経費", &[], None)
            .await
            .expect("repair")
            .expect("client");
        // 登録と照合は、サービスアカウントの利用者（自動実行のトークンの sub）を返す（BUG-183）。
        assert!(first.service_account_sub.is_some());
        assert_eq!(repaired.service_account_sub, first.service_account_sub);
        let service_token = |secret: String| {
            let client_id = client_id.clone();
            let endpoint = realm.token_endpoint();
            async move {
                reqwest::Client::new()
                    .post(endpoint)
                    .form(&[
                        ("grant_type", "client_credentials"),
                        ("client_id", client_id.as_str()),
                        ("client_secret", secret.as_str()),
                    ])
                    .send()
                    .await
                    .expect("service token")
            }
        };
        let stopped = service_token(first.client_secret.clone().unwrap()).await;
        assert!(!stopped.status().is_success(), "止めた secret では取れない");
        let service = service_token(rotated.client_secret.clone().unwrap()).await;
        assert!(service.status().is_success(), "{}", service.status());
        let body: serde_json::Value = service.json().await.unwrap();
        let token = body["access_token"].as_str().unwrap();
        // サービスアカウントのトークンにも sub が載る（basic）。登録が返したサービスアカウントの利用者と同じ。
        assert_eq!(
            claims(token)["sub"].as_str(),
            first.service_account_sub.as_deref(),
            "{}",
            claims(token)
        );
    };
    run_then(result, realm.delete_client(&client_id)).await;
}
