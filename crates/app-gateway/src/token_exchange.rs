//! RFC 8693 token-exchange（Task 9.7・B2 のユーザー代理維持）。
//!
//! B2（confidential）のサーバ関数がユーザー操作を代行するとき、ユーザーのアクセストークンを
//! **on-behalf-of** で交換し、`sub`＝ユーザーを維持したトークンを得る（アプリ単独権限へ昇格しない
//! ＝confused-deputy 防御・design §4.3）。ゲスト（サンドボックス）には交換後トークンすら渡さず、
//! host が HostCall 経由で Authorization を付与する（設計デフォルト・INV-1）。
//!
//! **Keycloak 26.2 の標準の交換の決まり（BUG-182・実 Keycloak で確かめた）**:
//! - 交換を頼むクライアント（B2）が、元のトークンの `aud` に入っていること（自分に発行されたトークンを除く）。
//!   入っていないと 403 `access_denied`「Client is not within the token audience」。B1 のクライアントが同じアプリの
//!   B2 を `aud` に足す（[`crate::client_representation`] の `exchanger`）。別のアプリの B2 は交換できない。
//! - `audience` は realm のクライアントの client_id で、トークンの `aud` を**絞る**だけ（足さない）。ゲートウェイの
//!   `aud`（`gateway.audience`・既定 `shiki-gateway`）はクライアントではなくマッパーが足す値なので、渡すと 400 `invalid_client`
//!   「Audience not found」になる。ここでは渡さない（新しいトークンの `aud` は B2 のマッパーが足す）。
//! - `scope` を渡すと、B2 の既定のスコープに**任意のスコープを足す**（元のトークンに無い能力スコープでも足せる）。
//!   なので、元のトークンの能力スコープだけを求め、返ったトークンの能力スコープがその内にあることを確かめる
//!   （交換で得た権限が元より広がらない）。
//! - 無効にしたクライアント（アンインストール）の B1 のトークン・B2 の資格情報では交換できない。
//!
//! フォームパラメタの組み立ては純粋関数（[`exchange_params`]）に切り出し単体検証する。

use authz::CapabilityScope;
use serde::Deserialize;

use crate::GatewayError;

/// RFC 8693 の grant_type。
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
/// subject_token_type（アクセストークン）。
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// 交換を断られた・交換の結果を使えないときの文（理由・状態コード・Keycloak の文は返さない。ログにだけ残す）。
const EXCHANGE_REFUSED: &str =
    "アプリのサーバーの処理に、利用者の代わりに動く許可を渡せませんでした";

/// 交換で得たトークン（sub＝ユーザーを維持）。
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangedToken {
    pub access_token: String,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// token-exchange のフォームパラメタを組み立てる（純粋関数・単体検証対象）。
///
/// `subject_token` = ユーザーのアクセストークン。`scope` = 求める能力スコープ（空白区切り・空なら送らない）。
/// `audience` は送らない（[モジュール文書](self)）。client 資格情報（B2 の client_id/secret）は confidential 前提。
pub fn exchange_params<'a>(
    client_id: &'a str,
    client_secret: &'a str,
    subject_token: &'a str,
    scope: &'a str,
) -> Vec<(&'static str, &'a str)> {
    let mut params = vec![
        ("grant_type", GRANT_TYPE),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("subject_token", subject_token),
        ("subject_token_type", ACCESS_TOKEN_TYPE),
        ("requested_token_type", ACCESS_TOKEN_TYPE),
    ];
    if !scope.is_empty() {
        params.push(("scope", scope));
    }
    params
}

/// トークンの `scope` クレームの能力スコープ（署名は確かめない）。
///
/// 使うのは、ゲートウェイが検証済みの元のトークンと、トークンのエンドポイントから受け取ったばかりのトークンだけ。
/// ここで読んだ値は、交換で求めるスコープを**狭める**ことと、結果が広がっていないかを確かめることにしか使わない
/// （元のトークンの署名と期限は Keycloak も交換のときに確かめる）。
fn capability_scopes_of(token: &str) -> Result<Vec<CapabilityScope>, GatewayError> {
    #[derive(Deserialize)]
    struct ScopeClaim {
        #[serde(default)]
        scope: String,
    }
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.insecure_disable_signature_validation();
    validation.validate_aud = false;
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    let claims = jsonwebtoken::decode::<ScopeClaim>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(&[]),
        &validation,
    )
    .map_err(|e| GatewayError::Unauthenticated(format!("トークンを読めません: {e}")))?
    .claims;
    CapabilityScope::parse_scope_string(&claims.scope)
        .map_err(|e| GatewayError::Unauthenticated(format!("スコープ不正: {e}")))
}

