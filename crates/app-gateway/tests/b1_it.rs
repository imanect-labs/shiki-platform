//! B1 フロントバンドル配信の結合テスト（Task 9.11 受け入れ条件）。
//!
//! - 同意時ピン突合: 未インストール／ピン外 sha は 404（publish 済みでも配信しない）
//! - 配信: 200 ＋ CSP（sandbox・connect-src=gateway・frame-ancestors=host）＋ immutable
//! - 改竄: オブジェクトストア内容が sha と不一致なら配信拒否（500）
//! - 入力: sha が hex64 でなければ 404（キー注入防止）

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use app_gateway::{build_b1_router, AiPin, AppInstallationStore, B1State, NewAppInstallation};
use axum::{body::Body, http::Request, http::StatusCode};
use common::{ctx, setup, MemStore};
use http_body_util::BodyExt;
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use tower::ServiceExt;
use uuid::Uuid;

const BUNDLE: &[u8] = b"<!doctype html><html><body>hello miniapp</body></html>";

async fn get(app: &axum::Router, path: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

#[tokio::test]
async fn serves_pinned_bundle_with_isolation_headers() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let sha = sha256_hex(BUNDLE);

    let store = Arc::new(MemStore::default());
    store
        .0
        .lock()
        .unwrap()
        .insert(miniapp_bundle_key(&tenant, &sha), BUNDLE.to_vec());

    let installations = AppInstallationStore::new(pool.clone());
    let app = build_b1_router(B1State {
        installations: installations.clone(),
        store: store.clone(),
        gateway_origin: "http://gw.example:8090".into(),
        host_origin: "http://host.example:3000".into(),
    });

    // 未インストール → 404（publish 済みでも配信しない）。
    let (s, _, _) = get(&app, &format!("/a/{app_id}/{sha}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // インストール（frontend ピン付き）。
    installations
        .upsert(
            &ctx(&tenant),
            NewAppInstallation {
                app_id,
                app_name: "b1-app",
                installed_version: "1.0.0",
                granted_scopes: &["data.read".to_string()],
                client_id_b1: Some("app-b1"),
                client_id_b2: None,
                ai: AiPin::default(),
                frontend_bundle: Some(&sha),
                server_bundle: None,
                server_spec: None,
            },
        )
        .await
        .expect("install");

    // ピン一致 → 200 ＋ 隔離ヘッダ。
    let (s, headers, body) = get(&app, &format!("/a/{app_id}/{sha}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, BUNDLE);
    let csp = headers
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(csp.contains("sandbox allow-scripts allow-forms"), "{csp}");
    assert!(csp.contains("connect-src http://gw.example:8090"), "{csp}");
    assert!(
        csp.contains("frame-ancestors http://host.example:3000"),
        "{csp}"
    );
    assert!(!csp.contains("allow-same-origin"), "{csp}");
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
    // cookie は一切発行しない（別オリジン・無状態配信）。
    assert!(headers.get("set-cookie").is_none());

    // ピン外 sha（publish 済み別バージョン相当）→ 404。
    let other = sha256_hex(b"other bundle");
    store.0.lock().unwrap().insert(
        miniapp_bundle_key(&tenant, &other),
        b"other bundle".to_vec(),
    );
    let (s, _, _) = get(&app, &format!("/a/{app_id}/{other}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // sha 形式不正 → 404（オブジェクトキー注入防止）。
    let (s, _, _) = get(&app, &format!("/a/{app_id}/not-a-sha")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // オブジェクトストア改竄（内容差し替え）→ 配信拒否（500・sha 再検証）。
    store
        .0
        .lock()
        .unwrap()
        .insert(miniapp_bundle_key(&tenant, &sha), b"tampered".to_vec());
    let (s, _, _) = get(&app, &format!("/a/{app_id}/{sha}")).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
}
