//! Keycloak Account REST API クライアント（ユーザー自身の資格情報・SSO セッション）。
//!
//! 「設定 → セキュリティ / デバイス」画面の裏側。**ユーザー本人の access token** で
//! `{realm}/account/*` を叩く（provisioner のような管理者資格情報は使わない＝最小権限）。
//! Keycloak 側で `account` クライアントの `manage-account` ロール（realm の既定ロールに含まれる）
//! が要る。
//!
//! 資格情報の**追加・削除**はここでは行わない。パスキー登録・認証アプリ設定・パスワード変更・
//! 資格情報の削除は Application Initiated Action（`kc_action`）で Keycloak の画面へ送り、
//! 再認証（max_age）と確認を Keycloak に任せる（`routes/auth/action.rs`）。セッション Cookie を
//! 盗まれただけで MFA を外されないようにするため。
//!
//! [`crate::keycloak_admin`] と同じ薄いパターン（`&reqwest::Client`＋専用 error enum・リトライ無し）。

use serde::Deserialize;

use crate::config::AuthConfig;

#[derive(Debug, thiserror::Error)]
pub enum KeycloakAccountError {
    /// Account API への到達自体が失敗（ネットワーク等）。
    #[error("Keycloak Account API への接続に失敗: {0}")]
    Transport(String),
    /// Account API がエラー応答を返した。
    #[error("Keycloak Account API がエラー応答: status={status} body={body}")]
    Status { status: u16, body: String },
}

/// 資格情報の種類ごとのまとまり（`GET /account/credentials` の 1 要素・必要フィールドのみ）。
#[derive(Debug, Clone, Deserialize)]
pub struct KcCredentialContainer {
    /// `password` / `otp` / `webauthn` / `webauthn-passwordless` 等。
    #[serde(rename = "type")]
    pub kind: String,
    /// ユーザーが自分で削除できる種類か（パスワードは false）。
    #[serde(default)]
    pub removeable: bool,
    #[serde(default, rename = "userCredentialMetadatas")]
    pub metadatas: Vec<KcCredentialMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KcCredentialMetadata {
    pub credential: KcCredential,
}

/// 登録済みの資格情報 1 件（秘密値は Account API が返さない）。
#[derive(Debug, Clone, Deserialize)]
pub struct KcCredential {
    pub id: String,
    /// ユーザーが付けた名前（パスキーの登録名・認証アプリのデバイス名）。
    #[serde(default, rename = "userLabel")]
    pub user_label: Option<String>,
    /// 作成日時（epoch ミリ秒）。
    #[serde(default, rename = "createdDate")]
    pub created_date: Option<i64>,
}

/// 端末ごとの SSO セッション（`GET /account/sessions/devices` の 1 要素）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KcDevice {
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    /// 最終アクセス（epoch 秒）。
    #[serde(default)]
    pub last_access: i64,
    /// この端末に今使っているセッションが含まれるか。
    #[serde(default)]
    pub current: bool,
    #[serde(default)]
    pub mobile: bool,
    #[serde(default)]
    pub sessions: Vec<KcSession>,
}

/// SSO セッション 1 件。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KcSession {
    pub id: String,
    #[serde(default)]
    pub ip_address: Option<String>,
    /// 開始・最終アクセス・失効（epoch 秒）。
    #[serde(default)]
    pub started: i64,
    #[serde(default)]
    pub last_access: i64,
    #[serde(default)]
    pub expires: i64,
    #[serde(default)]
    pub browser: Option<String>,
    #[serde(default)]
    pub current: bool,
    #[serde(default)]
    pub clients: Vec<KcSessionClient>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KcSessionClient {
    pub client_id: String,
    #[serde(default)]
    pub client_name: Option<String>,
}

/// Account REST の薄いクライアント（1 リクエスト分の access token を握る）。
pub struct KeycloakAccount<'a> {
    http: &'a reqwest::Client,
    base: String,
    access_token: &'a str,
}

impl<'a> KeycloakAccount<'a> {
    pub fn new(http: &'a reqwest::Client, auth: &AuthConfig, access_token: &'a str) -> Self {
        Self {
            http,
            base: auth.account_base(),
            access_token,
        }
    }

    /// 自分の資格情報（種類ごと）。
    pub async fn credentials(&self) -> Result<Vec<KcCredentialContainer>, KeycloakAccountError> {
        self.get_json("/credentials").await
    }

    /// 自分の SSO セッションを端末ごとにまとめたもの。
    pub async fn devices(&self) -> Result<Vec<KcDevice>, KeycloakAccountError> {
        self.get_json("/sessions/devices").await
    }

    /// 自分の SSO セッション 1 件をログアウトさせる。
    /// Keycloak は該当セッションの RP へ backchannel logout を送り、BFF 側のセッションも消える。
    pub async fn delete_session(&self, session_id: &str) -> Result<(), KeycloakAccountError> {
        let mut url = self.url("/sessions")?;
        url.path_segments_mut()
            .map_err(|()| KeycloakAccountError::Transport("Account API の URL が不正".into()))?
            .push(session_id);
        self.send_delete(url).await
    }

    /// 今使っているセッション以外を全てログアウトさせる。
    pub async fn delete_other_sessions(&self) -> Result<(), KeycloakAccountError> {
        let mut url = self.url("/sessions")?;
        url.query_pairs_mut().append_pair("current", "false");
        self.send_delete(url).await
    }

    fn url(&self, path: &str) -> Result<reqwest::Url, KeycloakAccountError> {
        reqwest::Url::parse(&format!("{}{path}", self.base))
            .map_err(|e| KeycloakAccountError::Transport(format!("Account API の URL が不正: {e}")))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, KeycloakAccountError> {
        let resp = self
            .http
            .get(self.url(path)?)
            .bearer_auth(self.access_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| KeycloakAccountError::Transport(e.to_string()))?;
        let resp = ensure_success(resp).await?;
        resp.json::<T>()
            .await
            .map_err(|e| KeycloakAccountError::Transport(format!("応答の解釈に失敗: {e}")))
    }

    async fn send_delete(&self, url: reqwest::Url) -> Result<(), KeycloakAccountError> {
        let resp = self
            .http
            .delete(url)
            .bearer_auth(self.access_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| KeycloakAccountError::Transport(e.to_string()))?;
        ensure_success(resp).await.map(|_| ())
    }
}

async fn ensure_success(
    resp: reqwest::Response,
) -> Result<reqwest::Response, KeycloakAccountError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(KeycloakAccountError::Status {
        status: status.as_u16(),
        body,
    })
}
