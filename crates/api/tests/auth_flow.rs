//! OIDC code フロー（login → callback）と JWKS 検証の統合テスト（外部依存なし）。
//!
//! モックの OIDC token エンドポイントと JWKS エンドポイントを in-process で立て、
//! `build_router` 経由で BFF callback の token 交換・access token 検証・セッション発行を
//! 一気通貫で検証する。これにより login.rs / callback.rs / oidc.rs / middleware/jwks.rs /
//! server.rs（CORS・observe）の実経路をカバーする。
//!
//! 署名検証のため、テスト専用の RSA 鍵ペアを固定値で持つ（公開鍵を JWKS として配り、
//! 秘密鍵でトークンを RS256 署名する）。本番鍵とは無関係。

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use api::{build_router, config::*, session::MemorySessionStore, state::AppState};
use async_trait::async_trait;
use authz::{AuthzClient, AuthzError, FgaObject, Relation, Subject};
use axum::{
    body::Body,
    http::{header::COOKIE, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use jsonwebtoken::{EncodingKey, Header};
use tower::ServiceExt;

const KID: &str = "test-key-1";
const ISSUER: &str = "http://idp.test/realms/shiki";
const AUDIENCE: &str = "shiki-api";

/// テスト専用 RSA 秘密鍵（PKCS#8 PEM・2048bit）。公開鍵成分は [`JWK_N`]/`e=AQAB`。
const TEST_RSA_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDZHuilPKTVyZGZ
5AvC8vIL1M3dZdeaufFSwrqAcybhfnNTjGSo2Wn9T2O5Q+FScS42Gqj9MU6rHscy
VFfOxFMTCJ8WHANaOKCfjaHlQ8Dt5yT5Z5KtacEcYK2tjgfYTiA1kzdOxV7mAaWo
RoeuHbwC48F8fQH99ZkPGL5Yq06BWXuCJStzcWBFbo4GYJb6LFmOoYsemCZI15TE
uG3P4Vgs40fVcfbhB2eczxKuV1nzpRpp40/JYW0l4fE/t9VKKklXMPyxHqAOTqDB
NckcgttixNro0fXrt5BpFu75lyaNeL59p6TurOxJyBw8ooXADmpHolt0VrJ06jvP
65aIo9nrAgMBAAECggEABVysjSwevALi3CSUH8flL2KdhOq3dORDr/IMLhDp9Gat
jXIpqDvaUL2trk0dWu5doEtvQfV+Rl6Xt8f6dSpHDPDJEQA3GvrMCLS0O1e9g4KS
WLB0oGC8uqkukNsxmWdwhzIwCvt32QFQaIP5ZNTqKD4csKjbrDxs/8DyLnlokmwq
KfpHYkoZ9BtBWjAkpIhRA5FvqGTa6vXZou95d+4Vjx6V4TmvvfXuiJHcD977PG00
lkdbrw8PPnodbao5GzjUz//uku8hymYWjfckHZmK8SgnsmxM0ZzOWmp9qrz1DKEp
4W/9iuLKEQVILeZzJUHYt4tTqczt7s2jkbhLWGsCjQKBgQDt/cPya+AJ996gYsPs
bKliFtFb5Q/UzRv1JwTjIq/8U15dOXCxJL4Lim6K7LiMZbuq9T2zsuujqrOaLDCC
qQj/MKI4aftUmjgGYVW8CovghOVCgvKb6f6FeOGMrn2B0lGyying6dhVzrL7PLDq
+iWUuumN1vgzfhD9Cts5MdRcbwKBgQDpjNnZpD0SkqZlJXihRaEXR1d20vTafR0z
JP/LnvL2RYb5CGu6DA5xG7ngS7JmNDzylQBn54YcHqxWovdD2V6nk9C2YUhZrxr1
yfoi8FQOm0ScVwE9tTox5HMOviZo7bvN1xmlgutfOuOW93YQ0KqZEQZusSA3/NkH
MehCBWAQRQKBgHujbSvA7ThghFDwXnayEOE7l3JVMv9Lu22F4t0ZRTIiIZDu6WOu
Aek+9qTHzCxsIa30ECUOG6sAYKQEtwL6TAk/O9dw/7f5EogGAyNYm0h94hjGrMFh
M/AlV4/diqhqGjV3H4CQG+qgIo2w/vxkDigRXopolrMxmCPNgwxYncmTAoGBALMb
xeZXQk8AEIP5XK2xjH0hxT3nQshcswwKD/HEkGe1onFRt+wSWvD7Zm1RIBupbCRN
iOYmdH8UNu6qRB7QkPrLLYDw0l+VHoPoxeANlyksgk2zm8wLM/oXTPW9dg96YlDV
6WE5KfD6ZJfeZ7k1jd+dYuV5CVBmpLoT2B7pqGZRAoGATm1reSlO1CNa50Wpyki0
ZhyOnFyW40/6HAm9TPQTd5Ckee+tkRpTpOU3sdNNGkLMRSg1qUKy+TJgHXpvygGl
d8yx2gsYwCxeUAvpdbVQN+kwRY44bZiXhgMD8sK0UNW6p9197txgymvcwVwzCgBb
dQGoas84lLLCX8s898HkuXc=
-----END PRIVATE KEY-----";

/// [`TEST_RSA_PEM`] の公開鍵 modulus（base64url・パディング無し）。
const JWK_N: &str = "2R7opTyk1cmRmeQLwvLyC9TN3WXXmrnxUsK6gHMm4X5zU4xkqNlp_U9juUPhUnEuNhqo_TFOqx7HMlRXzsRTEwifFhwDWjign42h5UPA7eck-WeSrWnBHGCtrY4H2E4gNZM3TsVe5gGlqEaHrh28AuPBfH0B_fWZDxi-WKtOgVl7giUrc3FgRW6OBmCW-ixZjqGLHpgmSNeUxLhtz-FYLONH1XH24QdnnM8SrldZ86UaaeNPyWFtJeHxP7fVSipJVzD8sR6gDk6gwTXJHILbYsTa6NH167eQaRbu-ZcmjXi-faek7qzsScgcPKKFwA5qR6JbdFaydOo7z-uWiKPZ6w";

struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// 指定 kid で RS256 署名した access token を作る。`kid` を変えると未知 kid 経路を試せる。
fn sign_token(kid: &str, claims: serde_json::Value) -> String {
    let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let key = EncodingKey::from_rsa_pem(TEST_RSA_PEM.as_bytes()).expect("テスト鍵の読み込み");
    jsonwebtoken::encode(&header, &claims, &key).expect("署名")
}

/// iss/aud/exp/sub を満たす有効な access token。
fn valid_access_token() -> String {
    sign_token(
        KID,
        serde_json::json!({
            "sub": "00000000-0000-0000-0000-000000000001",
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": now() + 3600,
            "groups": ["/acme"],
            "email": "alice@acme.example",
        }),
    )
}

fn jwks_body(kid: &str) -> serde_json::Value {
    serde_json::json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": kid,
            "n": JWK_N,
            "e": "AQAB",
        }]
    })
}

