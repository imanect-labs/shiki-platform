//! Keycloak admin REST クライアント（SAAS.2 テナント・プロビジョニング / #87）。
//!
//! `shiki-provisioner` service account（client_credentials）でトークンを取得し、
//! realm 内の group / user を管理する。テナント作成＝group `/{org}` ＋ 初期 admin ユーザー
//! （`attributes.tenant` / 一時パスワード＋初回変更必須）、テナント削除＝tenant 属性一致
//! ユーザーと group の撤去。全操作は**冪等**（既存/不在を成功に倒す）で、削除フローの
//! 再実行で収束する。
//!
//! [`crate::oidc`] と同じ薄いパターン（`&reqwest::Client`＋専用 error enum・リトライ無し）。

use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::AuthConfig;

#[derive(Debug, thiserror::Error)]
pub enum KeycloakAdminError {
    /// admin REST への到達自体が失敗（ネットワーク等）。
    #[error("Keycloak admin API への接続に失敗: {0}")]
    Transport(String),
    /// admin REST がエラー応答を返した。
    #[error("Keycloak admin API がエラー応答: status={status} body={body}")]
    Status { status: u16, body: String },
    /// プロビジョニング設定（provisioner client / admin base URL）が無い。
    #[error("プロビジョニング設定が不足: {0}")]
    NotConfigured(String),
}

/// Keycloak 上のユーザー（必要フィールドのみ）。
#[derive(Debug, Clone, Deserialize)]
pub struct KcUser {
    pub id: String,
    pub username: String,
    /// ユーザー属性（`tenant` の照合に使う）。
    #[serde(default)]
    pub attributes: std::collections::HashMap<String, Vec<String>>,
    /// 未消化の必須アクション（`UPDATE_PASSWORD` が残っていれば初回ログイン前）。
    #[serde(default, rename = "requiredActions")]
    pub required_actions: Vec<String>,
}

impl KcUser {
    /// `attributes.tenant` の先頭値。
    fn tenant(&self) -> Option<&str> {
        self.attributes
            .get("tenant")
            .and_then(|v| v.first())
            .map(String::as_str)
    }
}

/// admin REST の薄いクライアント。トークンは呼び出し毎に取得する
/// （プロビジョニングは低頻度の管理操作でありキャッシュ複雑性に見合わない）。
pub struct KeycloakAdmin<'a> {
    http: &'a reqwest::Client,
    auth: &'a AuthConfig,
    base: String,
}

impl<'a> KeycloakAdmin<'a> {
    /// 設定から構築する。provisioner 資格情報 or admin base が無ければ `NotConfigured`。
    pub fn from_config(
        http: &'a reqwest::Client,
        auth: &'a AuthConfig,
    ) -> Result<Self, KeycloakAdminError> {
        if auth.provisioner_credentials().is_none() {
            return Err(KeycloakAdminError::NotConfigured(
                "auth.provisioner_client_id / provisioner_client_secret".into(),
            ));
        }
        let base = auth.admin_base().ok_or_else(|| {
            KeycloakAdminError::NotConfigured("auth.admin_base_url（realm パス導出不能）".into())
        })?;
        Ok(Self { http, auth, base })
    }

