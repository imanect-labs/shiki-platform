//! OIDC backchannel クライアント（BFF のサーバ側 token 交換・refresh・PKCE）。
//!
//! ブラウザは code を運ぶだけで、code↔token 交換と refresh は**サーバ側**で行う
//! （docs/auth/browser-token-strategy.md §4）。ここはトークンをブラウザに出さない
//! 不変条件を支える層。

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::AuthConfig;

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    /// token エンドポイントへの到達自体が失敗（ネットワーク等）。
    #[error("OIDC token エンドポイントへの接続に失敗: {0}")]
    Transport(String),
    /// token エンドポイントが 4xx/5xx を返した（invalid_grant 等）。
    #[error("OIDC token エンドポイントがエラー応答: status={status} body={body}")]
    Status { status: u16, body: String },
}

/// OIDC token エンドポイントの応答（必要な項目のみ）。
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// access token の有効秒数。
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub id_token: Option<String>,
}

/// PKCE の code_challenge（S256）を verifier から導出する。
pub fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// authorization code を token に交換する（PKCE verifier 付き）。
pub async fn exchange_code(
    http: &reqwest::Client,
    auth: &AuthConfig,
    code: &str,
    code_verifier: &str,
) -> Result<TokenResponse, OidcError> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", auth.redirect_uri.as_str()),
        ("client_id", auth.client_id.as_str()),
        ("code_verifier", code_verifier),
    ];
    if let Some(secret) = auth.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    post_token(http, auth, &form).await
}

/// refresh token で access/refresh をローテーション更新する。
pub async fn refresh_tokens(
    http: &reqwest::Client,
    auth: &AuthConfig,
    refresh_token: &str,
) -> Result<TokenResponse, OidcError> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", auth.client_id.as_str()),
    ];
    if let Some(secret) = auth.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    post_token(http, auth, &form).await
}

async fn post_token(
    http: &reqwest::Client,
    auth: &AuthConfig,
    form: &[(&str, &str)],
) -> Result<TokenResponse, OidcError> {
    let resp = http
        .post(auth.token_endpoint())
        .form(form)
        .send()
        .await
        .map_err(|e| OidcError::Transport(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(OidcError::Status {
            status: status.as_u16(),
            body,
        });
    }
    resp.json::<TokenResponse>()
        .await
        .map_err(|e| OidcError::Transport(e.to_string()))
}
