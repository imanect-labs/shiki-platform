//! OAuth2 クライアント動的登録（Task 9.7・Keycloak 連携）。
//!
//! 各ミニアプリ = Keycloak の OAuth2 クライアント（レジストリ登録と連動・新しい認証基盤は作らない）。
//! - **B1 = public client**（authcode+PKCE 強制・secret なし・短命トークン）。
//! - **B2 = confidential client**（secret はサーバ側 secrets 保管・service account＋standard
//!   token-exchange 有効。ユーザー操作は token-exchange でユーザー代理を維持＝単独昇格しない）。
//!
//! クライアント表現（登録 body）の組み立ては純粋関数（[`client_representation`]）に切り出し
//! 単体検証する。実登録は Keycloak admin REST（provisioner service account）で行い IT で確認する。

use authz::CapabilityScope;
use serde::Deserialize;

use crate::GatewayError;

/// 登録するクライアントの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// B1: ブラウザ配信（public・PKCE 強制・secret なし）。
    PublicPkce,
    /// B2: サーバ関数（confidential・service account・token-exchange 有効）。
    Confidential,
}

impl ClientKind {
    fn is_public(self) -> bool {
        matches!(self, ClientKind::PublicPkce)
    }
}

/// 登録済みクライアント（B2 のみ secret を持つ）。
#[derive(Debug, Clone)]
pub struct RegisteredClient {
    pub client_id: String,
    /// confidential のみ Some（サーバ側 secrets 保管・ゲストへは渡さない）。
    pub client_secret: Option<String>,
}

/// Keycloak admin REST 越しにクライアントを登録する OAuth 配線。
#[derive(Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    /// Keycloak admin REST の base（例 `http://keycloak:8080/admin/realms/shiki`）。
    admin_base: String,
    /// token エンドポイント（provisioner の client_credentials 取得用）。
    token_endpoint: String,
    provisioner_id: String,
    provisioner_secret: String,
}

impl OAuthClient {
    pub fn new(
        http: reqwest::Client,
        admin_base: String,
        token_endpoint: String,
        provisioner_id: String,
        provisioner_secret: String,
    ) -> Self {
        OAuthClient {
            http,
            admin_base,
            token_endpoint,
            provisioner_id,
            provisioner_secret,
        }
    }

