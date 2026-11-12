//! ゲートウェイ・アクセストークンの検証（Task 9.6/9.7・JWKS ローカル検証）。
//!
//! ミニアプリが提示する Bearer JWT を**署名（JWKS）＋ iss ＋ exp ＋ aud（`gateway.audience`・既定 `shiki-gateway`）**で
//! 検証し、`azp`（=登録 client_id）・`sub`（呼出ユーザー）・`scope`（トークン付与スコープ）を
//! 取り出す。実効スコープは後段で `granted_scopes` と突合される（token scope だけには依存しない）。
//!
//! JWKS 取得は [`KeyResolver`] 越しに委譲する（`crates/api` の `JwksCache` を実装として渡す・
//! app-gateway → api の依存循環を避けるためトレイト境界で分離）。

use authz::CapabilityScope;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::GatewayError;

/// kid → 検証鍵の解決（JWKS キャッシュのトレイト境界）。
///
/// `crates/api` の `JwksCache` がこれを実装し、ゲートウェイ配線時に注入される。
#[async_trait::async_trait]
pub trait KeyResolver: Send + Sync {
    /// kid に対応する RS256 検証鍵を返す（未知 kid は再取得・fail-closed）。
    async fn resolve(&self, kid: &str) -> Result<DecodingKey, GatewayError>;
}

/// ゲートウェイトークン検証の設定（aud / iss）。
#[derive(Debug, Clone)]
pub struct GatewayTokenConfig {
    /// 期待する audience（デプロイ設定 `gateway.audience`・既定 [`crate::DEFAULT_GATEWAY_AUDIENCE`]）。インストールが
    /// 同じ値をアプリのクライアントのマッパーに書く（[`crate::OAuthClient::with_audience`]・BUG-206）。
    pub audience: String,
    /// 期待する issuer（Keycloak realm URL）。
    pub issuer: String,
}

/// 検証対象のクレーム（署名/iss/exp/aud は [`Validation`] が検証・ここは抽出項目のみ）。
#[derive(Debug, Deserialize)]
struct GatewayClaims {
    /// 呼出ユーザー（OIDC sub）。token-exchange 後もユーザーを維持する（confused-deputy 防御）。
    sub: String,
    /// authorized party（トークンを取得した client_id＝登録ミニアプリ）。
    #[serde(default)]
    azp: Option<String>,
    /// テナント識別子（SaaS の protocol mapper 由来・single では設定固定値へフォールバック）。
    #[serde(default)]
    tenant: Option<String>,
    /// 付与スコープ（スペース区切り・OIDC 標準の `scope` クレーム）。
    #[serde(default)]
    scope: String,
    // 所属グループ（`groups`）は読まない。組織はインストールから決め、OpenFGA の organization#member で確かめる
    // （D-130）。前の版で登録したクライアントのトークンに載っていても無視する。
}

/// 検証済みトークンから得たゲートウェイ呼出主体。
#[derive(Debug, Clone)]
pub struct GatewayIdentity {
    /// 呼出ユーザー（OIDC sub）。二重ゲートのユーザー ReBAC はこの主体で評価する。
    pub user_sub: String,
    /// 登録ミニアプリ client_id（azp）。インストール解決のキー。
    pub client_id: String,
    /// トークンの tenant クレーム（無ければ None・呼び出し側が固定値へ解決）。
    pub tenant: Option<String>,
    /// トークンが付与された能力スコープ（未知スコープは fail-closed で拒否済み）。
    pub token_scopes: Vec<CapabilityScope>,
}

/// Bearer トークンを検証して [`GatewayIdentity`] を得る（JWKS ローカル検証）。
///
/// `azp` 欠落は拒否（登録 client を特定できないトークンは受理しない）。`scope` に未知スコープが
/// 1 つでもあれば拒否（fail-closed・ハルシネーション/改竄境界）。
pub async fn verify_gateway_token(
    token: &str,
    resolver: &dyn KeyResolver,
    cfg: &GatewayTokenConfig,
) -> Result<GatewayIdentity, GatewayError> {
    let header = jsonwebtoken::decode_header(token)
        .map_err(|e| GatewayError::Unauthenticated(format!("JWT ヘッダ不正: {e}")))?;
    let kid = header
        .kid
        .ok_or_else(|| GatewayError::Unauthenticated("JWT に kid がありません".into()))?;
    let key = resolver.resolve(&kid).await?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[cfg.audience.as_str()]);
    validation.set_issuer(&[cfg.issuer.as_str()]);
    validation.set_required_spec_claims(&["exp", "aud", "iss"]);

    let claims = jsonwebtoken::decode::<GatewayClaims>(token, &key, &validation)
        .map(|d| d.claims)
        .map_err(|e| GatewayError::Unauthenticated(format!("トークン検証に失敗: {e}")))?;
    identity_from_claims(claims)
}

