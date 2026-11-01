//! 公開 API ゲートウェイの二重ゲート結合テスト（Task 9.6 受け入れ条件）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）でインストール台帳を用意し、RSA 署名の
//! Bearer トークンで whoami/能力ルートを叩く。①未認証 401 ②未インストール/失効 403
//! ③スコープ未付与 403 ④広トークンでも granted に無ければ 403（同意失効の即時反映）を検証する。
//! スコープゲートの対象は実能力ルート `/gw/data/tables`（data.read・Task 9.8）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use app_gateway::{build_gateway_router, AiPin, AppInstallationStore, NewAppInstallation};
use axum::http::StatusCode;
use common::{
    ctx, get, request_json, setup, signed, state, state_with, token, token_without_tenant,
    StubAuthz, AUDIENCE, ISSUER,
};
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
                server_bundle: None,
                server_spec: None,
            },
        )
        .await
        .expect("install");

    let app = build_gateway_router(state(pool.clone(), &tenant));

    // ① トークン無し → 401。
    let (s, body) = get(&app, "/gw/whoami", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated", "{body}");
    // 壊れたトークンの 401 も同じ構造化した本文で、解析の内部文言を含まない（3 回目の検証 3-B-5）。
    let (s, broken) = get(&app, "/gw/whoami", Some("not-a-jwt")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(broken, body, "理由で本文を変えない");
    let text = broken.to_string();
    assert!(
        !text.contains("JWT") && !text.contains("JSON") && !text.contains("error:"),
        "{text}"
    );

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
                server_bundle: None,
                server_spec: None,
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
    // TenantSource::Claim（SaaS multi）では tenant クレーム欠落を fail-closed で 401。
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
                server_bundle: None,
                server_spec: None,
            },
        )
        .await
        .expect("install");
    let mut st = state(pool.clone(), &tenant);
    st.tenant_source = authz::TenantSource::Claim;
    let app = build_gateway_router(st);

    // tenant クレーム無し → 401（既定テナントへフォールバックしない）。
    let no_tenant = token_without_tenant(&client, "data.read");
    let (s, _) = get(&app, "/gw/whoami", Some(&no_tenant)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // FGA 名前空間区切りを含む tenant クレームは識別子ポリシーで 401（内部 API と同じ規則）。
    let bad = token(&client, "data.read", &format!("{tenant}|x"));
    let (s, _) = get(&app, "/gw/whoami", Some(&bad)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // tenant クレームありなら通る。
    let ok = token(&client, "data.read", &tenant);
    let (s, _) = get(&app, "/gw/whoami", Some(&ok)).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn single_tenant_ignores_tenant_claim() {
    // TenantSource::Fixed（オンプレ/cell single）は設定の固定値を使い、トークンの tenant
    // クレームでテナントを選ばせない（内部 API の resolve_tenant_id と同じ規則・#808）。
    let Some(pool) = setup().await else { return };
    let fixed = format!("t-{}", Uuid::new_v4());
    let other = format!("t-{}", Uuid::new_v4());
    let client_fixed = format!("app-{}", Uuid::new_v4());
    let client_other = format!("app-{}", Uuid::new_v4());
    let store = AppInstallationStore::new(pool.clone());
    for (tenant, client) in [(&fixed, &client_fixed), (&other, &client_other)] {
        store
            .upsert(
                &ctx(tenant),
                NewAppInstallation {
                    app_id: Uuid::new_v4(),
                    app_name: "single",
                    installed_version: "1.0.0",
                    granted_scopes: &["data.read".to_string()],
                    client_id_b1: Some(client),
                    client_id_b2: None,
                    ai: AiPin::default(),
                    frontend_bundle: None,
                    server_bundle: None,
                    server_spec: None,
                },
            )
            .await
            .expect("install");
    }
    let app = build_gateway_router(state(pool.clone(), &fixed));

    // 別テナントを名乗るクレームでも固定テナントへ解決する（固定テナントのインストールで通る）。
    let claims_other = token(&client_fixed, "data.read", &other);
    let (s, body) = get(&app, "/gw/whoami", Some(&claims_other)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    // クレーム無しでも固定テナントへ解決する（single の既存動作）。
    let no_tenant = token_without_tenant(&client_fixed, "data.read");
    let (s, body) = get(&app, "/gw/whoami", Some(&no_tenant)).await;
    assert_eq!(s, StatusCode::OK, "{body}");

    // 越境の負のテスト: 別テナントのインストールはクレームで名乗っても解決されない（403）。
    let cross = token(&client_other, "data.read", &other);
    let (s, _) = get(&app, "/gw/whoami", Some(&cross)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn single_tenant_empty_fixed_value_fails_closed() {
    // 固定値が空（設定ミス）ならクレームへフォールバックせず 401。
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let client = format!("app-{}", Uuid::new_v4());
    let mut st = state(pool.clone(), &tenant);
    st.tenant_source = authz::TenantSource::Fixed("  ".into());
    let app = build_gateway_router(st);
    let tok = token(&client, "data.read", &tenant);
    let (s, _) = get(&app, "/gw/whoami", Some(&tok)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
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

/// B2 関数起動ルート（Task 9.12）: 宣言済み関数は FunctionPort へ委譲し、宣言外は存在秘匿 404。
#[tokio::test]
async fn b2_function_invoke_delegates_and_hides_undeclared() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let client_b2 = format!("b2-{}", Uuid::new_v4());
    let installations = AppInstallationStore::new(pool.clone());
    installations
        .upsert(
            &ctx(&tenant),
            NewAppInstallation {
                app_id,
                app_name: "経費",
                installed_version: "1.0.0",
                granted_scopes: &[],
                client_id_b1: None,
                client_id_b2: Some(&client_b2),
                ai: AiPin::default(),
                frontend_bundle: None,
                server_bundle: Some("deadbeef"),
                server_spec: Some(serde_json::json!({ "functions": ["on_approved"] })),
            },
        )
        .await
        .expect("install");

    let app = build_gateway_router(state(pool.clone(), &tenant));
    let tok = token(&client_b2, "", &tenant);

    // 宣言済み関数 → 委譲され StubFunctions が echo（subject_token はハンドラが Bearer から抽出）。
    let (s, body) = request_json(
        &app,
        "POST",
        "/gw/apps/functions/on_approved/invoke",
        Some(&tok),
        &serde_json::json!({ "record": "r1" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["function_stub"], "on_approved");
    assert_eq!(body["has_subject_token"], true);
    assert_eq!(body["server_bundle"], "deadbeef");

    // 宣言外関数 → server_spec.functions に無いので 404（存在秘匿）。
    let (s, _) = request_json(
        &app,
        "POST",
        "/gw/apps/functions/unknown/invoke",
        Some(&tok),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// 組織はトークンではなくインストールから決め、呼ぶ人がその組織のメンバー（organization#member）かを確かめる
/// （D-130）。トークンに所属グループが載っていても読まない（前の版で登録したクライアントのトークン）。
#[tokio::test]
async fn org_comes_from_the_installation_and_requires_membership() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let client = format!("app-{}", Uuid::new_v4());
    // ctx は組織 acme でインストールする。
    AppInstallationStore::new(pool.clone())
        .upsert(
            &ctx(&tenant),
            NewAppInstallation {
                app_id,
                app_name: "経費",
                installed_version: "1.0.0",
                granted_scopes: &["data.read".to_string()],
                client_id_b1: Some(&client),
                client_id_b2: None,
                ai: AiPin::default(),
                frontend_bundle: None,
                server_bundle: None,
                server_spec: None,
            },
        )
        .await
        .expect("install");
    let body = async {
        // acme のメンバーは alice だけ。
        let authz = Arc::new(StubAuthz {
            org_members: Some(vec![(
                format!("user:{tenant}|alice"),
                format!("organization:{tenant}|acme"),
            )]),
            ..StubAuthz::default()
        });
        let app = build_gateway_router(state_with(pool.clone(), &tenant, authz));
        let with_groups = |sub: &str, group: &str| {
            signed(&serde_json::json!({
                "sub": sub, "azp": client, "tenant": tenant, "scope": "data.read",
                "groups": [format!("/tenants/{tenant}/{group}")],
                "aud": AUDIENCE, "iss": ISSUER, "exp": 9_999_999_999u64,
            }))
        };

        // alice はトークンが別の組織のグループを名乗っても、インストールの組織（acme）のメンバーとして通る。
        let (s, body) = get(&app, "/gw/whoami", Some(&with_groups("alice", "other"))).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let (s, body) = get(
            &app,
            "/gw/data/tables",
            Some(&token(&client, "data.read", &tenant)),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let orgs: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT org FROM audit_log \
             WHERE tenant_id = $1 AND action = 'gateway.call' AND decision = 'allow'",
        )
        .bind(&tenant)
        .fetch_all(&pool)
        .await
        .expect("audit");
        assert_eq!(orgs, ["acme"], "能力の呼び出しはインストールの組織で動く");

        // bob はメンバーでないので 403。トークンが acme のグループを名乗っても通らない。
        for tok in [token_as_bob(&client, &tenant), with_groups("bob", "acme")] {
            let (s, body) = get(&app, "/gw/whoami", Some(&tok)).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(
                body["error"],
                "権限がありません: このアプリを使えるのは、アプリをインストールした組織のメンバーだけです"
            );
        }
        let denied: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND org = 'acme' \
               AND action = 'gateway.call' AND decision = 'deny' AND actor = 'bob' \
               AND metadata->>'security' = 'true'",
        )
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .expect("audit deny");
        assert_eq!(denied, 2, "メンバーでない呼び出しは監査に deny で残る");
    };
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(body)).await;
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
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn token_as_bob(client: &str, tenant: &str) -> String {
    common::token_as(client, "data.read", tenant, "bob")
}
