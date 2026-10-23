//! Sign in with ChatGPT（オープンソース向けの dynamic client）の OAuth 部品。
//!
//! 正本: <https://developers.openai.com/siwc/token-sharing-open-source/sign-in>。
//! PKCE（S256）＋ `state` ＋ `nonce`、戻り先は `127.0.0.1` の loopback 固定。初回は
//! `client_id=dynamic_agent_client` で登録し、戻り先のクエリで発行されたクライアント ID を受け取る。
//! I/O（HTTP）は呼び出し側の [`super::ChatGptAccount`] がここの関数を通して行う。

use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{ChatGptError, ChatGptModel};

/// 初回登録に使うクライアント ID（交換・リフレッシュには使わない）。
pub(super) const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
/// 戻り先。scheme・host・path は固定（`localhost` に置き換えない）。サーバ側では待ち受けず、
/// ブラウザが戻された URL を利用者に貼ってもらう。
pub(super) const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
/// 識別（`openid profile email`）＋プラン利用（`offline_access resource.invoke chatgpt.tokens.use.direct`）。
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// プラン利用を許可されたかを示すスコープ（ID トークンだけでは利用を許可されない）。
pub(super) const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// トークンの宛先（`resource`）。
pub(super) const RESOURCE: &str = "https://api.openai.com/v1";
/// 同意画面に出すアプリ名（表示用で、識別には使われない）。
const AGENT_NAME: &str = "Shiki";
/// ID トークンの発行者。
const ISSUER: &str = "https://auth.openai.com";
/// 認証サーバ・モデル一覧への 1 回の呼び出しの上限。
pub(super) const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// リフレッシュトークンが使えなくなったことを示すエラーコード（再ログインが要る）。
const TERMINAL_REFRESH_CODES: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// URL 安全な乱数文字列（32 バイト）。PKCE verifier・state・nonce に使う。
pub(super) fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE の `code_challenge`（S256）。
pub(super) fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// 認可 URL の材料。
pub(super) struct AuthorizeParams<'a> {
    pub client_id: &'a str,
    pub host_id: &'a str,
    /// 再ログイン（発行済みクライアント ID）のときだけ付ける。
    pub id_token_hint: Option<&'a str>,
    pub state: &'a str,
    pub nonce: &'a str,
    pub verifier: &'a str,
}

/// 認可 URL を組む（値はすべて URL エンコードする）。
pub(super) fn authorize_url(
    auth_base: &str,
    p: &AuthorizeParams<'_>,
) -> Result<String, ChatGptError> {
    let challenge = pkce_challenge(p.verifier);
    let mut url = reqwest::Url::parse(&format!("{auth_base}/api/accounts/authorize"))
        .map_err(|e| ChatGptError::Upstream(format!("認証サーバの URL が不正です: {e}")))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", p.client_id);
        if p.client_id == DYNAMIC_CLIENT_ID {
            // 表示名は初回登録のときだけ送る。
            q.append_pair("agent_name_hint", AGENT_NAME);
        }
        q.append_pair("ext_agent_host_id", p.host_id);
        if let Some(hint) = p.id_token_hint {
            q.append_pair("id_token_hint", hint);
        }
        q.append_pair("response_type", "code")
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("scope", SCOPES)
            .append_pair("resource", RESOURCE)
            .append_pair("state", p.state)
            .append_pair("nonce", p.nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge);
    }
    Ok(url.into())
}

/// 戻り先 URL から取り出した値。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Callback {
    pub code: String,
    pub state: String,
    /// 新規登録で発行されたクライアント ID（再ログインでは無いことがある）。
    pub client_id: Option<String>,
}

/// 利用者が貼った戻り先 URL を読む。
///
/// 値を取り出すだけで、URL へアクセスはしない。戻り先以外（別ホスト・別パス）の URL は
/// 貼り間違いとして弾く。同意を断った戻り（`error=access_denied`）は、その旨を返して交換しない。
pub(super) fn parse_callback(pasted: &str) -> Result<Callback, ChatGptError> {
    let invalid = |m: &str| ChatGptError::Invalid(m.to_string());
    let url = reqwest::Url::parse(pasted.trim()).map_err(|_| {
        invalid(
            "貼り付けた内容が URL として読めません。アドレスバーの URL をそのまま貼ってください。",
        )
    })?;
    if url.host_str() != Some("127.0.0.1") || url.path() != "/auth/callback" {
        return Err(invalid(
            "ChatGPT のログイン後に開いた `http://127.0.0.1:…/auth/callback?…` の URL を貼ってください。",
        ));
    }
    let get = |k: &str| {
        url.query_pairs()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.is_empty())
    };
    if let Some(error) = get("error") {
        return Err(if error == "access_denied" {
            invalid("ChatGPT での許可がキャンセルされました。")
        } else {
            ChatGptError::Invalid(format!("ChatGPT のログインに失敗しました（{error}）。"))
        });
    }
    let code = get("code").ok_or_else(|| invalid("URL に認可コード（code）がありません。"))?;
    let state = get("state").ok_or_else(|| invalid("URL に state がありません。"))?;
    Ok(Callback {
        code,
        state,
        client_id: get("client_id"),
    })
}

