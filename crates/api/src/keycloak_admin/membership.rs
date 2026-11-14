//! 利用者の所属グループとロールの属性を読む（組織の member と部署の定期の照合・D-136）。
//!
//! 照合は 1 回に何百人も読むので、トークンを読むたびに取り直さない（[`AdminSession`] が 1 つを持ち回し、401 のときだけ
//! 取り直す）。読むのは provisioner の `manage-users`（`view-users` を含む）で足りる（権限は足していない）。
//! 利用者の id は URL の 1 つの段として符号化する（id に `/` などがあっても別の API に届かない）。

use serde::Deserialize;

use super::{status_error, KcGroup, KeycloakAdmin, KeycloakAdminError};

/// 所属グループの一覧の 1 ページの件数。
const PAGE: u32 = 100;

/// トークンを持ち回す admin REST の読み手（[`KeycloakAdmin::session`]）。
pub struct AdminSession<'a> {
    admin: &'a KeycloakAdmin<'a>,
    token: String,
}

impl<'a> KeycloakAdmin<'a> {
    /// トークンを 1 つ取り、読み手を作る（取れなければ誤り）。
    pub async fn session(&'a self) -> Result<AdminSession<'a>, KeycloakAdminError> {
        let token = self.admin_token().await?;
        Ok(AdminSession { admin: self, token })
    }
}

/// 利用者の表現のうち照合で使うもの。
#[derive(Deserialize)]
struct UserAttributes {
    #[serde(default)]
    attributes: std::collections::HashMap<String, Vec<String>>,
}

impl AdminSession<'_> {
    /// 利用者の所属グループ（直接の所属）の完全なパス。ログインのクライアントのトークンの `groups`（group membership
    /// のマッパー・full.path）と同じ値。利用者がいなければ `None`。
    ///
    /// パスの無いグループが返ったら誤りにする（空のパスを「グループが無い」と読んで外さない）。
    pub async fn user_group_paths(
        &mut self,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, KeycloakAdminError> {
        let url = self.user_url(user_id, &["groups"])?;
        let mut out = Vec::new();
        let mut first: u32 = 0;
        loop {
            let query = [
                ("first", first.to_string()),
                ("max", PAGE.to_string()),
                ("briefRepresentation", "true".to_string()),
            ];
            let Some(resp) = self.get(&url, &query).await? else {
                return Ok(None);
            };
            let page: Vec<KcGroup> = resp
                .json()
                .await
                .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
            let n = page.len();
            for group in page {
                if group.path.is_empty() {
                    return Err(KeycloakAdminError::Status {
                        status: 200,
                        body: format!("所属グループ {} にパスがありません", group.id),
                    });
                }
                out.push(group.path);
            }
            if (n as u32) < PAGE {
                return Ok(Some(out));
            }
            first += PAGE;
        }
    }

    /// 利用者の属性 `name` の値（ログインのクライアントのマッパーが claim に写す元・`roles` など）。
    /// 利用者がいなければ `None`。
    pub async fn user_attribute(
        &mut self,
        user_id: &str,
        name: &str,
    ) -> Result<Option<Vec<String>>, KeycloakAdminError> {
        let url = self.user_url(user_id, &[])?;
        let Some(resp) = self.get(&url, &[]).await? else {
            return Ok(None);
        };
        let user: UserAttributes = resp
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        Ok(Some(user.attributes.get(name).cloned().unwrap_or_default()))
    }

    /// トークンを取り直す（Keycloak に届くかの確かめにも使う）。
    pub async fn refresh_token(&mut self) -> Result<(), KeycloakAdminError> {
        self.token = self.admin.admin_token().await?;
        Ok(())
    }

    /// `{base}/users/{user_id}/{rest...}`（各段を符号化する）。
    fn user_url(&self, user_id: &str, rest: &[&str]) -> Result<reqwest::Url, KeycloakAdminError> {
        let mut url = reqwest::Url::parse(&self.admin.base)
            .map_err(|e| KeycloakAdminError::NotConfigured(format!("admin base: {e}")))?;
        url.path_segments_mut()
            .map_err(|()| KeycloakAdminError::NotConfigured("admin base に段を足せません".into()))?
            .pop_if_empty()
            .push("users")
            .push(user_id)
            .extend(rest);
        Ok(url)
    }

    /// GET する。404 は `None`（利用者がいない）、401 はトークンを 1 回だけ取り直す、ほかの失敗は誤り。
    async fn get(
        &mut self,
        url: &reqwest::Url,
        query: &[(&str, String)],
    ) -> Result<Option<reqwest::Response>, KeycloakAdminError> {
        for retry in [false, true] {
            let resp = self
                .admin
                .http
                .get(url.clone())
                .bearer_auth(&self.token)
                .query(query)
                .send()
                .await
                .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
            match resp.status().as_u16() {
                s if (200..300).contains(&s) => return Ok(Some(resp)),
                404 => return Ok(None),
                401 if !retry => self.token = self.admin.admin_token().await?,
                _ => return Err(status_error(resp).await),
            }
        }
        Err(KeycloakAdminError::Status {
            status: 401,
            body: "取り直したトークンも断られました".into(),
        })
    }
}