/// Keycloak の誤りの応答（ログに残すだけ）。
#[derive(Debug, Default, Deserialize)]
struct OAuthErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// ユーザーのトークンを B2 の代理トークンへ交換する（sub＝ユーザー維持・on-behalf-of）。
///
/// 求める能力スコープは元のトークンのものだけで、返ったトークンの能力スコープがその内にあることを確かめる
/// （[モジュール文書](self)）。Keycloak が断ったとき・結果が広すぎるときは 403（理由はログにだけ残す）。
pub async fn exchange_for_user(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: &str,
    subject_token: &str,
) -> Result<ExchangedToken, GatewayError> {
    let wanted = capability_scopes_of(subject_token)?;
    let scope = wanted
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let params = exchange_params(client_id, client_secret, subject_token, &scope);
    let resp = http
        .post(token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| GatewayError::Upstream(format!("token-exchange 送信: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body: OAuthErrorBody = resp.json().await.unwrap_or_default();
        tracing::warn!(
            status = status.as_u16(),
            error = %body.error,
            description = %body.error_description,
            client_id,
            "トークンの交換を Keycloak が断りました"
        );
        return Err(GatewayError::Forbidden(EXCHANGE_REFUSED.into()));
    }
    let exchanged: ExchangedToken = resp
        .json()
        .await
        .map_err(|e| GatewayError::Upstream(format!("token-exchange parse: {e}")))?;
    let got = capability_scopes_of(&exchanged.access_token).map_err(|e| {
        tracing::error!(error = %e, client_id, "交換で得たトークンを読めません");
        GatewayError::Forbidden(EXCHANGE_REFUSED.into())
    })?;
    if let Some(extra) = got.iter().find(|s| !wanted.contains(s)) {
        // B2 のクライアントの既定のスコープに能力スコープが足されたなど（揃える処理が直す）。
        tracing::error!(
            scope = extra.as_str(),
            client_id,
            "交換で得たトークンに、元のトークンに無い能力スコープが付いています"
        );
        return Err(GatewayError::Forbidden(EXCHANGE_REFUSED.into()));
    }
    Ok(exchanged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    fn token_with_scope(scope: &str) -> String {
        encode(
            &Header::new(jsonwebtoken::Algorithm::HS256),
            &serde_json::json!({ "sub": "u", "scope": scope, "exp": 1 }),
            &EncodingKey::from_secret(b"k"),
        )
        .unwrap()
    }

    #[test]
    fn params_are_rfc8693_shaped_without_audience() {
        let p = exchange_params("app-b2", "secret", "user-tok", "data.read llm.invoke");
        let get = |k: &str| p.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v);
        assert_eq!(get("grant_type"), Some(GRANT_TYPE));
        assert_eq!(get("subject_token"), Some("user-tok"));
        assert_eq!(get("subject_token_type"), Some(ACCESS_TOKEN_TYPE));
        assert_eq!(get("requested_token_type"), Some(ACCESS_TOKEN_TYPE));
        assert_eq!(get("client_id"), Some("app-b2"));
        assert_eq!(get("client_secret"), Some("secret"));
        assert_eq!(get("scope"), Some("data.read llm.invoke"));
        // ゲートウェイの aud はクライアントではないので、audience に渡すと Keycloak が断る（BUG-182）。
        assert_eq!(get("audience"), None);
        // 能力スコープが無いトークンは、scope を送らない（B2 の既定のスコープだけになる）。
        let none = exchange_params("app-b2", "secret", "user-tok", "");
        assert!(none.iter().all(|(k, _)| *k != "scope"));
    }

    #[test]
    fn scopes_are_read_from_the_token() {
        assert_eq!(
            capability_scopes_of(&token_with_scope("openid data.read llm.invoke")).unwrap(),
            [CapabilityScope::DataRead, CapabilityScope::LlmInvoke]
        );
        assert!(capability_scopes_of(&token_with_scope(""))
            .unwrap()
            .is_empty());
        // 未知のスコープ・JWT でないものは読めない（交換しない）。
        assert!(capability_scopes_of(&token_with_scope("data.read bogus")).is_err());
        assert!(capability_scopes_of("not-a-jwt").is_err());
    }
}
