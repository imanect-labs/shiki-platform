//! 実 Keycloak を使う結合テストの手助け（feature `test-support`・本番のビルドには入らない・BUG-172）。
//!
//! realm `shiki`（`deploy/keycloak/shiki-realm.json` の dev の import）の provisioner と、試しの利用者（alice・
//! bob・charlie。パスワードは `password`）を使う。`KEYCLOAK_TEST_URL`（例 `http://localhost:8081`）が無ければ
//! [`TestRealm::from_env`] は `None` を返し、テストはスキップする。
//!
//! 利用者のトークンは、ブラウザーと同じ認可コード＋PKCE で取る（ログインの画面に名前とパスワードを送る）。
//! Keycloak の Cookie は Secure 付きなので、HTTP の Cookie の入れ物は送らない。ここでは応答の Set-Cookie を
//! そのまま次の要求に付ける。

// テストの手助け: 前提が崩れたら（Keycloak に届かない・realm の import が違う）その場で落とす。
#![allow(
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::expect_used,
    clippy::similar_names
)]

use base64::Engine as _;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::DecodingKey;
use sha2::Digest as _;

use crate::{GatewayError, KeyResolver, OAuthClient};

/// realm の名前（dev の import）。
const REALM: &str = "shiki";
/// provisioner の dev の資格情報（realm の import と同じ）。
const PROVISIONER_ID: &str = "shiki-provisioner";
const PROVISIONER_SECRET: &str = "shiki-provisioner-dev-secret";
/// 試しの利用者のパスワード（realm の import と同じ）。
const USER_PASSWORD: &str = "password";

/// 試しの realm（Keycloak の base URL）。
pub struct TestRealm {
    base: String,
    http: reqwest::Client,
}

impl TestRealm {
    /// `KEYCLOAK_TEST_URL` から作る（無ければ `None`＝テストはスキップ）。
    pub fn from_env() -> Option<Self> {
        let base = std::env::var("KEYCLOAK_TEST_URL").ok()?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client");
        Some(TestRealm {
            base: base.trim_end_matches('/').to_string(),
            http,
        })
    }

    /// トークンの `iss`（realm の URL）。
    pub fn issuer(&self) -> String {
        format!("{}/realms/{REALM}", self.base)
    }

    /// admin REST の base。
    pub fn admin_base(&self) -> String {
        format!("{}/admin/realms/{REALM}", self.base)
    }

    pub fn token_endpoint(&self) -> String {
        format!("{}/protocol/openid-connect/token", self.issuer())
    }

    pub fn jwks_uri(&self) -> String {
        format!("{}/protocol/openid-connect/certs", self.issuer())
    }

    /// provisioner の資格情報（`(id, secret)`）。
    pub fn provisioner(&self) -> (&'static str, &'static str) {
        (PROVISIONER_ID, PROVISIONER_SECRET)
    }

    /// インストールがクライアントを登録するのと同じ資格情報の [`OAuthClient`]。
    pub fn oauth_client(&self) -> OAuthClient {
        OAuthClient::new(
            reqwest::Client::new(),
            self.admin_base(),
            self.token_endpoint(),
            PROVISIONER_ID.into(),
            PROVISIONER_SECRET.into(),
        )
    }