/// token / JWKS を返す in-process モック IdP を立て、その base URL を返す。
/// `token_status` で token エンドポイントの応答ステータスを差し替えできる。
async fn spawn_idp(
    token_status: StatusCode,
    token_body: serde_json::Value,
    jwks_kid: &str,
) -> String {
    use axum::{routing::get, routing::post, Json, Router};
    let jwks = jwks_body(jwks_kid);
    let app = Router::new()
        .route(
            "/realms/shiki/protocol/openid-connect/token",
            post(move || {
                let body = token_body.clone();
                async move { (token_status, Json(body)) }
            }),
        )
        .route(
            "/realms/shiki/protocol/openid-connect/certs",
            get(move || {
                let jwks = jwks.clone();
                async move { Json(jwks) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/realms/shiki")
}

fn config_with(idp_base: &str, cors: Vec<String>) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 0,
            cors_allowed_origins: cors,
        },
        database: DatabaseConfig {
            url: "postgres://localhost/none".into(),
            max_connections: 1,
        },
        auth: AuthConfig {
            // ブラウザ向け authorize/end-session は公開 issuer 由来。
            issuer: ISSUER.into(),
            // token 交換・JWKS はモック IdP（内部 base）へ向ける。
            internal_base_url: Some(idp_base.to_string()),
            jwks_uri: Some(format!("{idp_base}/protocol/openid-connect/certs")),
            audience: AUDIENCE.into(),
            jwks_ttl_secs: 300,
            client_id: "shiki-web".into(),
            client_secret: Some("secret".into()),
            redirect_uri: "http://localhost:3000/auth/callback".into(),
            post_logout_redirect_uri: "http://localhost:3000/".into(),
            scopes: "openid profile".into(),
            tenancy: Tenancy::Single,
            tenant_id: Some("default".into()),
        },
        authz: AuthzConfig {
            base_url: "http://localhost:8080".into(),
            store_name: "shiki".into(),
        },
        session: SessionConfig {
            redis_url: "redis://localhost:6379".into(),
            ttl_secs: 86400,
            secure: false,
            refresh_leeway_secs: 60,
        },
        telemetry: TelemetryConfig {
            otlp_endpoint: None,
            service_name: "test".into(),
            log_format: LogFormat::Json,
        },
        storage: StorageConfig {
            backend: ObjectStoreBackend::Minio,
        },
        vector: VectorConfig {
            backend: VectorStoreBackend::Qdrant,
        },
        llm: LlmConfig {
            backend: LlmBackend::Vllm,
        },
    }
}

fn state_with(config: AppConfig) -> AppState {
    let store = Arc::new(MemorySessionStore::new());
    let db = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&config.database.url)
        .unwrap();
    let jwks = Arc::new(api::middleware::JwksCache::new(
        reqwest::Client::new(),
        config.auth.effective_jwks_uri(),
        Duration::from_secs(config.auth.jwks_ttl_secs),
    ));
    AppState {
        config: Arc::new(config),
        db,
        authz: Arc::new(AllowAll),
        jwks,
        sessions: store,
        http: reqwest::Client::new(),
    }
}