    /// service account の access token を client_credentials で取得する。
    async fn admin_token(&self) -> Result<String, KeycloakAdminError> {
        // from_config で存在検証済み。
        let (id, secret) = self
            .auth
            .provisioner_credentials()
            .expect("from_config で検証済み");
        let resp = self
            .http
            .post(self.auth.token_endpoint())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", id),
                ("client_secret", secret),
            ])
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let resp = ensure_ok(resp).await?;
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
        }
        let t: Token = resp
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        Ok(t.access_token)
    }

    /// トップレベル group `/{name}` を冪等に用意し、その id を返す。
    pub async fn ensure_group(&self, name: &str) -> Result<String, KeycloakAdminError> {
        let token = self.admin_token().await?;
        // 409（既存）は成功に倒し、検索で id を引く。
        let resp = self
            .http
            .post(format!("{}/groups", self.base))
            .bearer_auth(&token)
            .json(&json!({ "name": name }))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if !resp.status().is_success() && resp.status().as_u16() != 409 {
            return Err(status_error(resp).await);
        }
        // 作成応答は body が空のため、検索で id を解決する（exact 一致）。
        let resp = self
            .http
            .get(format!("{}/groups", self.base))
            .bearer_auth(&token)
            .query(&[("search", name), ("exact", "true")])
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let resp = ensure_ok(resp).await?;
        #[derive(Deserialize)]
        struct Group {
            id: String,
            name: String,
        }
        let groups: Vec<Group> = resp
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        groups
            .into_iter()
            .find(|g| g.name == name)
            .map(|g| g.id)
            .ok_or_else(|| KeycloakAdminError::Status {
                status: 404,
                body: format!("group '{name}' が作成後も見つかりません"),
            })
    }

    /// username で 1 ユーザーを引く（不在は `None`）。
    pub async fn find_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<KcUser>, KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(format!("{}/users", self.base))
            .bearer_auth(&token)
            .query(&[("username", username), ("exact", "true")])
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let resp = ensure_ok(resp).await?;
        let users: Vec<KcUser> = resp
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        Ok(users.into_iter().next())
    }

    /// テナント初期 admin ユーザーを冪等に作成し、`(user_id, 新規作成なら一時パスワード)` を返す。
    ///
    /// - `attributes.tenant` / `attributes.roles` を設定（claim マッパーの取得元）
    /// - group `/{org}` へ参加（`groups` claim ＝ org 解決の取得元）
    /// - 一時パスワード＋ `UPDATE_PASSWORD` 必須アクション（初回ログインで変更を強制）
    /// - 既存ユーザーなら**作り直さず**現状を維持。ただし **初回ログイン前
    ///   （`UPDATE_PASSWORD` 未消化）なら一時パスワードを再発行して返す**（#91 M-6:
    ///   プロビジョニング後段の失敗で初回応答の temp_password が破棄されても、
    ///   同一リクエストの再実行で回収できる）。ログイン済みならパスワードに触れない。
    #[allow(clippy::too_many_arguments)] // ユーザー作成に必要な属性一式。
    pub async fn ensure_tenant_admin(
        &self,
        tenant_id: &str,
        org_group: &str,
        username: &str,
        email: &str,
        temp_password: &str,
    ) -> Result<(String, Option<String>), KeycloakAdminError> {
        if let Some(existing) = self.find_user_by_username(username).await? {
            return self
                .resolve_existing_admin(existing, tenant_id, username, temp_password)
                .await;
        }
        let token = self.admin_token().await?;
        let body = json!({
            "username": username,
            "email": email,
            "enabled": true,
            "emailVerified": true,
            "attributes": { "tenant": [tenant_id], "roles": [] },
            "groups": [format!("/{org_group}")],
            "credentials": [{ "type": "password", "value": temp_password, "temporary": true }],
            "requiredActions": ["UPDATE_PASSWORD"],
        });
        let resp = self
            .http
            .post(format!("{}/users", self.base))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        // 並行作成の 409 は「既存」に倒す（冪等）。tenant 照合と初回ログイン前の一時パスワード
        // 再発行（#91 M-6）は pre-POST 検出パスと同一に扱う（レース敗者でも回収可能にする・
        // #91 P2 レビュー対応: 敗者が None を返すと初期 admin が資格情報を失う）。
        if resp.status().as_u16() == 409 {
            let user = self.find_user_by_username(username).await?.ok_or_else(|| {
                KeycloakAdminError::Status {
                    status: 409,
                    body: "既存応答だがユーザーが見つかりません".into(),
                }
            })?;
            return self
                .resolve_existing_admin(user, tenant_id, username, temp_password)
                .await;
        }
        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }
        let user = self.find_user_by_username(username).await?.ok_or_else(|| {
            KeycloakAdminError::Status {
                status: 404,
                body: "作成直後のユーザーが見つかりません".into(),
            }
        })?;
        Ok((user.id, Some(temp_password.to_string())))
    }

    /// 既存ユーザーを初期 admin として解決する（tenant 照合＋初回ログイン前の一時パスワード再発行）。
    ///
    /// `ensure_tenant_admin` の pre-POST 検出パスと 409（並行作成の敗者）パスで共用する（#91 M-6/P2）:
    /// - 別テナントの既存ユーザーは乗っ取らず 409 で拒否。
    /// - 初回ログイン前（`UPDATE_PASSWORD` 未消化）なら一時パスワードを再発行して返す。
    /// - ログイン済みならパスワードに触れず `None` を返す。
    async fn resolve_existing_admin(
        &self,
        user: KcUser,
        tenant_id: &str,
        username: &str,
        temp_password: &str,
    ) -> Result<(String, Option<String>), KeycloakAdminError> {
        if user.tenant() != Some(tenant_id) {
            return Err(KeycloakAdminError::Status {
                status: 409,
                body: format!(
                    "username '{username}' は別テナント（tenant={:?}）の既存ユーザーです。\
                     別の admin_username を指定してください",
                    user.tenant()
                ),
            });
        }
        if user.required_actions.iter().any(|a| a == "UPDATE_PASSWORD") {
            self.reset_temp_password(&user.id, temp_password).await?;
            return Ok((user.id, Some(temp_password.to_string())));
        }
        Ok((user.id, None))
    }

    /// 一時パスワードを再設定する（`temporary: true`＝初回ログインで変更必須のまま）。
    ///
    /// `ensure_tenant_admin` の再実行時、初回ログイン前の admin にのみ使う（#91 M-6）。
    async fn reset_temp_password(
        &self,
        user_id: &str,
        temp_password: &str,
    ) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .put(format!("{}/users/{}/reset-password", self.base, user_id))
            .bearer_auth(&token)
            .json(&json!({ "type": "password", "value": temp_password, "temporary": true }))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if resp.status().is_success() {
            return Ok(());
        }
        Err(status_error(resp).await)
    }

    /// `attributes.tenant == tenant_id` のユーザーを全列挙する（テナント削除用）。
    pub async fn find_users_by_tenant(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<KcUser>, KeycloakAdminError> {
        let token = self.admin_token().await?;
        // Keycloak の属性検索（`q=key:value`）。ページングして全件集める。
        let mut out = Vec::new();
        let mut first: u32 = 0;
        const PAGE: u32 = 100;
        loop {
            let resp = self
                .http
                .get(format!("{}/users", self.base))
                .bearer_auth(&token)
                .query(&[
                    ("q", format!("tenant:{tenant_id}").as_str()),
                    ("first", first.to_string().as_str()),
                    ("max", PAGE.to_string().as_str()),
                ])
                .send()
                .await
                .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
            let resp = ensure_ok(resp).await?;
            let users: Vec<KcUser> = resp
                .json()
                .await
                .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
            let n = users.len();
            out.extend(users);
            if (n as u32) < PAGE {
                return Ok(out);
            }
            first += PAGE;
        }
    }

    /// ユーザーの `tenant` 属性を更新する（retenant 移行時の IdP 追従・#89）。
    ///
    /// Keycloak の user PUT は部分更新でなく representation 置換のため、GET で全体を
    /// 取得して attributes.tenant だけ差し替えて書き戻す。
    pub async fn update_user_tenant(
        &self,
        user_id: &str,
        tenant_id: &str,
    ) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(format!("{}/users/{}", self.base, user_id))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let resp = ensure_ok(resp).await?;
        let mut user: Value = resp
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let attrs = user
            .as_object_mut()
            .and_then(|o| {
                o.entry("attributes")
                    .or_insert_with(|| Value::Object(Default::default()))
                    .as_object_mut()
            })
            .ok_or_else(|| KeycloakAdminError::Status {
                status: 500,
                body: "user representation が想定外の形式です".into(),
            })?;
        attrs.insert("tenant".into(), serde_json::json!([tenant_id]));
        let resp = self
            .http
            .put(format!("{}/users/{}", self.base, user_id))
            .bearer_auth(&token)
            .json(&user)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if resp.status().is_success() {
            return Ok(());
        }
        Err(status_error(resp).await)
    }

    /// ユーザーを削除する（不在 404 は成功に倒す＝冪等）。
    pub async fn delete_user(&self, user_id: &str) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .delete(format!("{}/users/{}", self.base, user_id))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if resp.status().is_success() || resp.status().as_u16() == 404 {
            return Ok(());
        }
        Err(status_error(resp).await)
    }

    /// トップレベル group `/{name}` を削除する（不在は成功に倒す＝冪等）。
    pub async fn delete_group_by_name(&self, name: &str) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(format!("{}/groups", self.base))
            .bearer_auth(&token)
            .query(&[("search", name), ("exact", "true")])
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let resp = ensure_ok(resp).await?;
        let groups: Vec<Value> = resp
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let Some(id) = groups
            .iter()
            .find(|g| g.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|g| g.get("id").and_then(Value::as_str))
        else {
            return Ok(()); // 不在＝冪等成功。
        };
        let resp = self
            .http
            .delete(format!("{}/groups/{}", self.base, id))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if resp.status().is_success() || resp.status().as_u16() == 404 {
            return Ok(());
        }
        Err(status_error(resp).await)
    }
}

async fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response, KeycloakAdminError> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    Err(status_error(resp).await)
}

async fn status_error(resp: reqwest::Response) -> KeycloakAdminError {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    KeycloakAdminError::Status { status, body }
}