    async fn admin_token(&self) -> String {
        let resp = self
            .http
            .post(self.token_endpoint())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", PROVISIONER_ID),
                ("client_secret", PROVISIONER_SECRET),
            ])
            .send()
            .await
            .expect("admin token");
        assert!(resp.status().is_success(), "admin token: {}", resp.status());
        let body: serde_json::Value = resp.json().await.expect("admin token json");
        body["access_token"]
            .as_str()
            .expect("access_token")
            .to_string()
    }

    /// クライアントの表現（`defaultClientScopes`・`optionalClientScopes`・`protocolMappers` を含む）。無ければ `None`。
    pub async fn client(&self, client_id: &str) -> Option<serde_json::Value> {
        let token = self.admin_token().await;
        let list: Vec<serde_json::Value> = self
            .http
            .get(format!("{}/clients", self.admin_base()))
            .query(&[("clientId", client_id)])
            .bearer_auth(&token)
            .send()
            .await
            .expect("clients")
            .json()
            .await
            .expect("clients json");
        list.into_iter().next()
    }

    /// クライアントを表現のまま作る（前の版で登録したクライアントを模す）。
    pub async fn create_client_raw(&self, rep: &serde_json::Value) {
        let token = self.admin_token().await;
        let resp = self
            .http
            .post(format!("{}/clients", self.admin_base()))
            .bearer_auth(&token)
            .json(rep)
            .send()
            .await
            .expect("create client");
        assert!(
            resp.status().is_success(),
            "create client: {}",
            resp.status()
        );
    }

    /// クライアントを部分の表現で書き換える（無効にする・戻り先を変えるなど）。
    pub async fn update_client_raw(&self, client_id: &str, rep: &serde_json::Value) {
        let id = self.internal_id(client_id).await.expect("client");
        let token = self.admin_token().await;
        let resp = self
            .http
            .put(format!("{}/clients/{id}", self.admin_base()))
            .bearer_auth(&token)
            .json(rep)
            .send()
            .await
            .expect("update client");
        assert!(
            resp.status().is_success(),
            "update client: {}",
            resp.status()
        );
    }

    /// クライアントの既定のスコープを外す（前の版で登録したクライアントを模す）。
    pub async fn remove_default_scopes(&self, client_id: &str) {
        let id = self.internal_id(client_id).await.expect("client");
        let token = self.admin_token().await;
        let base = format!("{}/clients/{id}/default-client-scopes", self.admin_base());
        let have: Vec<serde_json::Value> = self
            .http
            .get(&base)
            .bearer_auth(&token)
            .send()
            .await
            .expect("default scopes")
            .json()
            .await
            .expect("default scopes json");
        for scope in have {
            let sid = scope["id"].as_str().expect("scope id");
            let resp = self
                .http
                .delete(format!("{base}/{sid}"))
                .bearer_auth(&token)
                .send()
                .await
                .expect("remove scope");
            assert!(
                resp.status().is_success(),
                "remove scope: {}",
                resp.status()
            );
        }
    }

    async fn internal_id(&self, client_id: &str) -> Option<String> {
        self.client(client_id)
            .await
            .and_then(|c| c["id"].as_str().map(str::to_string))
    }

    /// クライアントを消す（テストの片付け・無ければ何もしない）。
    pub async fn delete_client(&self, client_id: &str) {
        let Some(id) = self.internal_id(client_id).await else {
            return;
        };
        let token = self.admin_token().await;
        let resp = self
            .http
            .delete(format!("{}/clients/{id}", self.admin_base()))
            .bearer_auth(&token)
            .send()
            .await
            .expect("delete client");
        assert!(
            resp.status().is_success(),
            "delete client: {}",
            resp.status()
        );
    }

    /// 利用者 `username` が、クライアント `client_id`（public・PKCE S256）で、戻り先 `redirect_uri` を使って
    /// アクセストークンを取る（シェルのポップアップと同じ要求）。Keycloak が断ったら、その段と状態を `Err` で返す。
    pub async fn user_token(
        &self,
        client_id: &str,
        redirect_uri: &str,
        username: &str,
        scope: &str,
    ) -> Result<String, String> {
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()));
        let page = self
            .http
            .get(format!("{}/protocol/openid-connect/auth", self.issuer()))
            .query(&[
                ("client_id", client_id),
                ("response_type", "code"),
                ("redirect_uri", redirect_uri),
                ("scope", scope),
                ("state", "test-state"),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
            ])
            .send()
            .await
            .map_err(|e| format!("auth: {e}"))?;
        if !page.status().is_success() {
            return Err(format!("auth: {}", page.status()));
        }
        let cookie = cookies_of(&page);
        let html = page.text().await.map_err(|e| format!("auth body: {e}"))?;
        let action = login_action(&html).ok_or("ログインの画面にフォームがありません")?;
        let login = self
            .http
            .post(action)
            .header(reqwest::header::COOKIE, cookie)
            .form(&[("username", username), ("password", USER_PASSWORD)])
            .send()
            .await
            .map_err(|e| format!("login: {e}"))?;
        let location = login
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        // 認可コードは URL で使える文字だけなので、戻り先の問い合わせから切り出すだけでよい。
        let code = location
            .split(['?', '&'])
            .find_map(|kv| kv.strip_prefix("code="))
            .map(str::to_string)
            .ok_or_else(|| format!("login: {} {location}", login.status()))?;
        if !location.starts_with(redirect_uri) {
            return Err(format!("戻り先が違います: {location}"));
        }
        let token = self
            .http
            .post(self.token_endpoint())
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("token: {e}"))?;
        if !token.status().is_success() {
            let status = token.status();
            return Err(format!(
                "token: {status} {}",
                token.text().await.unwrap_or_default()
            ));
        }
        let body: serde_json::Value = token.json().await.map_err(|e| format!("token: {e}"))?;
        body["access_token"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "token: access_token がありません".to_string())
    }

    /// realm の JWKS で検証鍵を引く [`KeyResolver`]（ゲートウェイの検証を本物の鍵で通す）。
    pub fn key_resolver(&self) -> JwksResolver {
        JwksResolver {
            http: self.http.clone(),
            jwks_uri: self.jwks_uri(),
        }
    }
}

/// realm の JWKS を毎回引く検証鍵の解決（テスト用・キャッシュしない）。
pub struct JwksResolver {
    http: reqwest::Client,
    jwks_uri: String,
}

#[async_trait::async_trait]
impl KeyResolver for JwksResolver {
    async fn resolve(&self, kid: &str) -> Result<DecodingKey, GatewayError> {
        let set: JwkSet = self
            .http
            .get(&self.jwks_uri)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("jwks: {e}")))?
            .json()
            .await
            .map_err(|e| GatewayError::Upstream(format!("jwks: {e}")))?;
        let jwk = set
            .find(kid)
            .ok_or_else(|| GatewayError::Unauthenticated("未知の kid".into()))?;
        DecodingKey::from_jwk(jwk).map_err(|e| GatewayError::Upstream(format!("jwk: {e}")))
    }
}

/// トークンのクレーム（署名は確かめない・テストで中身を見るだけ）。
pub fn claims(token: &str) -> serde_json::Value {
    let payload = token.split('.').nth(1).expect("JWT");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("JWT payload");
    serde_json::from_slice(&bytes).expect("JWT claims")
}

/// 応答の Set-Cookie を、次の要求の Cookie の値にする。
fn cookies_of(resp: &reqwest::Response) -> String {
    resp.headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .collect::<Vec<_>>()
        .join("; ")
}

/// ログインの画面のフォームの送り先（HTML の `&amp;` を戻す）。
fn login_action(html: &str) -> Option<String> {
    let form = &html[html.find("<form")?..];
    let start = form.find("action=\"")? + "action=\"".len();
    let end = form[start..].find('"')?;
    Some(form[start..start + end].replace("&amp;", "&"))
}
