//! RFC 8693 token-exchange（Task 9.7・B2 のユーザー代理維持）。
//!
//! B2（confidential）のサーバ関数がユーザー操作を代行するとき、ユーザーのアクセストークンを
//! **on-behalf-of** で交換し、`sub`＝ユーザーを維持したトークンを得る（アプリ単独権限へ昇格しない
//! ＝confused-deputy 防御・design §4.3）。ゲスト（サンドボックス）には交換後トークンすら渡さず、
//! host が HostCall 経由で Authorization を付与する（設計デフォルト・INV-1）。
//!
//! フォームパラメタの組み立ては純粋関数（[`exchange_params`]）に切り出し単体検証する。

use serde::Deserialize;

use crate::GatewayError;

/// RFC 8693 の grant_type。
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
/// subject_token_type（アクセストークン）。
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// 交換で得たトークン（sub＝ユーザーを維持）。
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangedToken {
    pub access_token: String,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// token-exchange のフォームパラメタを組み立てる（純粋関数・単体検証対象）。
///
/// `subject_token` = ユーザーのアクセストークン。`audience` = 呼び先（ゲートウェイ）。
/// client 資格情報（B2 の client_id/secret）は confidential 前提。
pub fn exchange_params<'a>(
    client_id: &'a str,
    client_secret: &'a str,
    subject_token: &'a str,
    audience: &'a str,
) -> Vec<(&'static str, &'a str)> {
    vec![
        ("grant_type", GRANT_TYPE),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("subject_token", subject_token),
        ("subject_token_type", ACCESS_TOKEN_TYPE),
        ("audience", audience),
        ("requested_token_type", ACCESS_TOKEN_TYPE),
    ]
}

/// ユーザーのトークンを B2 の代理トークンへ交換する（sub＝ユーザー維持・on-behalf-of）。
pub async fn exchange_for_user(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: &str,
    subject_token: &str,
    audience: &str,
) -> Result<ExchangedToken, GatewayError> {
    let params = exchange_params(client_id, client_secret, subject_token, audience);
    let resp = http
        .post(token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| GatewayError::Upstream(format!("token-exchange 送信: {e}")))?;
    if !resp.status().is_success() {
        return Err(GatewayError::Forbidden(format!(
            "token-exchange 拒否: {}",
            resp.status()
        )));
    }
    resp.json()
        .await
        .map_err(|e| GatewayError::Upstream(format!("token-exchange parse: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_rfc8693_shaped() {
        let p = exchange_params("app-b2", "secret", "user-tok", "shiki-gateway");
        let get = |k: &str| p.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v);
        assert_eq!(get("grant_type"), Some(GRANT_TYPE));
        assert_eq!(get("subject_token"), Some("user-tok"));
        assert_eq!(get("subject_token_type"), Some(ACCESS_TOKEN_TYPE));
        assert_eq!(get("audience"), Some("shiki-gateway"));
        assert_eq!(get("client_id"), Some("app-b2"));
        assert_eq!(get("client_secret"), Some("secret"));
    }
}