/// トークンエンドポイントの応答。
#[derive(Deserialize)]
pub(super) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    pub expires_in: i64,
    /// 許可されたスコープ（空白区切り）。
    #[serde(default)]
    pub scope: Option<String>,
    /// これより前にはリフレッシュしない（形が文書に無いので、秒の数値と RFC 3339 の両方を受ける）。
    #[serde(default)]
    pub earliest_refresh_at: Option<Value>,
}

impl TokenResponse {
    /// プラン利用のスコープを許可されたか。
    pub(super) fn grants_plan_usage(&self) -> bool {
        self.scope
            .as_deref()
            .is_some_and(|s| s.split_whitespace().any(|x| x == PLAN_SCOPE))
    }

    pub(super) fn expires_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        now + chrono::Duration::seconds(self.expires_in.max(0))
    }

    pub(super) fn earliest_refresh(&self) -> Option<DateTime<Utc>> {
        match self.earliest_refresh_at.as_ref()? {
            Value::Number(n) => DateTime::from_timestamp(n.as_i64()?, 0),
            Value::String(s) => DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|t| t.with_timezone(&Utc)),
            _ => None,
        }
    }
}

/// トークンエンドポイントの失敗。
#[derive(Debug)]
pub(super) enum TokenError {
    /// 到達できない・5xx（一時障害。資格情報は消さない）。
    Transport(String),
    /// 4xx。`code` は応答のエラーコード（読めれば）。
    Rejected { status: u16, code: Option<String> },
}

impl TokenError {
    /// リフレッシュトークンが使えなくなった（再ログインが要る）か。
    pub(super) fn is_terminal(&self) -> bool {
        matches!(self, TokenError::Rejected { code: Some(c), .. } if TERMINAL_REFRESH_CODES.contains(&c.as_str()))
    }
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Transport(e) => write!(f, "{e}"),
            TokenError::Rejected { status, code } => {
                write!(
                    f,
                    "HTTP {status} ({})",
                    code.as_deref().unwrap_or("unknown")
                )
            }
        }
    }
}

/// エラー応答のコード（`{"error":"invalid_grant"}` と `{"error":{"code":"…"}}` の両方を読む）。
fn error_code(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let e = v.get("error")?;
    e.as_str()
        .or_else(|| e.get("code").and_then(Value::as_str))
        .map(str::to_string)
}

