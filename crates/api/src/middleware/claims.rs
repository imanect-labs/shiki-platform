//! JWT クレームの型と純粋な検証ロジック（副作用なし）。
//!
//! ここを副作用のない関数群に保ち、将来 `crates/auth` へ切り出せるようにする。

use authz::Principal;
use base64::Engine;
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
    /// テナント識別子（SaaS の Keycloak protocol mapper → claim `tenant`）。
    /// オンプレ/cell のシングルテナントでは付与されず、設定の固定値にフォールバックする。
    #[serde(default)]
    pub tenant: Option<String>,
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

/// **署名検証せずに**ペイロードからクレームを取り出す（純粋関数）。
///
/// BFF の refresh 経路専用。リフレッシュで受領した access token は、こちらが設定済みの
/// Keycloak token エンドポイントへ TLS backchannel で問い合わせて得た**信頼済み応答**であり、
/// 真正性は接続の信頼に拠る（ログイン時の callback では JWKS で完全検証している）。
/// これにより JWKS の kid 未知スロットルに左右されず、refresh 後に最新クレームへ追従できる。
/// ブラウザ由来トークンには**絶対に使わない**こと。
pub fn decode_claims_insecure(token: &str) -> Result<Claims, AuthError> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| AuthError::InvalidToken("JWT のペイロード部がありません".into()))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| AuthError::InvalidToken(format!("ペイロードの base64 decode に失敗: {e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AuthError::InvalidToken(format!("クレームの parse に失敗: {e}")))
}

/// クレームから認証主体を組み立てる（純粋関数）。
pub fn principal_from_claims(claims: Claims) -> Principal {
    Principal {
        id: claims.sub,
        email: claims.email,
        groups: claims.groups,
        dept: claims.department,
        tenant_id: claims.tenant,
    }
}