    /// service account の access token（client_credentials）。
    async fn admin_token(&self) -> Result<String, GatewayError> {
        let resp = self
            .http
            .post(&self.token_endpoint)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.provisioner_id.as_str()),
                ("client_secret", self.provisioner_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("admin token 取得: {e}")))?;
        if !resp.status().is_success() {
            return Err(GatewayError::Upstream(format!(
                "admin token 応答: {}",
                resp.status()
            )));
        }
        #[derive(Deserialize)]
        struct Tok {
            access_token: String,
        }
        let tok: Tok = resp
            .json()
            .await
            .map_err(|e| GatewayError::Upstream(format!("admin token parse: {e}")))?;
        Ok(tok.access_token)
    }

    /// ミニアプリ用クライアントを登録する（冪等: 同一 client_id は 409 を許容し既存を返す）。
    ///
    /// confidential は登録後に生成 secret を取得して返す（サーバ側で secrets 保管する前提）。
    pub async fn register(
        &self,
        kind: ClientKind,
        client_id: &str,
        app_name: &str,
        redirect_uris: &[String],
    ) -> Result<RegisteredClient, GatewayError> {
        let token = self.admin_token().await?;
        // 能力スコープの realm client-scope を冪等作成する（optionalClientScopes 名前解決の前提）。
        // realm JSON のビルトインを汚さないため動的に用意する（Task 9.11/9.12）。
        self.ensure_capability_scopes(&token).await?;
        let body = client_representation(kind, client_id, app_name, redirect_uris);
        let resp = self
            .http
            .post(format!("{}/clients", self.admin_base))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("client 登録: {e}")))?;
        let status = resp.status();
        // 201=新規, 409=既存（冪等）。それ以外は上流エラー。
        if !status.is_success() && status.as_u16() != 409 {
            return Err(GatewayError::Upstream(format!("client 登録応答: {status}")));
        }
        let client_secret = if kind.is_public() {
            None
        } else {
            Some(self.fetch_secret(&token, client_id).await?)
        };
        Ok(RegisteredClient {
            client_id: client_id.to_string(),
            client_secret,
        })
    }

    /// 能力スコープを realm の client-scope として冪等作成する（既存 409 は許容）。
    ///
    /// `include.in.token.scope=true` で、要求された scope がアクセストークンの `scope`
    /// クレームに載る（二重ゲート③ granted ∩ token の token 側の素）。
    async fn ensure_capability_scopes(&self, token: &str) -> Result<(), GatewayError> {
        for cap in CapabilityScope::ALL {
            let body = serde_json::json!({
                "name": cap.as_str(),
                "protocol": "openid-connect",
                "attributes": {
                    "include.in.token.scope": "true",
                    "display.on.consent.screen": "false",
                },
            });
            let resp = self
                .http
                .post(format!("{}/client-scopes", self.admin_base))
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|e| GatewayError::Upstream(format!("client-scope 作成: {e}")))?;
            let status = resp.status();
            if !status.is_success() && status.as_u16() != 409 {
                return Err(GatewayError::Upstream(format!(
                    "client-scope 作成応答: {status}"
                )));
            }
        }
        Ok(())
    }

    /// クライアントの有効/無効を切り替える（アンインストール失効・補償に使う）。
    ///
    /// 無効化は削除ではなく enabled=false（client_id の再利用と監査可能性を保つ）。
    pub async fn set_enabled(&self, client_id: &str, enabled: bool) -> Result<(), GatewayError> {
        let token = self.admin_token().await?;
        let internal_id = self.find_internal_id(&token, client_id).await?;
        let resp = self
            .http
            .put(format!("{}/clients/{internal_id}", self.admin_base))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "clientId": client_id, "enabled": enabled }))
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("client 更新: {e}")))?;
        if !resp.status().is_success() {
            return Err(GatewayError::Upstream(format!(
                "client 更新応答: {}",
                resp.status()
            )));
        }
        Ok(())
    }

    /// client_id → Keycloak 内部 uuid。
    async fn find_internal_id(&self, token: &str, client_id: &str) -> Result<String, GatewayError> {
        let list: Vec<KcClient> = self
            .http
            .get(format!("{}/clients", self.admin_base))
            .query(&[("clientId", client_id)])
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("client 検索: {e}")))?
            .json()
            .await
            .map_err(|e| GatewayError::Upstream(format!("client 検索 parse: {e}")))?;
        list.into_iter()
            .next()
            .map(|c| c.id)
            .ok_or(GatewayError::NotFound)
    }

    /// confidential client の生成 secret を取得する（内部 uuid を引いてから secret を GET）。
    async fn fetch_secret(&self, token: &str, client_id: &str) -> Result<String, GatewayError> {
        let internal_id = self.find_internal_id(token, client_id).await?;
        #[derive(Deserialize)]
        struct Secret {
            value: String,
        }
        let secret: Secret = self
            .http
            .get(format!(
                "{}/clients/{internal_id}/client-secret",
                self.admin_base
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("secret 取得: {e}")))?
            .json()
            .await
            .map_err(|e| GatewayError::Upstream(format!("secret parse: {e}")))?;
        Ok(secret.value)
    }
}

#[derive(Deserialize)]
struct KcClient {
    id: String,
}

