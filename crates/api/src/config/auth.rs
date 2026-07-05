//! 認証（OIDC/Keycloak）設定: [`AuthConfig`] / [`SessionConfig`] / [`Tenancy`]。
//!
//! エンドポイント導出（authorize/token/end-session/JWKS/admin base）と
//! プロビジョナ資格情報の解決をここに閉じる。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// OIDC issuer（Keycloak realm URL。必須。トークンの `iss` 検証値であり、
    /// ブラウザがログインで到達する**公開 URL**でもある）。
    pub issuer: String,
    /// サーバ側 OIDC 呼び出し（token 交換・end-session backchannel）に使う**内部 base URL**。
    /// 未指定なら `issuer` を使う。compose では公開 URL（issuer）がコンテナ内から引けないため、
    /// `http://keycloak:8080/realms/shiki` のような内部 URL を指定する（JWKS の内部 URL 指定と同様）。
    pub internal_base_url: Option<String>,
    /// JWKS エンドポイント。未指定なら `internal_base_url`（無ければ issuer）から導出する。
    pub jwks_uri: Option<String>,
    /// アクセストークンの `aud` 検証値（必須）。
    pub audience: String,
    /// JWKS キャッシュの TTL（秒）。
    pub jwks_ttl_secs: u64,
    /// BFF（confidential client）の client_id。既定 `"shiki-web"`。
    pub client_id: String,
    /// BFF（confidential client）の client_secret。BFF はサーバ側でのみ保持する。
    pub client_secret: Option<String>,
    /// OIDC code フローのブラウザ向け redirect_uri（callback の登録 URL）。
    /// 既定はローカル開発の `http://localhost:3000/auth/callback`（Next rewrites で同一オリジン）。
    pub redirect_uri: String,
    /// ログアウト後のブラウザ向けリダイレクト先。既定 `http://localhost:3000/`。
    pub post_logout_redirect_uri: String,
    /// 要求スコープ（スペース区切り）。既定 `"openid profile"`。
    pub scopes: String,
    /// テナンシーモード（`single`=オンプレ/cell・`multi`=SaaS）。既定 `single`。
    /// `resolve_tenant_id` の解決戦略を分岐し、SaaS では claim 欠落を fail-closed にする。
    pub tenancy: Tenancy,
    /// `single` モードのテナント固定値（案C）。オンプレ/cell のシングルテナントで使う。
    /// 既定 `"default"`。`multi` モードでは使わず claim `tenant` を必須にする（案A）。
    pub tenant_id: Option<String>,
    /// テナント・プロビジョニング用 client（SAAS.2 / #87）。service account
    /// （client_credentials）で Keycloak admin REST を叩き、かつ `/admin/*` の呼び出し
    /// トークンの `azp` 照合値になる。`provisioner_client_secret` と**両方**揃った時のみ
    /// admin ルートが有効化される（未設定なら fail-closed でルート自体を組み込まない）。
    #[serde(default)]
    pub provisioner_client_id: Option<String>,
    /// プロビジョニング client の secret（サーバ側でのみ保持）。
    #[serde(default)]
    pub provisioner_client_secret: Option<String>,
    /// Keycloak admin REST の base URL 上書き（例 `http://keycloak:8080/admin/realms/shiki`）。
    /// 未指定なら `internal_base_url`（無ければ issuer）の `/realms/{realm}` から導出する。
    #[serde(default)]
    pub admin_base_url: Option<String>,
}

/// BFF セッション（オパーク Cookie + Redis）の設定。
///
/// ブラウザにはトークンを置かず、`session.cookie_name` の不透明セッション ID のみを渡す。
/// セッション本体（principal/claims/OIDC token/expiry）は Redis に `tenant_id` スコープで保持する。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Redis 接続 URL（例 `redis://redis:6379`）。
    pub redis_url: String,
    /// セッション TTL（秒）。既定 86400（24h）。
    pub ttl_secs: u64,
    /// Cookie の `Secure` 属性。本番(HTTPS)は true、ローカル HTTP 開発のみ false。既定 true。
    pub secure: bool,
    /// access token の期限が残りこの秒数を切ったらサーバ側で refresh する閾値。既定 60。
    pub refresh_leeway_secs: i64,
}

impl AuthConfig {
    /// サーバ側 OIDC 呼び出しの base URL（`internal_base_url` 優先・末尾スラッシュ除去）。
    fn backchannel_base(&self) -> String {
        self.internal_base_url
            .as_deref()
            .unwrap_or(&self.issuer)
            .trim_end_matches('/')
            .to_string()
    }

    /// ブラウザ向け authorize エンドポイント（公開 issuer 由来）。
    pub fn authorize_endpoint(&self) -> String {
        format!(
            "{}/protocol/openid-connect/auth",
            self.issuer.trim_end_matches('/')
        )
    }

    /// サーバ側 token エンドポイント（内部 base 由来。code 交換・refresh で使う）。
    pub fn token_endpoint(&self) -> String {
        format!("{}/protocol/openid-connect/token", self.backchannel_base())
    }

    /// ブラウザ向け end-session エンドポイント（公開 issuer 由来）。
    pub fn end_session_endpoint(&self) -> String {
        format!(
            "{}/protocol/openid-connect/logout",
            self.issuer.trim_end_matches('/')
        )
    }

    /// Keycloak admin REST の base URL（SAAS.2 プロビジョニング）。
    ///
    /// `admin_base_url` 上書きが無ければ、内部 base（例 `http://keycloak:8080/realms/shiki`）
    /// を `{root}/admin/realms/{realm}` へ写して導出する。realm セグメントが見つからない
    /// 形式（プロキシ等で realm パスを含まない URL）は `None`（admin ルート無効化に倒す）。
    pub fn admin_base(&self) -> Option<String> {
        if let Some(explicit) = &self.admin_base_url {
            return Some(explicit.trim_end_matches('/').to_string());
        }
        let base = self.backchannel_base();
        let (root, realm) = base.split_once("/realms/")?;
        Some(format!("{root}/admin/realms/{realm}"))
    }

    /// プロビジョニング client の資格情報（id, secret）。両方揃った時のみ `Some`。
    pub fn provisioner_credentials(&self) -> Option<(&str, &str)> {
        match (
            self.provisioner_client_id.as_deref(),
            self.provisioner_client_secret.as_deref(),
        ) {
            (Some(id), Some(secret)) if !id.is_empty() && !secret.is_empty() => Some((id, secret)),
            _ => None,
        }
    }
}

/// テナンシーモード。`tenant_id` の取得元（案A/案C）を決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tenancy {
    /// オンプレ/cell シングルテナント。固定値 `auth.tenant_id`（案C）を使う。
    Single,
    /// SaaS マルチテナント。Keycloak claim `tenant`（案A）を必須にし、欠落は fail-closed。
    Multi,
}

impl AuthConfig {
    /// 実効 JWKS URI。`jwks_uri` 未指定なら内部 base（無ければ issuer）から OIDC 規約で導出する。
    /// JWKS はサーバ側 backchannel で取得するため内部 base を優先する。
    pub fn effective_jwks_uri(&self) -> String {
        self.jwks_uri
            .clone()
            .unwrap_or_else(|| format!("{}/protocol/openid-connect/certs", self.backchannel_base()))
    }
}
