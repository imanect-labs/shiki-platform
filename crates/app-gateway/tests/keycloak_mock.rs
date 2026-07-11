//! OAuth クライアント登録／token-exchange のモック Keycloak 結合テスト（Task 9.7）。
//!
//! 実 Keycloak を使わず、admin token・client 登録・secret 取得・token-exchange の各
//! エンドポイントをローカル axum サーバで模し、live 呼び出し経路（admin_token/register/
//! fetch_secret/exchange_for_user）を検証する（DB 不要・CI で常時実行）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use app_gateway::{exchange_for_user, ClientKind, OAuthClient};
use axum::{
    extract::Query,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::collections::HashMap;

/// モック Keycloak（token / clients / client-secret / token-exchange）を起動し base URL を返す。
async fn spawn_mock_keycloak() -> (String, String) {
    let app = Router::new()
        // token エンドポイント: client_credentials も token-exchange も同一パスで受ける。
        .route(
            "/realms/shiki/protocol/openid-connect/token",
            post(|body: String| async move {
                // grant_type で分岐（token-exchange は subject_token を返し込む）。
                if body.contains("token-exchange") {
                    Json(json!({ "access_token": "exchanged-user-token", "expires_in": 300 }))
                } else {
                    Json(json!({ "access_token": "admin-token", "expires_in": 60 }))
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
        )
        .await
        .expect("register b1");
    assert_eq!(reg.client_id, "app-b1");
    // public は secret を持たない。
    assert!(reg.client_secret.is_none());
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
        .register(ClientKind::Confidential, "app-b2", "経費", &[])
        .await
        .expect("register b2");
    assert_eq!(reg.client_id, "app-b2");
    // confidential は生成 secret を取得して返す。
    assert_eq!(reg.client_secret.as_deref(), Some("generated-secret"));
}

#[tokio::test]
async fn token_exchange_returns_user_token() {
    let (_admin_base, token_ep) = spawn_mock_keycloak().await;
    let out = exchange_for_user(
        &reqwest::Client::new(),
        &token_ep,
        "app-b2",
        "b2-secret",
        "user-access-token",
        "shiki-gateway",
    )
    .await
    .expect("exchange");
    // sub=ユーザー維持のトークン（モックは固定文字列を返す）。
    assert_eq!(out.access_token, "exchanged-user-token");
    assert_eq!(out.expires_in, Some(300));
}
