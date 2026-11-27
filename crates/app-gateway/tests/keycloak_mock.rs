//! OAuth クライアント登録／token-exchange のモック Keycloak 結合テスト（Task 9.7）。
//!
//! 実 Keycloak を使わず、admin token・client 登録・secret 取得・token-exchange の各
//! エンドポイントをローカル axum サーバで模し、live 呼び出し経路（admin_token/register/
//! fetch_secret/exchange_for_user）を検証する（DB 不要・CI で常時実行）。
//!
//! モックは何でも通すので、Keycloak 26.2 の交換の決まり（aud・audience・scope）はここでは確かめられない。
//! それは実 Keycloak の `keycloak_it.rs` が確かめる（BUG-182）。ここで見るのは、送る値と、返った値の扱い。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use app_gateway::{exchange_for_user, ClientKind, GatewayError, OAuthClient};
use axum::{
    extract::Query,
    routing::{get, post},
    Form, Json, Router,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 交換の要求に来た値（form）。
type Seen = Arc<Mutex<Vec<HashMap<String, String>>>>;

/// 署名を確かめない読み手向けの JWT（`scope` だけを持つ）。
fn jwt(scope: &str) -> String {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("k".into());
    encode(
        &header,
        &json!({ "sub": "user-1", "scope": scope, "exp": 9_999_999_999u64 }),
        &EncodingKey::from_secret(b"k"),
    )
    .unwrap()
}

/// モック Keycloak（token / clients / client-secret / token-exchange）を起動し base URL を返す。
async fn spawn_mock_keycloak() -> (String, String) {
    spawn_mock_keycloak_with(Seen::default()).await
}

/// 交換の要求を `seen` に残すモック。交換は、求めた scope のトークンを返す（client_id が `widen` なら data.write
/// を足し、`refuse` なら 403 で断る）。
async fn spawn_mock_keycloak_with(seen: Seen) -> (String, String) {
    let app = Router::new()
        // token エンドポイント: client_credentials も token-exchange も同一パスで受ける。
        .route(
            "/realms/shiki/protocol/openid-connect/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let seen = Arc::clone(&seen);
                async move {
                    let grant = form.get("grant_type").cloned().unwrap_or_default();
                    if !grant.contains("token-exchange") {
                        return (
                            axum::http::StatusCode::OK,
                            Json(json!({ "access_token": "admin-token", "expires_in": 60 })),
                        );
                    }
                    seen.lock().unwrap().push(form.clone());
                    let client = form.get("client_id").cloned().unwrap_or_default();
                    if client == "refuse" {
                        return (
                            axum::http::StatusCode::FORBIDDEN,
                            Json(json!({ "error": "access_denied",
                                         "error_description": "Client is not within the token audience" })),
                        );
                    }
                    let mut scope = form.get("scope").cloned().unwrap_or_default();
                    if client == "widen" {
                        scope.push_str(" data.write");
                    }
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({ "access_token": jwt(scope.trim()), "expires_in": 300 })),
                    )
                }
            }),
        )
        // client 登録（POST）と検索（GET ?clientId=）。
        .route(
            "/admin/realms/shiki/clients",
            post(|| async { (axum::http::StatusCode::CREATED, "") }).get(
                |Query(q): Query<HashMap<String, String>>| async move {
                    let cid = q.get("clientId").cloned().unwrap_or_default();
                    Json(json!([{ "id": format!("internal-{cid}") }]))
                },
            ),
        )
        // 能力スコープの client-scope 冪等作成（register が呼ぶ・201 を返す）。
        .route(
            "/admin/realms/shiki/client-scopes",
            post(|| async { (axum::http::StatusCode::CREATED, "") }),
        )
        // confidential client の secret 取得。
        .route(
            "/admin/realms/shiki/clients/{id}/client-secret",
            get(|| async { Json(json!({ "value": "generated-secret" })) }),
        )
        // confidential client のサービスアカウントの利用者（自動実行のトークンの sub）。
        .route(
            "/admin/realms/shiki/clients/{id}/service-account-user",
            get(|| async { Json(json!({ "id": "service-account-1", "username": "service-account-app-b2" })) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        format!("http://{addr}/admin/realms/shiki"),
        format!("http://{addr}/realms/shiki/protocol/openid-connect/token"),
    )
}

#[tokio::test]
async fn register_b1_public_has_no_secret() {
    let (admin_base, token_ep) = spawn_mock_keycloak().await;
    let oauth = OAuthClient::new(
        reqwest::Client::new(),
        admin_base,
        token_ep,
        "provisioner".into(),
        "prov-secret".into(),
    );
    let reg = oauth
        .register(
            ClientKind::PublicPkce,
            "app-b1",
            "経費",
            &["https://apps.example/cb".into()],
            Some("app-b2"),
        )
        .await
        .expect("register b1");
    assert_eq!(reg.client_id, "app-b1");
    // public は secret もサービスアカウントも持たない。
    assert!(reg.client_secret.is_none());
    assert!(reg.service_account_sub.is_none());
}

#[tokio::test]
async fn register_b2_confidential_fetches_secret() {
    let (admin_base, token_ep) = spawn_mock_keycloak().await;
    let oauth = OAuthClient::new(
        reqwest::Client::new(),
        admin_base,
        token_ep,
        "provisioner".into(),
        "prov-secret".into(),
    );
    let reg = oauth
        .register(ClientKind::Confidential, "app-b2", "経費", &[], None)
        .await
        .expect("register b2");
    assert_eq!(reg.client_id, "app-b2");
    // confidential は生成 secret と、サービスアカウントの利用者を取得して返す。
    assert_eq!(reg.client_secret.as_deref(), Some("generated-secret"));
    assert_eq!(
        reg.service_account_sub.as_deref(),
        Some("service-account-1")
    );
}

#[tokio::test]
async fn token_exchange_asks_only_for_the_subject_scopes() {
    let seen = Seen::default();
    let (_admin_base, token_ep) = spawn_mock_keycloak_with(Arc::clone(&seen)).await;
    let subject = jwt("openid data.read llm.invoke");
    let out = exchange_for_user(
        &reqwest::Client::new(),
        &token_ep,
        "app-b2",
        "b2-secret",
        &subject,
    )
    .await
    .expect("exchange");
    assert_eq!(out.expires_in, Some(300));
    let form = seen.lock().unwrap()[0].clone();
    assert_eq!(form["subject_token"], subject);
    assert_eq!(form["client_id"], "app-b2");
    assert_eq!(form["client_secret"], "b2-secret");
    // 元のトークンの能力スコープだけを求める（openid は能力スコープではない）。
    assert_eq!(form["scope"], "data.read llm.invoke");
    // ゲートウェイの aud はクライアントではないので audience は送らない（実 Keycloak は Audience not found で断る）。
    assert!(!form.contains_key("audience"), "{form:?}");
}

/// 返ったトークンの能力スコープが元より広ければ使わない。Keycloak が断ったときは、状態コードと Keycloak の文を
/// 返さない（BUG-182）。
#[tokio::test]
async fn token_exchange_rejects_wider_tokens_and_hides_the_reason() {
    let (_admin_base, token_ep) = spawn_mock_keycloak().await;
    let subject = jwt("data.read");
    for client in ["widen", "refuse"] {
        let err = exchange_for_user(
            &reqwest::Client::new(),
            &token_ep,
            client,
            "secret",
            &subject,
        )
        .await
        .expect_err(client);
        let GatewayError::Forbidden(message) = &err else {
            panic!("{client}: {err:?}");
        };
        assert!(
            !message.contains("403") && !message.contains("audience"),
            "{message}"
        );
    }
    // 元のトークンが JWT でなければ交換しない（送る前に断る）。
    assert!(exchange_for_user(
        &reqwest::Client::new(),
        &token_ep,
        "app-b2",
        "secret",
        "not-a-jwt"
    )
    .await
    .is_err());
}
