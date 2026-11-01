//! OAuth2 クライアント動的登録（Task 9.7・Keycloak 連携）。
//!
//! 各ミニアプリ = Keycloak の OAuth2 クライアント（レジストリ登録と連動・新しい認証基盤は作らない）。
//! - **B1 = public client**（authcode+PKCE 強制・secret なし・短命トークン）。
//! - **B2 = confidential client**（secret はサーバ側 secrets 保管・service account＋standard
//!   token-exchange 有効。ユーザー操作は token-exchange でユーザー代理を維持＝単独昇格しない）。
//!
//! クライアント表現（登録 body）の組み立ては純粋関数（[`client_representation`]）に切り出し
//! 単体検証する。実登録は Keycloak admin REST（provisioner service account）で行い IT で確認する。
//!
//! **登録の形はサーバーが持つ（BUG-172）。** 既にあるクライアント（再インストール・前の版で登録したもの）は、
//! [`OAuthClient::register`] と [`OAuthClient::repair`] が今の形に揃える: 有効に戻し、戻り先・属性・マッパーを
//! PUT で書き直し（Keycloak は protocolMappers を名前で突き合わせ、足し・直し・消す）、既定と任意のスコープを
//! 今の集合にする（スコープは PUT では変わらないので、クライアントのスコープの下位の資源で足し引きする）。
//! アプリのクライアントに管理者が手で足したマッパーやスコープは、揃えるときに消える。

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

    /// ミニアプリ用クライアントを登録する（冪等: 同じ client_id が既にあれば、有効に戻して今の形に揃える）。
    ///
    /// アンインストールはクライアントを無効にする（[`Self::set_enabled`]）。再インストールは同じ client_id を
    /// 登録し直すので、ここで有効に戻す（これまでは 409 を成功として扱い、無効のまま残っていた・BUG-172）。
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
        // 201=新規, 409=既存（再インストール・前の版で登録したもの）。それ以外は上流エラー。
        if status.as_u16() == 409 {
            let internal_id = self.find_internal_id(&token, client_id).await?;
            self.update_client(&token, &internal_id, &body).await?;
        } else if !status.is_success() {
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

    /// 既にあるクライアントを今の登録の形に揃える（前の版で登録したクライアントを直す・起動時の照合）。
    ///
    /// 有効なインストールのクライアントに使うので、有効に戻す。クライアントが無ければ作らずに `Ok(false)` を返す（confidential の secret を保管し直す経路がここに無いため。作り直しは
    /// 再インストールが行う）。secret は取得しない・作り直さない。
    pub async fn repair(
        &self,
        kind: ClientKind,
        client_id: &str,
        app_name: &str,
        redirect_uris: &[String],
    ) -> Result<bool, GatewayError> {
        let token = self.admin_token().await?;
        self.ensure_capability_scopes(&token).await?;
        let internal_id = match self.find_internal_id(&token, client_id).await {
            Ok(id) => id,
            Err(GatewayError::NotFound) => return Ok(false),
            Err(e) => return Err(e),
        };
        let body = client_representation(kind, client_id, app_name, redirect_uris);
        self.update_client(&token, &internal_id, &body).await?;
        Ok(true)
    }

    /// 既にあるクライアントを `body`（[`client_representation`]）の形に書き直す（有効・戻り先・属性・マッパー・
    /// スコープ）。
    async fn update_client(
        &self,
        token: &str,
        internal_id: &str,
        body: &serde_json::Value,
    ) -> Result<(), GatewayError> {
        // スコープは PUT では変わらない（Keycloak は表現の defaultClientScopes を読まない）ので、外して送り、
        // 下位の資源で足し引きする。protocolMappers は名前で突き合わされ、足し・直し・消しが行われる。
        let mut put = body.clone();
        if let Some(obj) = put.as_object_mut() {
            obj.remove("defaultClientScopes");
            obj.remove("optionalClientScopes");
        }
        let resp = self
            .http
            .put(format!("{}/clients/{internal_id}", self.admin_base))
            .bearer_auth(token)
            .json(&put)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("client 更新: {e}")))?;
        if !resp.status().is_success() {
            return Err(GatewayError::Upstream(format!(
                "client 更新応答: {}",
                resp.status()
            )));
        }
        let realm_scopes = self.realm_scope_ids(token).await?;
        for (kind, field) in [
            ("default-client-scopes", "defaultClientScopes"),
            ("optional-client-scopes", "optionalClientScopes"),
        ] {
            let want: Vec<&str> = body
                .get(field)
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            self.sync_client_scopes(token, internal_id, kind, &want, &realm_scopes)
                .await?;
        }
        Ok(())
    }

    /// realm の client-scope の名前 → id。
    async fn realm_scope_ids(
        &self,
        token: &str,
    ) -> Result<std::collections::HashMap<String, String>, GatewayError> {
        let list: Vec<KcScope> = self
            .http
            .get(format!("{}/client-scopes", self.admin_base))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("client-scope 一覧: {e}")))?
            .json()
            .await
            .map_err(|e| GatewayError::Upstream(format!("client-scope 一覧 parse: {e}")))?;
        Ok(list.into_iter().map(|s| (s.name, s.id)).collect())
    }

    /// クライアントの既定（`default-client-scopes`）か任意（`optional-client-scopes`）のスコープを `want` の
    /// 集合にする（足りないものを足し、余分なものを外す）。realm に無いスコープは足せないので warn に残す。
    async fn sync_client_scopes(
        &self,
        token: &str,
        internal_id: &str,
        kind: &str,
        want: &[&str],
        realm_scopes: &std::collections::HashMap<String, String>,
    ) -> Result<(), GatewayError> {
        let base = format!("{}/clients/{internal_id}/{kind}", self.admin_base);
        let have: Vec<KcScope> = self
            .http
            .get(&base)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| GatewayError::Upstream(format!("{kind} 取得: {e}")))?
            .json()
            .await
            .map_err(|e| GatewayError::Upstream(format!("{kind} parse: {e}")))?;
        let mut changes = Vec::new();
        for scope in have.iter().filter(|s| !want.contains(&s.name.as_str())) {
            changes.push(self.http.delete(format!("{base}/{}", scope.id)));
        }
        for name in want.iter().filter(|n| !have.iter().any(|s| s.name == **n)) {
            if let Some(id) = realm_scopes.get(*name) {
                changes.push(self.http.put(format!("{base}/{id}")));
            } else {
                tracing::warn!(scope = name, kind, "realm にスコープが無いため足せません");
            }
        }
        for change in changes {
            let resp = change
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| GatewayError::Upstream(format!("{kind} 更新: {e}")))?;
            if !resp.status().is_success() {
                return Err(GatewayError::Upstream(format!(
                    "{kind} 更新応答: {}",
                    resp.status()
                )));
            }
        }
        Ok(())
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

