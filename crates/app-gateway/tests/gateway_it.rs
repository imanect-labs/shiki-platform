//! 公開 API ゲートウェイの二重ゲート結合テスト（Task 9.6 受け入れ条件）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）でインストール台帳を用意し、RSA 署名の
//! Bearer トークンで whoami/能力ルートを叩く。①未認証 401 ②未インストール/失効 403
//! ③スコープ未付与 403 ④広トークンでも granted に無ければ 403（同意失効の即時反映）を検証する。
//! スコープゲートの対象は実能力ルート `/gw/data/tables`（data.read・Task 9.8）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use app_gateway::{build_gateway_router, AiPin, AppInstallationStore, NewAppInstallation};
use axum::http::StatusCode;
use common::{ctx, get, setup, state, token, token_without_tenant};
use uuid::Uuid;

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
                ai: AiPin::default(),
                frontend_bundle: None,
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

    // ③ data.read の能力ルート → 200（granted かつ token scope 内）。
    let (s, body) = get(&app, "/gw/data/tables", Some(&tok)).await;
    assert_eq!(s, StatusCode::OK, "{body}");

    // ④ token に data.read が無ければ 403（scope マップ強制）。
    let narrow = token(&client_b1, "openid", &tenant);
    let (s, _) = get(&app, "/gw/data/tables", Some(&narrow)).await;
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
                ai: AiPin::default(),
                frontend_bundle: None,
            },
        )
        .await
        .expect("install");
    let app = build_gateway_router(state(pool.clone(), &tenant));

    let broad = token(&client, "data.read data.write identity.read", &tenant);
    // whoami（スコープ不要）は通る。
    let (s, _) = get(&app, "/gw/whoami", Some(&broad)).await;
    assert_eq!(s, StatusCode::OK);
    // data.read ルートは granted に無いので 403。
    let (s, _) = get(&app, "/gw/data/tables", Some(&broad)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // granted にある identity.read は通る。
    let (s, _) = get(&app, "/gw/identity/me", Some(&broad)).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn multi_tenant_rejects_missing_tenant_claim() {
    // require_tenant_claim=true（SaaS multi）では tenant クレーム欠落を fail-closed で 401。
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let client = format!("app-{}", Uuid::new_v4());
    AppInstallationStore::new(pool.clone())
        .upsert(
            &ctx(&tenant),
            NewAppInstallation {
                app_id: Uuid::new_v4(),
                app_name: "multi",
                installed_version: "1.0.0",
                granted_scopes: &["data.read".to_string()],
                client_id_b1: Some(&client),
                client_id_b2: None,
                ai: AiPin::default(),
                frontend_bundle: None,
            },
        )
        .await
        .expect("install");
    let mut st = state(pool.clone(), &tenant);
    st.require_tenant_claim = true;
    let app = build_gateway_router(st);

    // tenant クレーム無し → 401（既定テナントへフォールバックしない）。
    let no_tenant = token_without_tenant(&client, "data.read");
    let (s, _) = get(&app, "/gw/whoami", Some(&no_tenant)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // tenant クレームありなら通る。
    let ok = token(&client, "data.read", &tenant);
    let (s, _) = get(&app, "/gw/whoami", Some(&ok)).await;
    assert_eq!(s, StatusCode::OK);
}

/// CORS: B1 の opaque origin（Origin: null）からのプリフライトと能力応答にヘッダが付く。
#[tokio::test]
async fn cors_allows_opaque_origin_preflight_and_reflects_origin() {
    use axum::body::Body;
    use axum::http::{header, Request};
    use tower::ServiceExt;

    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app = build_gateway_router(state(pool.clone(), &tenant));

    // プリフライト（OPTIONS・Bearer なし）は dual_gate より外側で 204＋CORS を返す。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/gw/data/tables")
                .header(header::ORIGIN, "null")
                .header("access-control-request-method", "GET")
                .header("access-control-request-headers", "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let h = resp.headers();
    assert_eq!(
        h.get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("null")
    );
    assert!(h
        .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("authorization")));

    // 実リクエスト（未認証）でも応答に ACAO が付く（ブラウザが 401 本文を読めるように）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/gw/whoami")
                .header(header::ORIGIN, "null")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("null")
    );
}