/// Set-Cookie 群から指定名の Cookie 値を取り出す。
fn cookie_value(resp_cookies: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    resp_cookies.iter().find_map(|c| {
        c.strip_prefix(&prefix)
            .map(|rest| rest.split(';').next().unwrap_or("").to_string())
    })
}

fn set_cookies(resp: &axum::http::Response<Body>) -> Vec<String> {
    resp.headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

/// login → flow cookie 取り出し。state と flow cookie 文字列（"name=value"）を返す。
async fn do_login(app: axum::Router) -> (String, String) {
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "login は 303/302 リダイレクト"
    );
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.starts_with(&format!("{ISSUER}/protocol/openid-connect/auth")),
        "authorize へ。location={location}"
    );
    let cookies = set_cookies(&resp);
    let flow_val = cookie_value(&cookies, "shiki_oidc_flow").expect("flow cookie が発行される");
    // location の state クエリを取り出す。
    let url = reqwest::Url::parse(&location).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .unwrap();
    (state, format!("shiki_oidc_flow={flow_val}"))
}

#[tokio::test]
async fn login_redirects_to_authorize_with_pkce() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    // PKCE S256 と code フローのパラメータが付与される。
    assert!(location.contains("response_type=code"));
    assert!(location.contains("code_challenge="));
    assert!(location.contains("code_challenge_method=S256"));
    assert!(set_cookies(&resp)
        .iter()
        .any(|c| c.starts_with("shiki_oidc_flow=")));
}

#[tokio::test]
async fn callback_exchanges_code_and_issues_session() {
    let token_body = serde_json::json!({
        "access_token": valid_access_token(),
        "refresh_token": "rt",
        "expires_in": 3600,
        "id_token": "id.tok.sig",
    });
    let idp = spawn_idp(StatusCode::OK, token_body, KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));

    let (state, flow_cookie) = do_login(app.clone()).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback?code=abc&state={state}"))
                .header(COOKIE, flow_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "callback 成功はトップへリダイレクト"
    );
    assert_eq!(
        resp.headers().get(axum::http::header::LOCATION).unwrap(),
        "/"
    );
    let cookies = set_cookies(&resp);
    assert!(
        cookie_value(&cookies, "shiki_session").is_some(),
        "セッション Cookie が発行される"
    );
    assert!(
        cookie_value(&cookies, "shiki_csrf").is_some(),
        "CSRF Cookie が発行される"
    );
    // 相関 Cookie は破棄される（Max-Age 0）。
    assert!(cookies.iter().any(|c| c.starts_with("shiki_oidc_flow=")
        && (c.contains("Max-Age=0") || c.contains("max-age=0"))));
}

