//! OIDC access token の検証ヘルパ（JWKS で署名/aud/iss/exp を検証）。
//!
//! BFF 化により `Authorization: Bearer` 入口は撤去した（docs/auth/browser-token-strategy.md /
//! roadmap phase-0 Task 0.11）。本モジュールは BFF callback での token 交換後検証など、
//! サーバ側が受領したトークンの検証に再利用される。内部/サービス間（skillex 等）の
//! ステートレス JWT 検証もここを通る。

use jsonwebtoken::{Algorithm, Validation};

use super::claims::{self, AuthError, Claims};
use crate::{error::ApiError, state::AppState};

impl From<AuthError> for ApiError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::JwksFetch(detail) => ApiError::Internal(format!("jwks: {detail}")),
            AuthError::MissingBearer | AuthError::InvalidToken(_) | AuthError::UnknownKid => {
                tracing::debug!(error = %err, "認証失敗");
                ApiError::Unauthorized
            }
        }
    }
}

/// access token を JWKS で検証してクレームを取り出す（kid 解決 → 署名/aud/iss/exp 検証）。
///
/// BFF callback（token 交換後の検証）と内部 JWT 検証で共用する。
pub async fn verify_access_token(state: &AppState, token: &str) -> Result<Claims, AuthError> {
    // kid を取り出して対応する鍵を解決。
    let header =
        jsonwebtoken::decode_header(token).map_err(|e| AuthError::InvalidToken(e.to_string()))?;
    let kid = header
        .kid
        .ok_or_else(|| AuthError::InvalidToken("kid がありません".into()))?;
    let key = state.jwks.key_for_kid(&kid).await?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[state.config.auth.audience.as_str()]);
    validation.set_issuer(&[state.config.auth.issuer.as_str()]);
    validation.validate_exp = true;
    // aud/iss を必須化。これが無いと aud を含まない（別 client 向けの）トークンが
    // 素通りして audience 境界を破れる。exp は既定で必須。
    validation.set_required_spec_claims(&["exp", "aud", "iss"]);

    claims::verify_token(token, &key, &validation)
}
