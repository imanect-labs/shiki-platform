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

impl OidcError {
    /// 4xx（invalid_grant 等、refresh token が無効/失効）か。
    ///
    /// `true` ならセッションを破棄してよい。`false`（transport/5xx）は一過性障害として
    /// セッションを破棄せず継続/リトライさせるための判定に使う。
    pub fn is_client_error(&self) -> bool {
        matches!(self, OidcError::Status { status, .. } if (400..500).contains(status))
    }
}

/// OIDC token エンドポイントの応答（必要な項目のみ）。
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// access token の有効秒数（必須）。欠落を 0 で受理すると保存時に即期限切れになり
    /// refresh 連打や即 401 を招くため、必須項目として fail-fast にする。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_known_vector() {
        // RFC 7636 Appendix B の既知ベクタで S256 導出を固定する。
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = pkce_challenge(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn pkce_challenge_is_url_safe_no_pad() {
        // 出力は base64url(no-pad)。S256 は 32 バイト → 43 文字。
        let challenge = pkce_challenge("some-random-verifier-value");
        assert_eq!(challenge.len(), 43);
        assert!(!challenge.contains('='));
        assert!(challenge
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn pkce_challenge_is_deterministic() {
        // 同じ verifier からは常に同じ challenge が出る。
        assert_eq!(pkce_challenge("v"), pkce_challenge("v"));
        assert_ne!(pkce_challenge("a"), pkce_challenge("b"));
    }

    #[test]
    fn is_client_error_true_for_4xx() {
        // 4xx は refresh token 無効/失効＝セッション破棄してよい。
        assert!(OidcError::Status {
            status: 400,
            body: String::new()
        }
        .is_client_error());
        assert!(OidcError::Status {
            status: 499,
            body: String::new()
        }
        .is_client_error());
    }

    #[test]
    fn is_client_error_false_for_5xx_and_transport() {
        // 5xx / transport は一過性障害＝セッションを破棄しない。
        assert!(!OidcError::Status {
            status: 500,
            body: String::new()
        }
        .is_client_error());
        assert!(!OidcError::Status {
            status: 503,
            body: String::new()
        }
        .is_client_error());
        assert!(!OidcError::Transport("net".into()).is_client_error());
    }

    #[test]
    fn is_client_error_boundary_at_400_and_399() {
        // 境界: 400 は 4xx、399 は範囲外（3xx 想定）。
        assert!(OidcError::Status {
            status: 400,
            body: String::new()
        }
        .is_client_error());
        assert!(!OidcError::Status {
            status: 399,
            body: String::new()
        }
        .is_client_error());
    }

    #[test]
    fn token_response_full_deserialize() {
        // 全フィールドありの応答を解釈する。
        let resp: TokenResponse = serde_json::from_value(serde_json::json!({
            "access_token": "at",
            "refresh_token": "rt",
            "expires_in": 300,
            "id_token": "it",
        }))
        .unwrap();
        assert_eq!(resp.access_token, "at");
        assert_eq!(resp.refresh_token.as_deref(), Some("rt"));
        assert_eq!(resp.expires_in, 300);
        assert_eq!(resp.id_token.as_deref(), Some("it"));
    }

    #[test]
    fn token_response_optional_fields_default_to_none() {
        // refresh_token / id_token は任意（default None）。
        let resp: TokenResponse = serde_json::from_value(serde_json::json!({
            "access_token": "at",
            "expires_in": 60,
        }))
        .unwrap();
        assert_eq!(resp.refresh_token, None);
        assert_eq!(resp.id_token, None);
    }

    #[test]
    fn token_response_requires_expires_in() {
        // expires_in は必須（欠落を 0 で受理すると即期限切れになるため fail-fast）。
        let result: Result<TokenResponse, _> = serde_json::from_value(serde_json::json!({
            "access_token": "at",
        }));
        assert!(result.is_err());
    }

    #[test]
    fn token_response_requires_access_token() {
        // access_token も必須。
        let result: Result<TokenResponse, _> = serde_json::from_value(serde_json::json!({
            "expires_in": 60,
        }));
        assert!(result.is_err());
    }

    #[test]
    fn oidc_error_display() {
        // 表示文言（ログ用）。Status はステータスとボディを含む。
        let status = OidcError::Status {
            status: 400,
            body: "invalid_grant".into(),
        };
        let msg = status.to_string();
        assert!(msg.contains("400"));
        assert!(msg.contains("invalid_grant"));
        assert!(OidcError::Transport("refused".into())
            .to_string()
            .contains("refused"));
    }
}