#[tokio::test]
async fn callback_with_idp_error_is_unauthorized() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/callback?error=access_denied")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_without_code_is_unauthorized() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/callback?state=x")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_with_state_mismatch_is_unauthorized() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let (_state, flow_cookie) = do_login(app.clone()).await;
    // flow cookie はあるが state クエリが一致しない → CSRF/リプレイ拒否。
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/callback?code=abc&state=wrong-state")
                .header(COOKIE, flow_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_without_flow_cookie_is_unauthorized() {
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/callback?code=abc&state=s")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_with_token_endpoint_4xx_is_unauthorized() {
    // token エンドポイントが invalid_grant 等 4xx → ApiError::Unauthorized。
    let idp = spawn_idp(
        StatusCode::BAD_REQUEST,
        serde_json::json!({ "error": "invalid_grant" }),
        KID,
    )
    .await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let (state, flow_cookie) = do_login(app.clone()).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback?code=abc&state={state}"))
                .header(COOKIE, flow_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_with_unknown_kid_token_is_unauthorized() {
    // JWKS に存在しない kid で署名されたトークン → 検証失敗で 401。
    let token_body = serde_json::json!({
        "access_token": sign_token("unknown-kid", serde_json::json!({
            "sub": "u", "iss": ISSUER, "aud": AUDIENCE, "exp": now() + 3600,
        })),
        "expires_in": 3600,
    });
    let idp = spawn_idp(StatusCode::OK, token_body, KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let (state, flow_cookie) = do_login(app.clone()).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback?code=abc&state={state}"))
                .header(COOKIE, flow_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callback_with_wrong_audience_is_unauthorized() {
    // aud がサーバの期待値と異なるトークン → audience 境界で拒否。
    let token_body = serde_json::json!({
        "access_token": sign_token(KID, serde_json::json!({
            "sub": "u", "iss": ISSUER, "aud": "other-client", "exp": now() + 3600,
        })),
        "expires_in": 3600,
    });
    let idp = spawn_idp(StatusCode::OK, token_body, KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));
    let (state, flow_cookie) = do_login(app.clone()).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback?code=abc&state={state}"))
                .header(COOKIE, flow_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_status_true_after_login_callback() {
    // callback で確立したセッション Cookie で /auth/session が authenticated=true を返す。
    let token_body = serde_json::json!({
        "access_token": valid_access_token(),
        "refresh_token": "rt",
        "expires_in": 3600,
    });
    let idp = spawn_idp(StatusCode::OK, token_body, KID).await;
    let app = build_router(state_with(config_with(&idp, vec![])));

    let (state, flow_cookie) = do_login(app.clone()).await;
    let cb = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback?code=abc&state={state}"))
                .header(COOKIE, flow_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let session_cookie =
        cookie_value(&set_cookies(&cb), "shiki_session").expect("セッション Cookie");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/auth/session")
                .header(COOKIE, format!("shiki_session={session_cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json.get("authenticated"),
        Some(&serde_json::Value::Bool(true))
    );
}

#[tokio::test]
async fn cors_preflight_is_allowed_for_configured_origin() {
    // cors_allowed_origins 設定時、許可オリジンの preflight に CORS ヘッダを返す（cors_layer Some 経路）。
    let idp = spawn_idp(StatusCode::OK, serde_json::json!({}), KID).await;
    let app = build_router(state_with(config_with(
        &idp,
        vec!["http://localhost:3000".into()],
    )));
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/healthz")
                .header(axum::http::header::ORIGIN, "http://localhost:3000")
                .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:3000")
    );
}