#[derive(Deserialize)]
struct KcScope {
    id: String,
    name: String,
}

/// アプリのクライアントの既定のスコープ（realm の client-scope の名前）。
///
/// `basic` はトークンに `sub`（呼び出す人）を載せる（Keycloak 25 以降）。ゲートウェイは `sub` の無いトークンを
/// 401 で断る。表現に `optionalClientScopes` を書くと Keycloak は realm の既定のスコープを付けないので、明示する
/// （BUG-172）。名前・メールアドレス（`profile`・`email`）と realm のロール（`roles`）は、アプリに渡すトークンに
/// 載せない（B1 ではトークンがアプリのコードに渡る。ゲートウェイはどれも読まない）。B2 には、サービスアカウントを
/// 有効にすると Keycloak が自分で付ける `service_account` も残す（揃えるたびに外して付け直されないように）。
pub fn default_client_scopes(kind: ClientKind) -> &'static [&'static str] {
    match kind {
        ClientKind::PublicPkce => &["basic"],
        ClientKind::Confidential => &["basic", "service_account"],
    }
}

/// トークンに載せるクレームのマッパー（ログインのクライアント shiki-web と同じ写し方）。
///
/// - `tenant`: 利用者の属性 `tenant`（multi テナントでゲートウェイがテナントを決める）。値は Keycloak の利用者に
///   束縛されていて、アプリは選べない。別のテナントの人がこのクライアントでトークンを得ても、そのテナントには
///   このアプリのインストールが無いので、ゲートウェイが断る（インストールはテナントの中で引く）。
/// - `groups`: 所属グループの完全なパス（`/tenants/<tenant>/<org>`・ゲートウェイが組織を決める）。
///
/// どちらもアクセストークンにだけ載せる（ID トークン・userinfo には載せない）。ロール（`roles`）は載せない
/// （ゲートウェイは ReBAC のロールを読み、トークンのロールを使わない。ロールは identity.read の同意で渡す）。
fn identity_mappers() -> [serde_json::Value; 2] {
    let only_access_token = |extra: serde_json::Value| {
        let mut config = serde_json::json!({
            "access.token.claim": "true",
            "id.token.claim": "false",
            "userinfo.token.claim": "false",
        });
        if let (Some(c), Some(e)) = (config.as_object_mut(), extra.as_object()) {
            c.extend(e.clone());
        }
        config
    };
    [
        serde_json::json!({
            "name": "tenant",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-usermodel-attribute-mapper",
            "config": only_access_token(serde_json::json!({
                "user.attribute": "tenant",
                "claim.name": "tenant",
                "jsonType.label": "String",
                "multivalued": "false",
            })),
        }),
        serde_json::json!({
            "name": "groups",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-group-membership-mapper",
            "config": only_access_token(serde_json::json!({
                "full.path": "true",
                "claim.name": "groups",
            })),
        }),
    ]
}