/// Keycloak client 登録 body を組み立てる（純粋関数・単体検証対象）。
///
/// B1: `publicClient=true` ＋ PKCE S256 強制 ＋ 短命 access token（5min）。
/// B2: `publicClient=false` ＋ `serviceAccountsEnabled=true` ＋ standard token-exchange 有効
///     （`standard.token.exchange.enabled=true`・RFC 8693／Keycloak 26.2）。
pub fn client_representation(
    kind: ClientKind,
    client_id: &str,
    app_name: &str,
    redirect_uris: &[String],
) -> serde_json::Value {
    let public = kind.is_public();
    let mut attributes = serde_json::Map::new();
    if public {
        // PKCE S256 を強制（public は secret を持たない）。access token を短命に。
        attributes.insert(
            "pkce.code.challenge.method".into(),
            serde_json::json!("S256"),
        );
        attributes.insert(
            "access.token.lifespan".into(),
            serde_json::json!("300"), // 5 分
        );
    } else {
        // standard token-exchange（RFC 8693・on-behalf-of）を有効化。
        attributes.insert(
            "standard.token.exchange.enabled".into(),
            serde_json::json!("true"),
        );
    }
    // 能力スコープ（CapabilityScope 閉集合）を optional client scope として付与する。
    // アプリは authorize の `scope=` で必要分だけ要求し、ゲートウェイが granted ∩ token を
    // 強制する（realm 側の clientScopes 定義は deploy/keycloak/shiki-realm.json）。
    let capability_scopes: Vec<&str> = CapabilityScope::ALL.iter().map(|s| s.as_str()).collect();
    serde_json::json!({
        "clientId": client_id,
        "name": app_name,
        "enabled": true,
        "protocol": "openid-connect",
        "publicClient": public,
        "standardFlowEnabled": public,      // B1 は authcode フロー
        "serviceAccountsEnabled": !public,  // B2 は service account（自動化）
        "directAccessGrantsEnabled": false, // password grant は無効
        "redirectUris": redirect_uris,
        // ブラウザ（B1）から token エンドポイントを直接叩けるように CORS を許可
        // （"+" = redirectUris のオリジン）。
        "webOrigins": ["+"],
        "optionalClientScopes": capability_scopes,
        "attributes": serde_json::Value::Object(attributes),
        // ゲートウェイ audience を access token の aud に注入する（verify_gateway_token の
        // aud=shiki-gateway 検証を満たす）。これが無いとトークンが aud 不一致で弾かれる。
        "protocolMappers": [{
            "name": "shiki-gateway-audience",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-audience-mapper",
            "config": {
                "included.custom.audience": GATEWAY_AUDIENCE,
                "access.token.claim": "true",
                "id.token.claim": "false",
            }
        }],
    })
}

/// ゲートウェイトークンの audience（verify_gateway_token の既定 aud と一致させる）。
const GATEWAY_AUDIENCE: &str = "shiki-gateway";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b1_public_pkce_representation() {
        let rep = client_representation(
            ClientKind::PublicPkce,
            "app-b1",
            "経費",
            &["https://apps.example/cb".into()],
        );
        assert_eq!(rep["publicClient"], true);
        assert_eq!(rep["standardFlowEnabled"], true);
        assert_eq!(rep["serviceAccountsEnabled"], false);
        assert_eq!(rep["directAccessGrantsEnabled"], false);
        assert_eq!(rep["attributes"]["pkce.code.challenge.method"], "S256");
        // ゲートウェイ audience マッパーが付く（token の aud=shiki-gateway 検証を満たす）。
        assert_eq!(
            rep["protocolMappers"][0]["protocolMapper"],
            "oidc-audience-mapper"
        );
        assert_eq!(
            rep["protocolMappers"][0]["config"]["included.custom.audience"],
            "shiki-gateway"
        );
    }

    #[test]
    fn b2_confidential_token_exchange_representation() {
        let rep = client_representation(ClientKind::Confidential, "app-b2", "経費", &[]);
        assert_eq!(rep["publicClient"], false);
        assert_eq!(rep["serviceAccountsEnabled"], true);
        assert_eq!(rep["standardFlowEnabled"], false);
        assert_eq!(rep["attributes"]["standard.token.exchange.enabled"], "true");
        // public 専用の PKCE 属性は付かない。
        assert!(rep["attributes"]
            .get("pkce.code.challenge.method")
            .is_none());
    }
}
