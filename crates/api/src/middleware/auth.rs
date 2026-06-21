//! 認証ミドルウェア: `Authorization: Bearer` を検証し `Principal` を載せる。
//!
//! SSE の fetch-stream も同じ Authorization ヘッダ経路を通る前提（docs/design.md §4.1）。

use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{Algorithm, Validation};

use super::claims::{self, AuthError, Claims};
use crate::{
    error::ApiError,
    extract::{resolve_tenant_id, TenantId},
    state::AppState,
};

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
/// Bearer ミドルウェアと BFF callback（token 交換後の検証）で共用する。
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

/// 保護ルートに適用する axum middleware。検証成功で `Principal` を extension に載せる。
pub async fn require_auth(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token(&req).ok_or(AuthError::MissingBearer)?;

    let claims = verify_access_token(&state, &token).await?;
    let principal = claims::principal_from_claims(claims);
    // tenant_id をここで解決して extension に載せる（state を持つのは middleware 側のため）。
    // AuthContext extractor はこれを取り出して principal + org と合わせて組み立てる。
    let tenant_id = resolve_tenant_id(&principal, &state.config.auth)?;
    req.extensions_mut().insert(principal);
    req.extensions_mut().insert(TenantId(tenant_id));

    Ok(next.run(req).await)
}

fn bearer_token(req: &Request) -> Option<String> {
    let value = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}
