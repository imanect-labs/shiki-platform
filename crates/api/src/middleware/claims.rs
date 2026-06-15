//! JWT クレームの型と純粋な検証ロジック（副作用なし）。
//!
//! ここを副作用のない関数群に保ち、将来 `crates/auth` へ切り出せるようにする。

use authz::Principal;
use jsonwebtoken::{DecodingKey, Validation};
use serde::Deserialize;

/// アクセストークンから抽出するクレーム。
/// `exp`/`aud`/`iss` は [`Validation`] が生クレームから検証するため、構造体には
/// 抽出に必要な項目だけを持つ（未知フィールドは無視）。
#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    /// ユーザー ID（OIDC `sub`）。
    pub sub: String,
    pub email: Option<String>,
    pub preferred_username: Option<String>,
    /// Keycloak group マッパー由来。
    #[serde(default)]
    pub groups: Vec<String>,
    /// 所属部署（カスタム属性 → claim `department`）。
    #[serde(default)]
    pub department: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Authorization ヘッダがありません")]
    MissingBearer,
    #[error("トークンが不正です: {0}")]
    InvalidToken(String),
    #[error("トークンの kid が JWKS に存在しません")]
    UnknownKid,
    #[error("JWKS の取得に失敗しました: {0}")]
    JwksFetch(String),
}

/// トークンを検証してクレームを取り出す（純粋関数）。
pub fn verify_token(
    token: &str,
    key: &DecodingKey,
    validation: &Validation,
) -> Result<Claims, AuthError> {
    jsonwebtoken::decode::<Claims>(token, key, validation)
        .map(|data| data.claims)
        .map_err(|e| AuthError::InvalidToken(e.to_string()))
}

/// クレームから認証主体を組み立てる（純粋関数）。
pub fn principal_from_claims(claims: Claims) -> Principal {
    Principal {
        id: claims.sub,
        email: claims.email,
        groups: claims.groups,
        dept: claims.department,
    }
}