/// フォームを POST してトークン応答を読む。
async fn post_token(
    http: &reqwest::Client,
    auth_base: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, TokenError> {
    let resp = http
        .post(format!("{auth_base}/api/accounts/oauth/token"))
        .timeout(HTTP_TIMEOUT)
        .form(form)
        .send()
        .await
        .map_err(|e| TokenError::Transport(e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| TokenError::Transport(e.to_string()))?;
    if status.is_client_error() {
        // 応答本文はトークン類を含み得るのでログに出さず、コードだけ残す。
        return Err(TokenError::Rejected {
            status: status.as_u16(),
            code: error_code(&body),
        });
    }
    if !status.is_success() {
        return Err(TokenError::Transport(format!("HTTP {status}")));
    }
    serde_json::from_str(&body).map_err(|e| TokenError::Transport(format!("token response: {e}")))
}

/// 認可コードをトークンへ交換する（戻り先・resource は認可時と同じ値）。
pub(super) async fn exchange_code(
    http: &reqwest::Client,
    auth_base: &str,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse, TokenError> {
    post_token(
        http,
        auth_base,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT_URI),
            ("resource", RESOURCE),
        ],
    )
    .await
}

/// リフレッシュする（`scope` は送らない。応答のリフレッシュトークンで置き換える）。
pub(super) async fn refresh(
    http: &reqwest::Client,
    auth_base: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse, TokenError> {
    post_token(
        http,
        auth_base,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("resource", RESOURCE),
        ],
    )
    .await
}

/// リフレッシュトークンを失効させる（best-effort）。失効エンドポイントは discovery から引き、
/// 認証サーバと同じオリジンのときだけ送る。
pub(super) async fn revoke(
    http: &reqwest::Client,
    auth_base: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<(), String> {
    let discovery: Value = http
        .get(format!("{auth_base}/.well-known/openid-configuration"))
        .timeout(HTTP_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let endpoint = discovery
        .get("revocation_endpoint")
        .and_then(Value::as_str)
        .filter(|u| same_origin(u, auth_base))
        .ok_or("revocation_endpoint がありません")?;
    http.post(endpoint)
        .timeout(HTTP_TIMEOUT)
        .form(&[
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", client_id),
        ])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn same_origin(url: &str, base: &str) -> bool {
    match (reqwest::Url::parse(url), reqwest::Url::parse(base)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

/// ID トークンから読む値。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct IdClaims {
    pub sub: String,
    pub email: Option<String>,
}

/// ID トークンの発行者・宛先・期限・nonce を確かめて、アカウントの識別を読む。
///
/// 署名は検証しない。トークンエンドポイントから TLS で直接受け取った ID トークンは、
/// TLS のサーバ検証で発行者を確かめられる（OIDC Core 3.1.3.7 (6)）。ここで使うのは表示と
/// 再ログインの照合だけで、shiki の認証には使わない。
pub(super) fn id_claims(
    id_token: &str,
    client_id: &str,
    nonce: &str,
    now: DateTime<Utc>,
) -> Result<IdClaims, String> {
    let payload = id_token.split('.').nth(1).ok_or("形式が不正")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if v.get("iss").and_then(Value::as_str) != Some(ISSUER) {
        return Err("iss が一致しません".into());
    }
    let aud_ok = match v.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(a)) => a.iter().any(|x| x.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err("aud が一致しません".into());
    }
    if v.get("exp")
        .and_then(Value::as_i64)
        .is_none_or(|exp| exp <= now.timestamp())
    {
        return Err("期限切れです".into());
    }
    if v.get("nonce").and_then(Value::as_str) != Some(nonce) {
        return Err("nonce が一致しません".into());
    }
    let sub = v
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("sub がありません")?;
    Ok(IdClaims {
        sub: sub.to_string(),
        email: v.get("email").and_then(Value::as_str).map(str::to_string),
    })
}

/// このアカウントで選べるモデル（`visibility == "list"` のもの）。
pub(super) async fn list_models(
    http: &reqwest::Client,
    api_base: &str,
    access_token: &str,
) -> Result<Vec<ChatGptModel>, String> {
    let v: Value = http
        .get(format!("{api_base}/models"))
        .timeout(HTTP_TIMEOUT)
        .bearer_auth(access_token)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(models_from(&v))
}

fn models_from(v: &Value) -> Vec<ChatGptModel> {
    v.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|m| {
            let slug = m.get("slug").and_then(Value::as_str)?;
            let display_name = m
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or(slug);
            Some(ChatGptModel {
                slug: slug.to_string(),
                display_name: display_name.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn query(url: &str) -> std::collections::HashMap<String, String> {
        reqwest::Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn first_sign_in_registers_dynamically_with_plan_scopes() {
        let url = authorize_url(
            "https://auth.openai.com",
            &AuthorizeParams {
                client_id: DYNAMIC_CLIENT_ID,
                host_id: "urn:uuid:h",
                id_token_hint: None,
                state: "s",
                nonce: "n",
                verifier: "v",
            },
        )
        .unwrap();
        assert!(url.starts_with("https://auth.openai.com/api/accounts/authorize?"));
        let q = query(&url);
        assert_eq!(q["client_id"], DYNAMIC_CLIENT_ID);
        assert_eq!(q["agent_name_hint"], AGENT_NAME);
        assert_eq!(q["ext_agent_host_id"], "urn:uuid:h");
        assert_eq!(q["redirect_uri"], REDIRECT_URI);
        assert_eq!(q["resource"], RESOURCE);
        assert!(q["scope"].split(' ').any(|s| s == PLAN_SCOPE));
        assert!(q["scope"].split(' ').any(|s| s == "offline_access"));
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], pkce_challenge("v"));
        assert!(!q.contains_key("id_token_hint"));
    }

    #[test]
    fn re_sign_in_uses_the_issued_client_without_the_name_hint() {
        let q = query(
            &authorize_url(
                "https://auth.openai.com",
                &AuthorizeParams {
                    client_id: "oaiapp_1",
                    host_id: "urn:uuid:h",
                    id_token_hint: Some("idt"),
                    state: "s",
                    nonce: "n",
                    verifier: "v",
                },
            )
            .unwrap(),
        );
        assert_eq!(q["client_id"], "oaiapp_1");
        assert_eq!(q["id_token_hint"], "idt");
        assert!(!q.contains_key("agent_name_hint"));
    }

    #[test]
    fn callback_reads_code_state_and_issued_client() {
        let cb = parse_callback(
            " http://127.0.0.1:1455/auth/callback?code=c1&scope=openid&state=s1&client_id=oaiapp_1 ",
        )
        .unwrap();
        assert_eq!(
            cb,
            Callback {
                code: "c1".into(),
                state: "s1".into(),
                client_id: Some("oaiapp_1".into())
            }
        );
        // 再ログインでは client_id が無いことがある。
        let cb = parse_callback("http://127.0.0.1:54321/auth/callback?code=c&state=s").unwrap();
        assert!(cb.client_id.is_none());
    }

    #[test]
    fn callback_rejects_other_urls_denials_and_missing_values() {
        for bad in [
            "not a url",
            "https://evil.example/auth/callback?code=c&state=s",
            "http://localhost:1455/auth/callback?code=c&state=s",
            "http://127.0.0.1:1455/callback?code=c&state=s",
            "http://127.0.0.1:1455/auth/callback?state=s",
            "http://127.0.0.1:1455/auth/callback?code=c",
            "http://127.0.0.1:1455/auth/callback?error=access_denied&state=s",
        ] {
            assert!(
                matches!(parse_callback(bad), Err(ChatGptError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn plan_usage_requires_the_direct_scope() {
        let mut t: TokenResponse = serde_json::from_value(json!({
            "access_token": "a", "expires_in": 3600,
            "scope": "openid email offline_access resource.invoke chatgpt.tokens.use.direct",
            "earliest_refresh_at": 1_700_000_000
        }))
        .unwrap();
        assert!(t.grants_plan_usage());
        assert_eq!(t.earliest_refresh().unwrap().timestamp(), 1_700_000_000);
        t.scope = Some("openid email".into());
        assert!(!t.grants_plan_usage());
        t.earliest_refresh_at = Some(json!("2026-10-23T00:00:00Z"));
        assert!(t.earliest_refresh().is_some());
    }

    #[test]
    fn only_known_refresh_codes_are_terminal() {
        let rejected = |code: Option<&str>| TokenError::Rejected {
            status: 400,
            code: code.map(str::to_string),
        };
        assert!(rejected(Some("refresh_token_reused")).is_terminal());
        assert!(rejected(Some("invalid_grant")).is_terminal());
        // 設定不備や一時障害では資格情報を消さない。
        assert!(!rejected(Some("invalid_client")).is_terminal());
        assert!(!rejected(None).is_terminal());
        assert!(!TokenError::Transport("x".into()).is_terminal());
        assert_eq!(
            error_code(r#"{"error":"invalid_grant"}"#).as_deref(),
            Some("invalid_grant")
        );
        assert_eq!(
            error_code(r#"{"error":{"code":"refresh_token_expired"}}"#).as_deref(),
            Some("refresh_token_expired")
        );
    }

    fn id_token(claims: &Value) -> String {
        let enc = |v: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
        };
        format!("{}.{}.sig", enc(&json!({"alg": "RS256"})), enc(claims))
    }

    #[test]
    fn id_token_claims_are_checked() {
        let now = Utc::now();
        let good = json!({
            "iss": ISSUER, "aud": "oaiapp_1", "exp": now.timestamp() + 60,
            "nonce": "n", "sub": "user-1", "email": "a@example.com"
        });
        assert_eq!(
            id_claims(&id_token(&good), "oaiapp_1", "n", now).unwrap(),
            IdClaims {
                sub: "user-1".into(),
                email: Some("a@example.com".into())
            }
        );
        for (key, value) in [
            ("iss", json!("https://evil.example")),
            ("aud", json!("oaiapp_other")),
            ("exp", json!(now.timestamp() - 1)),
            ("nonce", json!("other")),
            ("sub", json!("")),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(
                id_claims(&id_token(&bad), "oaiapp_1", "n", now).is_err(),
                "{key}"
            );
        }
        let mut aud_list = good.clone();
        aud_list["aud"] = json!(["x", "oaiapp_1"]);
        assert!(id_claims(&id_token(&aud_list), "oaiapp_1", "n", now).is_ok());
        assert!(id_claims("garbage", "oaiapp_1", "n", now).is_err());
    }

    #[test]
    fn model_list_keeps_listed_models_only() {
        let v = json!({"models": [
            {"slug": "gpt-6.1-sol", "display_name": "GPT-6.1 Sol", "visibility": "list"},
            {"slug": "hidden", "visibility": "hide"},
            {"slug": "plain", "visibility": "list"},
        ]});
        let got = models_from(&v);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].display_name, "GPT-6.1 Sol");
        assert_eq!(got[1].display_name, "plain");
        assert!(models_from(&json!({})).is_empty());
    }

    #[test]
    fn revocation_only_goes_to_the_auth_origin() {
        assert!(same_origin(
            "https://auth.openai.com/oauth/revoke",
            "https://auth.openai.com"
        ));
        assert!(!same_origin(
            "https://evil.example/revoke",
            "https://auth.openai.com"
        ));
    }
}