/// 署名を確かめたクレームから [`GatewayIdentity`] を組む（本番の [`verify_gateway_token`] と試しの検証が共有する）。
///
/// - `sub` に FGA の区切りの文字（`:`・`#`・`|`）・制御文字・空白があれば断る（[`authz::validate_user_id`]・D-205）。
///   ログインとセッションの更新と同じ決まりで、組織の member の check（`router.rs`）より前で断る。`|` を含む人は
///   OpenFGA が member にできてしまい、定期の照合で外れない（D-136）。
/// - `azp` 欠落は拒否（登録 client を特定できないトークンは受理しない）。
/// - `scope` に未知スコープが 1 つでもあれば拒否（fail-closed）。
fn identity_from_claims(claims: GatewayClaims) -> Result<GatewayIdentity, GatewayError> {
    if let Err(violation) = authz::validate_user_id(&claims.sub) {
        return Err(GatewayError::Unauthenticated(format!(
            "sub {:?} は利用者の id に使えません（{violation}）",
            claims.sub
        )));
    }

    let client_id = claims
        .azp
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GatewayError::Unauthenticated("azp（client_id）がありません".into()))?;

    let token_scopes = CapabilityScope::parse_scope_string(&claims.scope)
        .map_err(|e| GatewayError::Unauthenticated(format!("スコープ不正: {e}")))?;

    Ok(GatewayIdentity {
        user_sub: claims.sub,
        client_id,
        tenant: claims.tenant,
        token_scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    /// テスト用の HMAC 鍵で署名し、同じ鍵で検証する Resolver。
    struct HsResolver(Vec<u8>);

    #[async_trait::async_trait]
    impl KeyResolver for HsResolver {
        async fn resolve(&self, _kid: &str) -> Result<DecodingKey, GatewayError> {
            Ok(DecodingKey::from_secret(&self.0))
        }
    }

    /// HS256 で検証する変種（テスト専用・本番は RS256）。署名を確かめた後は本番と同じ [`identity_from_claims`] を通る。
    fn verify_hs(
        token: &str,
        secret: &[u8],
        cfg: &GatewayTokenConfig,
    ) -> Result<GatewayIdentity, GatewayError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[cfg.audience.as_str()]);
        validation.set_issuer(&[cfg.issuer.as_str()]);
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        let key = DecodingKey::from_secret(secret);
        let claims = jsonwebtoken::decode::<GatewayClaims>(token, &key, &validation)
            .map(|d| d.claims)
            .map_err(|e| GatewayError::Unauthenticated(e.to_string()))?;
        identity_from_claims(claims)
    }

    fn cfg() -> GatewayTokenConfig {
        GatewayTokenConfig {
            audience: "shiki-gateway".into(),
            issuer: "https://kc/realms/shiki".into(),
        }
    }

    fn make_token(secret: &[u8], claims: &serde_json::Value) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            claims,
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    }

    #[test]
    fn valid_token_extracts_identity() {
        let secret = b"test-secret";
        let token = make_token(
            secret,
            &serde_json::json!({
                "sub": "user-1", "azp": "app-client", "tenant": "acme",
                "scope": "openid data.read data.write",
                "aud": "shiki-gateway", "iss": "https://kc/realms/shiki",
                "exp": 9_999_999_999u64,
            }),
        );
        let id = verify_hs(&token, secret, &cfg()).unwrap();
        assert_eq!(id.user_sub, "user-1");
        assert_eq!(id.client_id, "app-client");
        assert_eq!(id.tenant.as_deref(), Some("acme"));
        assert_eq!(
            id.token_scopes,
            vec![CapabilityScope::DataRead, CapabilityScope::DataWrite]
        );
    }

    #[test]
    fn missing_azp_is_rejected() {
        let secret = b"s";
        let token = make_token(
            secret,
            &serde_json::json!({
                "sub": "u", "scope": "data.read",
                "aud": "shiki-gateway", "iss": "https://kc/realms/shiki",
                "exp": 9_999_999_999u64,
            }),
        );
        assert!(verify_hs(&token, secret, &cfg()).is_err());
    }

    #[test]
    fn unknown_scope_is_fail_closed() {
        let secret = b"s";
        let token = make_token(
            secret,
            &serde_json::json!({
                "sub": "u", "azp": "app", "scope": "data.read bogus.scope",
                "aud": "shiki-gateway", "iss": "https://kc/realms/shiki",
                "exp": 9_999_999_999u64,
            }),
        );
        assert!(verify_hs(&token, secret, &cfg()).is_err());
    }

    #[test]
    fn sub_with_fga_separators_is_unauthenticated() {
        // `|` を含む sub は OpenFGA が member にできてしまい、定期の照合で外れない（D-136）。`:`・`#`・空白・制御文字も、
        // ログインと同じく組織の member の check より前で断る（D-205）。
        let secret = b"s";
        for sub in ["a|b", "f:ldap:bob", "a#b", "a b", "a\u{7}b"] {
            let token = make_token(
                secret,
                &serde_json::json!({
                    "sub": sub, "azp": "app", "scope": "data.read",
                    "aud": "shiki-gateway", "iss": "https://kc/realms/shiki",
                    "exp": 9_999_999_999u64,
                }),
            );
            let err = verify_hs(&token, secret, &cfg()).unwrap_err();
            assert!(
                matches!(&err, GatewayError::Unauthenticated(m) if m.contains("sub")),
                "{sub:?}: {err:?}"
            );
        }
    }

    #[test]
    fn wrong_audience_is_rejected() {
        let secret = b"s";
        let token = make_token(
            secret,
            &serde_json::json!({
                "sub": "u", "azp": "app", "scope": "data.read",
                "aud": "someone-else", "iss": "https://kc/realms/shiki",
                "exp": 9_999_999_999u64,
            }),
        );
        assert!(verify_hs(&token, secret, &cfg()).is_err());
    }

    #[tokio::test]
    async fn resolver_trait_is_used() {
        // KeyResolver 経由の RS256 パスは resolve() を呼ぶ（HMAC 鍵では decode 失敗で Err）。
        let r = HsResolver(b"k".to_vec());
        let out = verify_gateway_token("not-a-jwt", &r, &cfg()).await;
        assert!(out.is_err());
    }
}
