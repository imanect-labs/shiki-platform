//! sub が使えずに断った人の Keycloak の SSO セッションを、サーバ側で終える手順（`api::oidc::end_sso_session`・D-205）
//! を実 Keycloak で確かめる結合テスト（`KEYCLOAK_TEST_URL`＝realm `shiki` を import 済みの Keycloak の base URL。
//! 無ければスキップ）。
//!
//! ブラウザと同じ認可コード＋PKCE で試しの利用者を入れ、SSO の Cookie があれば authorize が黙って code を返すことを
//! 見てから、受け取った refresh token で終える。終えた後は、同じ Cookie でも authorize がログインの画面を出し、
//! refresh も断られる（開き直したログイン画面で、Keycloak が同じ人を黙って通さない）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use api::config::{AuthConfig, Tenancy};
use api::oidc;

/// realm の import の `shiki-web`（confidential）の dev の資格情報と戻り先。
const CLIENT_SECRET: &str = "shiki-web-dev-secret";
const REDIRECT_URI: &str = "http://localhost:3000/auth/callback";
/// realm の import の試しの利用者。
const USERNAME: &str = "charlie";
const PASSWORD: &str = "password";

fn auth_config(base: &str) -> AuthConfig {
    AuthConfig {
        issuer: format!("{base}/realms/shiki"),
        internal_base_url: None,
        jwks_uri: None,
        audience: "shiki-api".into(),
        jwks_ttl_secs: 300,
        client_id: "shiki-web".into(),
        client_secret: Some(CLIENT_SECRET.into()),
        redirect_uri: REDIRECT_URI.into(),
        post_logout_redirect_uri: "http://localhost:3000/".into(),
        scopes: "openid profile".into(),
        tenancy: Tenancy::Single,
        tenant_id: Some("default".into()),
        provisioner_client_id: None,
        provisioner_client_secret: None,
        admin_base_url: None,
    }
}

/// Cookie を持ち回るブラウザの代わり（Keycloak の Cookie は Secure 付きなので、HTTP の Cookie の入れ物を使わず、
/// 応答の Set-Cookie をそのまま次の要求に付ける）。
struct Browser {
    http: reqwest::Client,
    cookies: Vec<(String, String)>,
}

impl Browser {
    fn new() -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client");
        Browser {
            http,
            cookies: Vec::new(),
        }
    }

    /// 応答の Set-Cookie を持つ（同じ名前は置き換え、空の値は消す）。
    fn keep(&mut self, resp: &reqwest::Response) {
        for raw in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            let Some((name, value)) = raw
                .to_str()
                .ok()
                .and_then(|v| v.split(';').next())
                .and_then(|kv| kv.split_once('='))
            else {
                continue;
            };
            self.cookies.retain(|(n, _)| n != name);
            if !value.is_empty() {
                self.cookies.push((name.to_string(), value.to_string()));
            }
        }
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// shiki-web の authorize（BFF の `/auth/login` と同じ要求・`prompt` なし）。
    async fn authorize(&mut self, auth: &AuthConfig, challenge: &str) -> reqwest::Response {
        let resp = self
            .http
            .get(auth.authorize_endpoint())
            .header(reqwest::header::COOKIE, self.cookie_header())
            .query(&[
                ("client_id", auth.client_id.as_str()),
                ("response_type", "code"),
                ("redirect_uri", REDIRECT_URI),
                ("scope", "openid"),
                ("state", "test-state"),
                ("code_challenge", challenge),
                ("code_challenge_method", "S256"),
            ])
            .send()
            .await
            .expect("authorize");
        self.keep(&resp);
        resp
    }
}

/// 戻り先の問い合わせの認可コード（無ければ `None`）。
fn code_of(resp: &reqwest::Response) -> Option<String> {
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?;
    location
        .starts_with(REDIRECT_URI)
        .then(|| {
            location
                .split(['?', '&'])
                .find_map(|kv| kv.strip_prefix("code="))
                .map(str::to_string)
        })
        .flatten()
}

/// ログインの画面のフォームの送り先（HTML の `&amp;` を戻す）。
fn login_action(html: &str) -> Option<String> {
    let form = &html[html.find("<form")?..];
    let start = form.find("action=\"")? + "action=\"".len();
    let end = form[start..].find('"')?;
    Some(form[start..start + end].replace("&amp;", "&"))
}

#[tokio::test]
async fn ending_the_sso_session_makes_keycloak_ask_for_a_login_again() {
    let Ok(base) = shiki_testenv::var("KEYCLOAK_TEST_URL") else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    let auth = auth_config(base.trim_end_matches('/'));
    let http = reqwest::Client::new();
    let mut browser = Browser::new();
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = oidc::pkce_challenge(&verifier);

    // 1. 名前とパスワードで入り、BFF と同じ手順で code をトークンに換える。
    let page = browser.authorize(&auth, &challenge).await;
    assert_eq!(page.status(), reqwest::StatusCode::OK);
    let action = login_action(&page.text().await.unwrap()).expect("ログインの画面のフォーム");
    let login = browser
        .http
        .post(action)
        .header(reqwest::header::COOKIE, browser.cookie_header())
        .form(&[("username", USERNAME), ("password", PASSWORD)])
        .send()
        .await
        .expect("login");
    browser.keep(&login);
    let code = code_of(&login).unwrap_or_else(|| panic!("login: {}", login.status()));
    let tokens = oidc::exchange_code(&http, &auth, &code, &verifier)
        .await
        .expect("token");
    let refresh_token = tokens.refresh_token.expect("refresh token");

    // 2. SSO の Cookie があるので、authorize はログインの画面を出さずに code を返す（断った人が抜けられない状態）。
    let again = browser.authorize(&auth, &challenge).await;
    assert!(
        code_of(&again).is_some(),
        "SSO があれば黙って通す: {}",
        again.status()
    );

    // 3. サーバ側で終える。
    oidc::end_sso_session(&http, &auth, &refresh_token)
        .await
        .expect("end session");

    // 4. 同じ Cookie でも、authorize はログインの画面を出す（別のアカウントで入り直せる）。refresh も断られる。
    let after = browser.authorize(&auth, &challenge).await;
    assert_eq!(after.status(), reqwest::StatusCode::OK, "code を返さない");
    assert!(code_of(&after).is_none());
    assert!(
        login_action(&after.text().await.unwrap()).is_some(),
        "ログインの画面"
    );
    let refreshed = oidc::refresh_tokens(&http, &auth, &refresh_token).await;
    assert!(
        refreshed
            .as_ref()
            .is_err_and(oidc::OidcError::is_client_error),
        "終えたセッションの refresh は断られる: {:?}",
        refreshed.map(|_| ())
    );
}
