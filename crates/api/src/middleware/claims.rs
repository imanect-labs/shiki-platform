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
    /// 所属ロール（多値カスタム属性 → claim `roles`）。
    #[serde(default)]
    pub roles: Vec<String>,
    /// テナント識別子（SaaS の Keycloak protocol mapper → claim `tenant`）。
    /// オンプレ/cell のシングルテナントでは付与されず、設定の固定値にフォールバックする。
    #[serde(default)]
    pub tenant: Option<String>,
    /// authorized party（トークンを取得した client_id）。`/admin/*` の provisioner 照合に使う。
    #[serde(default)]
    pub azp: Option<String>,
    /// Keycloak の SSO セッション id（claim `sid`）。backchannel logout で
    /// 当該セッションのみを失効させるためにセッションレコードへ保持する。
    #[serde(default)]
    pub sid: Option<String>,
}

/// OIDC Back-Channel Logout 1.0 の logout_token クレーム。
///
/// Keycloak がユーザーのセッション終了（ログアウト・管理者による無効化/削除）時に
/// RP の backchannel logout URL へ POST する JWT。署名/iss/aud の検証は
/// [`verify_logout_token`](crate::middleware::auth::verify_logout_token) が行い、
/// ここでは失効対象（`sid`/`sub`）と logout イベント要件の検証に必要な項目を持つ。
#[derive(Debug, Clone, Deserialize)]
pub struct LogoutClaims {
    /// 失効対象ユーザー（Keycloak user id）。`sid` が無い場合は当該ユーザーの全セッションを失効。
    #[serde(default)]
    pub sub: Option<String>,
    /// 失効対象の SSO セッション id。存在すればそのセッションのみ失効。
    #[serde(default)]
    pub sid: Option<String>,
    /// logout イベント宣言（`http://schemas.openid.net/event/backchannel-logout` キー必須）。
    #[serde(default)]
    pub events: std::collections::HashMap<String, serde_json::Value>,
    /// logout_token は `nonce` を**含んではならない**（OIDC BCL §2.4）。含めば拒否。
    #[serde(default)]
    pub nonce: Option<String>,
}

/// OIDC Back-Channel Logout で必須の logout イベント種別。
pub const BACKCHANNEL_LOGOUT_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";

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
        roles: claims.roles,
        tenant_id: claims.tenant,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::{Algorithm, DecodingKey, Validation};

    /// JSON ペイロードを JWT の base64url ペイロード部に符号化する。
    fn encode_payload(json: &serde_json::Value) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(json).unwrap())
    }

    #[test]
    fn claims_minimal_deserialize() {
        // 任意フィールド欠落時は default（None / 空 Vec）になること。
        let claims: Claims =
            serde_json::from_value(serde_json::json!({ "sub": "user-1" })).unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.email, None);
        assert_eq!(claims.preferred_username, None);
        assert!(claims.groups.is_empty());
        assert!(claims.roles.is_empty());
        assert_eq!(claims.tenant, None);
    }

    #[test]
    fn claims_full_deserialize_ignores_unknown() {
        // 全フィールド＋未知フィールド（無視される）を含む。
        let claims: Claims = serde_json::from_value(serde_json::json!({
            "sub": "user-2",
            "email": "u@example.com",
            "preferred_username": "u",
            "groups": ["/acme/eng", "/acme"],
            "roles": ["eng", "sales"],
            "tenant": "acme",
            "exp": 9999999999u64,
            "unknown_field": "ignored",
        }))
        .unwrap();
        assert_eq!(claims.sub, "user-2");
        assert_eq!(claims.email.as_deref(), Some("u@example.com"));
        assert_eq!(claims.preferred_username.as_deref(), Some("u"));
        assert_eq!(claims.groups, vec!["/acme/eng", "/acme"]);
        assert_eq!(claims.roles, vec!["eng", "sales"]);
        assert_eq!(claims.tenant.as_deref(), Some("acme"));
    }

    #[test]
    fn claims_missing_sub_is_error() {
        // sub は必須（default 無し）なので欠落はエラー。
        let result: Result<Claims, _> = serde_json::from_value(serde_json::json!({ "email": "x" }));
        assert!(result.is_err());
    }

    #[test]
    fn principal_from_claims_maps_fields() {
        // claim → Principal のフィールド対応が正しいこと。
        let claims = Claims {
            sub: "user-3".into(),
            email: Some("p@example.com".into()),
            preferred_username: Some("p".into()),
            groups: vec!["/acme".into()],
            roles: vec!["sales".into(), "eng".into()],
            tenant: Some("acme".into()),
            azp: None,
            sid: None,
        };
        let principal = principal_from_claims(claims);
        assert_eq!(principal.id, "user-3");
        assert_eq!(principal.email.as_deref(), Some("p@example.com"));
        assert_eq!(principal.groups, vec!["/acme"]);
        assert_eq!(principal.roles, vec!["sales", "eng"]);
        assert_eq!(principal.tenant_id.as_deref(), Some("acme"));
        // preferred_username は Principal には載らない（マッピング対象外）。
    }

    #[test]
    fn auth_error_display_messages() {
        // 各 AuthError の表示文言（ログ・401 経路で使う）。
        assert_eq!(
            AuthError::MissingBearer.to_string(),
            "Authorization ヘッダがありません"
        );
        assert_eq!(
            AuthError::UnknownKid.to_string(),
            "トークンの kid が JWKS に存在しません"
        );
        assert!(AuthError::InvalidToken("x".into())
            .to_string()
            .contains("不正"));
        assert!(AuthError::JwksFetch("net".into())
            .to_string()
            .contains("net"));
    }

    #[test]
    fn decode_claims_insecure_extracts_payload() {
        // header.payload.sig の中間部からクレームを取り出す（署名検証なし）。
        let payload = encode_payload(&serde_json::json!({ "sub": "user-4", "tenant": "acme" }));
        let token = format!("aGVhZGVy.{payload}.c2ln");
        let claims = decode_claims_insecure(&token).unwrap();
        assert_eq!(claims.sub, "user-4");
        assert_eq!(claims.tenant.as_deref(), Some("acme"));
    }

    #[test]
    fn decode_claims_insecure_missing_payload_part() {
        // ドットが無くペイロード部が取れない場合はエラー。
        let result = decode_claims_insecure("not-a-jwt");
        assert!(matches!(result, Err(AuthError::InvalidToken(_))));
    }

    #[test]
    fn decode_claims_insecure_bad_base64() {
        // ペイロード部が base64url として不正ならエラー。
        let result = decode_claims_insecure("h.@@@invalid@@@.s");
        assert!(matches!(result, Err(AuthError::InvalidToken(_))));
    }

    #[test]
    fn decode_claims_insecure_invalid_json() {
        // base64 は通るが JSON として不正ならエラー。
        let payload = URL_SAFE_NO_PAD.encode(b"{ not json");
        let token = format!("h.{payload}.s");
        let result = decode_claims_insecure(&token);
        assert!(matches!(result, Err(AuthError::InvalidToken(_))));
    }

    #[test]
    fn verify_token_rejects_malformed_token() {
        // 署名検証経路: 形式不正なトークンは InvalidToken になる（外部 I/O なし）。
        let key = DecodingKey::from_secret(b"secret");
        let validation = Validation::new(Algorithm::HS256);
        let result = verify_token("garbage.token.value", &key, &validation);
        assert!(matches!(result, Err(AuthError::InvalidToken(_))));
    }
}