/// Keycloak client 登録 body を組み立てる（純粋関数・単体検証対象）。
///
/// B1: `publicClient=true` ＋ PKCE S256 強制 ＋ 短命 access token（5min）。
/// B2: `publicClient=false` ＋ `serviceAccountsEnabled=true` ＋ standard token-exchange 有効
///     （`standard.token.exchange.enabled=true`・RFC 8693／Keycloak 26.2）。
/// どちらも既定のスコープ（[`default_client_scopes`]）と、[`identity_mappers`] のマッパーを付ける。
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
    // ゲートウェイ audience を access token の aud に注入する（verify_gateway_token の
    // aud=shiki-gateway 検証を満たす）。これが無いとトークンが aud 不一致で弾かれる。
    let mut mappers = vec![serde_json::json!({
        "name": "shiki-gateway-audience",
        "protocol": "openid-connect",
        "protocolMapper": "oidc-audience-mapper",
        "config": {
            "included.custom.audience": GATEWAY_AUDIENCE,
            "access.token.claim": "true",
            "id.token.claim": "false",
        }
    })];
    mappers.extend(identity_mappers());
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
        "defaultClientScopes": default_client_scopes(kind),
        "optionalClientScopes": capability_scopes,
        "attributes": serde_json::Value::Object(attributes),
        "protocolMappers": mappers,
    })
}

/// 登録の形の指紋（起動時の照合で、今の形に揃え直すクライアントを選ぶ・BUG-172）。
///
/// B1・B2 の登録の形（アプリごとの名前と client_id を除く）と、B1 の戻り先から求める。登録の形を変えた版を
/// 出したとき・戻り先（web のオリジン）を変えたときに変わり、その後の最初の起動で、すべての有効なインストールの
/// クライアントを揃え直す。
pub fn client_spec_fingerprint(b1_redirect_uris: &[String]) -> String {
    let spec = serde_json::json!({
        "b1": client_representation(ClientKind::PublicPkce, "", "", b1_redirect_uris),
        "b2": client_representation(ClientKind::Confidential, "", "", &[]),
    });
    storage::content_address::sha256_hex(spec.to_string().as_bytes())
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
        assert_eq!(rep["redirectUris"][0], "https://apps.example/cb");
    }

    /// 既定のスコープ（sub を載せる basic）と、tenant・groups のマッパーを付ける。名前・メール・ロールは載せない
    /// （BUG-172。これが無いとトークンに sub と tenant が無く、ゲートウェイが 401 で断る）。
    #[test]
    fn both_kinds_carry_sub_tenant_and_groups_only() {
        for kind in [ClientKind::PublicPkce, ClientKind::Confidential] {
            let rep = client_representation(kind, "app-x", "経費", &[]);
            let defaults = rep["defaultClientScopes"].as_array().unwrap();
            assert_eq!(defaults[0], "basic");
            assert!(defaults
                .iter()
                .all(|s| matches!(s.as_str(), Some("basic" | "service_account"))));
            let optional = rep["optionalClientScopes"].as_array().unwrap();
            assert_eq!(optional.len(), CapabilityScope::ALL.len());
            let mappers = rep["protocolMappers"].as_array().unwrap();
            let names: Vec<&str> = mappers.iter().filter_map(|m| m["name"].as_str()).collect();
            assert_eq!(names, ["shiki-gateway-audience", "tenant", "groups"]);
            let tenant = &mappers[1];
            assert_eq!(tenant["protocolMapper"], "oidc-usermodel-attribute-mapper");
            // テナントは利用者の属性から写す（アプリが値を選べる固定のクレームにしない）。
            assert_eq!(tenant["config"]["user.attribute"], "tenant");
            assert_eq!(tenant["config"]["claim.name"], "tenant");
            assert_eq!(tenant["config"]["access.token.claim"], "true");
            assert_eq!(tenant["config"]["id.token.claim"], "false");
            let groups = &mappers[2];
            assert_eq!(groups["protocolMapper"], "oidc-group-membership-mapper");
            assert_eq!(groups["config"]["full.path"], "true");
            assert!(!names.contains(&"roles"));
            assert!(mappers
                .iter()
                .all(|m| m["protocolMapper"] != "oidc-hardcoded-claim-mapper"));
        }
    }

    /// 指紋は登録の形と戻り先で変わり、アプリの名前と client_id では変わらない。
    #[test]
    fn fingerprint_follows_shape_and_redirects() {
        let a = client_spec_fingerprint(&["http://localhost:3000/apps/oauth/callback".into()]);
        assert_eq!(
            a,
            client_spec_fingerprint(&["http://localhost:3000/apps/oauth/callback".into()])
        );
        assert_ne!(
            a,
            client_spec_fingerprint(&["https://shiki.example/apps/oauth/callback".into()])
        );
        assert_eq!(a.len(), 64);
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
