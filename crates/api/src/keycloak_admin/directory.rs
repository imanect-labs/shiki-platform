//! 管理ディレクトリのKeycloak操作。IDはURLセグメントとしてエンコードする。
use super::{ensure_ok, groups::KcGroup, KeycloakAdmin, KeycloakAdminError};
use serde_json::json;
use std::collections::HashMap;

fn segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

impl KeycloakAdmin<'_> {
    /// 自組織rootの子を一ページ読む。全件をブラウザへ渡さない。
    pub async fn directory_children(
        &self,
        parent: &str,
        first: u32,
        limit: u32,
    ) -> Result<Vec<KcGroup>, KeycloakAdminError> {
        let response = self
            .http
            .get(format!("{}/groups/{}/children", self.base, segment(parent)))
            .bearer_auth(self.admin_token().await?)
            .query(&[
                ("first", first.to_string()),
                ("max", limit.clamp(1, 100).to_string()),
                ("briefRepresentation", "false".to_string()),
            ])
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))
    }

    pub async fn directory_group(&self, id: &str) -> Result<KcGroup, KeycloakAdminError> {
        let response = self
            .http
            .get(format!("{}/groups/{}", self.base, segment(id)))
            .bearer_auth(self.admin_token().await?)
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))
    }

    /// ADへの逆書込を避け、LDAPマッパー配下でない管理専用の領域を使う。
    pub async fn directory_role_container(
        &self,
        tenant: &str,
        org: &str,
    ) -> Result<String, KeycloakAdminError> {
        let path = format!("/__shiki_roles/{tenant}/{org}");
        for (_, root) in self.ldap_mapper_groups().await? {
            let root = root.trim_end_matches('/');
            if root.is_empty()
                || path == root
                || path
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with('/'))
            {
                return Err(KeycloakAdminError::NotConfigured(
                    "LDAPの同期先とShikiの管理ロール領域を分離してください".into(),
                ));
            }
        }
        self.ensure_group_path(&["__shiki_roles", tenant, org])
            .await
    }

    /// ロールIDは固定し、名前と親はサーバ管理の属性へ保存する。
    pub async fn save_directory_role(
        &self,
        root: &str,
        role: &storage::directory::admin::RoleEdit,
    ) -> Result<String, KeycloakAdminError> {
        let id = self.ensure_child_group(Some(root), &role.id).await?;
        let mut attributes = HashMap::new();
        attributes.insert("shiki.role_id", vec![role.id.clone()]);
        attributes.insert("shiki.label", vec![role.label.clone()]);
        attributes.insert(
            "shiki.parent",
            role.parent_id.iter().cloned().collect::<Vec<_>>(),
        );
        let response = self
            .http
            .put(format!("{}/groups/{}", self.base, segment(&id)))
            .bearer_auth(self.admin_token().await?)
            .json(&json!({ "name": role.id, "attributes": attributes }))
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        ensure_ok(response).await?;
        Ok(id)
    }
}
