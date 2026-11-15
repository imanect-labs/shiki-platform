//! 組織ごとのLDAP接続。provider/mappersの所有範囲と認可属性をサーバで固定する。
use super::{ensure_ok, KeycloakAdmin, KeycloakAdminError};
use authz::AuthContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use storage::directory::admin::AD_GROUP_ROOT;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
pub struct LdapConnectionInput {
    pub connection_url: String,
    pub users_dn: String,
    pub groups_dn: String,
    pub bind_dn: String,
    /// 省略は保存済みのcredentialを保持。応答や監査には含めない。
    pub bind_password: Option<String>,
    #[serde(default)]
    pub preserve_group_inheritance: bool,
    #[serde(default)]
    pub recovery_confirmed: bool,
    #[serde(default)]
    pub identity_reset_confirmed: bool,
}

// Keycloakの接続状態とLDAPの独立した設定フラグをそのまま返す公開DTO。
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Serialize, ToSchema)]
pub struct LdapConnectionStatus {
    pub configured: bool,
    pub enabled: bool,
    pub connection_url: Option<String>,
    pub users_dn: Option<String>,
    pub groups_dn: Option<String>,
    pub bind_dn: Option<String>,
    pub credential_saved: bool,
    pub recovery_required: bool,
    pub identity_current: bool,
    pub preserve_group_inheritance: bool,
}

#[derive(Deserialize)]
struct Component {
    id: String,
    #[serde(rename = "providerId")]
    provider_id: String,
    #[serde(default)]
    config: BTreeMap<String, Vec<String>>,
}

fn first(component: &Component, key: &str) -> Option<String> {
    component.config.get(key).and_then(|v| v.first()).cloned()
}
fn component_name(ctx: &AuthContext) -> String {
    format!(
        "shiki-ad-{}",
        storage::directory::admin::role_id_prefix(ctx)
            .trim_start_matches("managed-")
            .trim_end_matches('-')
    )
}
fn id_segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

impl KeycloakAdmin<'_> {
    async fn owned_ldap(&self, ctx: &AuthContext) -> Result<Option<Component>, KeycloakAdminError> {
        let token = self.admin_token().await?;
        let response = self
            .http
            .get(format!("{}/components", self.base))
            .bearer_auth(token)
            .query(&[
                ("type", "org.keycloak.storage.UserStorageProvider"),
                ("name", component_name(ctx).as_str()),
            ])
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let mut components: Vec<Component> = ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if components.len() > 1 {
            return Err(KeycloakAdminError::NotConfigured(
                "同じ管理接続名が重複しています".into(),
            ));
        }
        let component = components.pop();
        if component.as_ref().is_some_and(|c| {
            c.provider_id != "ldap"
                || first(c, "shiki.tenant").as_deref() != Some(ctx.tenant_id())
                || first(c, "shiki.org").as_deref() != Some(ctx.org())
        }) {
            return Err(KeycloakAdminError::NotConfigured(
                "LDAP接続の所有範囲を確認してください".into(),
            ));
        }
        Ok(component)
    }

    pub async fn ldap_connection(
        &self,
        ctx: &AuthContext,
    ) -> Result<LdapConnectionStatus, KeycloakAdminError> {
        let component = self.owned_ldap(ctx).await?;
        Ok(LdapConnectionStatus {
            configured: component.is_some(),
            recovery_required: false,
            identity_current: component
                .as_ref()
                .is_some_and(|c| first(c, "shiki.groupIdentity").as_deref() == Some("guid-v1")),
            enabled: component
                .as_ref()
                .and_then(|c| first(c, "enabled"))
                .as_deref()
                == Some("true"),
            connection_url: component.as_ref().and_then(|c| first(c, "connectionUrl")),
            users_dn: component.as_ref().and_then(|c| first(c, "usersDn")),
            groups_dn: component.as_ref().and_then(|c| first(c, "shiki.groupsDn")),
            bind_dn: component.as_ref().and_then(|c| first(c, "bindDn")),
            preserve_group_inheritance: component
                .as_ref()
                .and_then(|c| first(c, "shiki.preserveGroups"))
                .as_deref()
                == Some("true"),
            credential_saved: component
                .as_ref()
                .and_then(|c| first(c, "bindCredential"))
                .is_some(),
        })
    }

    pub async fn test_ldap(
        &self,
        ctx: &AuthContext,
        input: &LdapConnectionInput,
    ) -> Result<(), KeycloakAdminError> {
        let stored = self.owned_ldap(ctx).await?;
        // Keycloak treats this marker as reuse of the component's saved secret.
        // Accept it only when generated here after checking the unchanged identity.
        if input.bind_password.as_deref() == Some("**********") {
            return Err(KeycloakAdminError::NotConfigured(
                "バインド用パスワードを入力し直してください".into(),
            ));
        }
        let secret = match input
            .bind_password
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            Some(value) => value,
            None if stored.as_ref().is_some_and(|component| {
                first(component, "connectionUrl").as_deref() == Some(input.connection_url.as_str())
                    && first(component, "bindDn").as_deref() == Some(input.bind_dn.as_str())
            }) =>
            {
                "**********"
            }
            None => {
                return Err(KeycloakAdminError::NotConfigured(
                    "バインド用パスワードを入力してください".into(),
                ))
            }
        };
        let token = self.admin_token().await?;
        let response = self.http.post(format!("{}/testLDAPConnection", self.base)).bearer_auth(token)
            .json(&json!({ "action": "testAuthentication", "connectionUrl": input.connection_url,
                "bindDn": input.bind_dn, "bindCredential": secret, "componentId": stored.as_ref().map(|c| &c.id),
                "authType": "simple", "useTruststoreSpi": "always", "connectionTimeout": "10000", "startTls": "false" }))
            .timeout(std::time::Duration::from_mins(2))
            .send().await.map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        // 接続・認証の応答本文はcredentialを含み得るため保持・記録しない。
        if !response.status().is_success() {
            return Err(KeycloakAdminError::NotConfigured(
                "ADへの接続または認証に失敗しました".into(),
            ));
        }
        Ok(())
    }

    async fn put_component(
        &self,
        id: Option<&str>,
        body: &Value,
    ) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let builder = match id {
            Some(id) => self
                .http
                .put(format!("{}/components/{}", self.base, id_segment(id))),
            None => self.http.post(format!("{}/components", self.base)),
        };
        let response = builder
            .bearer_auth(token)
            .json(body)
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if !response.status().is_success() {
            return Err(KeycloakAdminError::NotConfigured(
                "KeycloakのLDAP設定を保存できませんでした".into(),
            ));
        }
        Ok(())
    }

    async fn ensure_ldap_mapper(
        &self,
        provider: &str,
        name: &str,
        kind: &str,
        config: Value,
    ) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let response = self
            .http
            .get(format!("{}/components", self.base))
            .bearer_auth(token)
            .query(&[
                ("parent", provider),
                (
                    "type",
                    "org.keycloak.storage.ldap.mappers.LDAPStorageMapper",
                ),
                ("name", name),
            ])
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let existing: Vec<Component> = ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if existing.len() > 1 {
            return Err(KeycloakAdminError::NotConfigured(
                "LDAPマッパーが重複しています".into(),
            ));
        }
        let existing = existing.first();
        let id = if let Some(component) = existing.filter(|c| c.provider_id != kind) {
            let token = self.admin_token().await?;
            let response = self
                .http
                .delete(format!(
                    "{}/components/{}",
                    self.base,
                    id_segment(&component.id)
                ))
                .bearer_auth(token)
                .timeout(std::time::Duration::from_mins(2))
                .send()
                .await
                .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
            if !response.status().is_success()
                && response.status() != reqwest::StatusCode::NOT_FOUND
            {
                return Err(KeycloakAdminError::NotConfigured(
                    "旧LDAPマッパーを更新できませんでした".into(),
                ));
            }
            None
        } else {
            existing.map(|c| c.id.as_str())
        };
        self.put_component(id, &json!({
            "name": name, "parentId": provider, "providerType": "org.keycloak.storage.ldap.mappers.LDAPStorageMapper", "providerId": kind, "config": config,
        })).await
    }

    pub async fn save_ldap(
        &self,
        ctx: &AuthContext,
        input: &LdapConnectionInput,
    ) -> Result<LdapConnectionStatus, KeycloakAdminError> {
        self.test_ldap(ctx, input).await?;
        let existing = self.owned_ldap(ctx).await?;
        let token = self.admin_token().await?;
        let response = self
            .http
            .get(&self.base)
            .bearer_auth(token)
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let realm: Value = ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let realm_id = realm
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| KeycloakAdminError::NotConfigured("realm IDがありません".into()))?;
        let reset_groups = existing
            .as_ref()
            .is_some_and(|c| first(c, "shiki.groupIdentity").as_deref() != Some("guid-v1"));
        let org_path = format!("/tenants/{}/{}", ctx.tenant_id(), ctx.org());
        let mut config = json!({
            "enabled": ["false"], "vendor": ["ad"], "editMode": ["READ_ONLY"], "importEnabled": ["true"], "syncRegistrations": ["false"],
            "connectionUrl": [input.connection_url], "usersDn": [input.users_dn], "bindDn": [input.bind_dn],
            "bindCredential": [input.bind_password.as_deref().filter(|v| !v.is_empty()).unwrap_or("**********")],
            "authType": ["simple"], "useTruststoreSpi": ["always"], "startTls": ["false"], "connectionTimeout": ["10000"], "readTimeout": ["10000"],
            "usernameLDAPAttribute": ["sAMAccountName"], "rdnLDAPAttribute": ["cn"], "uuidLDAPAttribute": ["objectGUID"],
            "userObjectClasses": ["person,organizationalPerson,user"], "searchScope": ["2"], "pagination": ["true"], "batchSizeForSync": ["100"],
            "fullSyncPeriod": ["-1"], "changedSyncPeriod": ["-1"], "cachePolicy": ["DEFAULT"], "priority": ["0"],
            "shiki.groupIdentity": [existing.as_ref().and_then(|c|first(c,"shiki.groupIdentity")).unwrap_or_else(||"pending-guid-v1".into())], "shiki.tenant": [ctx.tenant_id()], "shiki.org": [ctx.org()], "shiki.groupsDn": [input.groups_dn], "shiki.preserveGroups": [input.preserve_group_inheritance.to_string()]
        });
        if let Some(existing) = &existing {
            for (key, value) in &existing.config {
                if config.get(key).is_none() {
                    config[key] = json!(value);
                }
            }
        }
        let mut body = json!({ "name": component_name(ctx), "parentId": realm_id, "providerType": "org.keycloak.storage.UserStorageProvider", "providerId": "ldap", "config": config });
        self.put_component(existing.as_ref().map(|c| c.id.as_str()), &body)
            .await?;
        let provider = self
            .owned_ldap(ctx)
            .await?
            .ok_or_else(|| KeycloakAdminError::NotConfigured("LDAP設定が見つかりません".into()))?;
        // 形式更新で旧ネイティブIDが別のGUID群へ再利用されることも防ぐ。providerはまだ無効。
        if reset_groups {
            for root_name in ["ad", AD_GROUP_ROOT] {
                if let Some(root) = self
                    .find_group_path(&["tenants", ctx.tenant_id(), ctx.org(), root_name])
                    .await?
                {
                    self.delete_group(&root.id).await?;
                }
            }
        }
        self.ensure_group_path(&["tenants", ctx.tenant_id(), ctx.org(), AD_GROUP_ROOT])
            .await?;
        // 固定テナント・固定組織のマッパーが揃うまでproviderは有効化しない。
        self.ensure_ldap_mapper(
            &provider.id,
            "shiki-tenant",
            "hardcoded-attribute-mapper",
            json!({ "user.model.attribute": ["tenant"], "attribute.value": [ctx.tenant_id()] }),
        )
        .await?;
        self.ensure_ldap_mapper(
            &provider.id,
            "shiki-organization",
            "hardcoded-ldap-group-mapper",
            json!({ "group": [org_path] }),
        )
        .await?;
        self.ensure_ldap_mapper(&provider.id, "shiki-ad-groups", "shiki-ad-guid-group-mapper", json!({
            "groups.dn": [input.groups_dn], "groups.path": [format!("{org_path}/{AD_GROUP_ROOT}")], "group.name.ldap.attribute": ["objectGUID"], "group.object.classes": ["group"],
            "membership.ldap.attribute": ["member"], "membership.attribute.type": ["DN"], "member.of.ldap.attribute": ["memberOf"],
            "mode": ["READ_ONLY"], "preserve.group.inheritance": ["false"], "ignore.missing.groups": ["false"],
            "drop.non.existing.groups.during.sync": ["true"], "mapped.group.attributes": ["cn,distinguishedName,memberOf"],
            "user.roles.retrieve.strategy": ["LOAD_GROUPS_BY_MEMBER_ATTRIBUTE_RECURSIVELY"]
        })).await?;
        config["enabled"] = json!(["true"]);
        config["shiki.groupIdentity"] = json!(["guid-v1"]);
        body["config"] = config;
        self.put_component(Some(&provider.id), &body).await?;
        self.ldap_connection(ctx).await
    }
    /// 自組織の所有するグループmapperだけを、AD→Keycloak方向で同期する。
    pub async fn sync_ldap_groups(&self, ctx: &AuthContext) -> Result<(), KeycloakAdminError> {
        let provider = self
            .owned_ldap(ctx)
            .await?
            .ok_or_else(|| KeycloakAdminError::NotConfigured("AD接続が未設定です".into()))?;
        if first(&provider, "enabled").as_deref() != Some("true") {
            return Err(KeycloakAdminError::NotConfigured("AD接続が無効です".into()));
        }
        let token = self.admin_token().await?;
        let response = self
            .http
            .get(format!("{}/components", self.base))
            .bearer_auth(&token)
            .query(&[
                ("parent", provider.id.as_str()),
                (
                    "type",
                    "org.keycloak.storage.ldap.mappers.LDAPStorageMapper",
                ),
                ("name", "shiki-ad-groups"),
            ])
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let mappers: Vec<Component> = ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let [mapper] = mappers.as_slice() else {
            return Err(KeycloakAdminError::NotConfigured(
                "管理グループmapperがありません".into(),
            ));
        };
        if mapper.provider_id != "shiki-ad-guid-group-mapper"
            || first(mapper, "groups.path").as_deref()
                != Some(&format!(
                    "/tenants/{}/{}/{AD_GROUP_ROOT}",
                    ctx.tenant_id(),
                    ctx.org()
                ))
            || first(mapper, "mode").as_deref() != Some("READ_ONLY")
            || first(mapper, "group.name.ldap.attribute").as_deref() != Some("objectGUID")
            || first(mapper, "preserve.group.inheritance").as_deref() != Some("false")
            || first(mapper, "mapped.group.attributes").as_deref()
                != Some("cn,distinguishedName,memberOf")
            || first(mapper, "user.roles.retrieve.strategy").as_deref()
                != Some("LOAD_GROUPS_BY_MEMBER_ATTRIBUTE_RECURSIVELY")
        {
            return Err(KeycloakAdminError::NotConfigured(
                "管理グループmapperの所有範囲が不正です".into(),
            ));
        }
        let response = self
            .http
            .post(format!(
                "{}/user-storage/{}/mappers/{}/sync",
                self.base,
                id_segment(&provider.id),
                id_segment(&mapper.id)
            ))
            .bearer_auth(token)
            .query(&[("direction", "fedToKeycloak")])
            .timeout(std::time::Duration::from_mins(2))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if !response.status().is_success() {
            return Err(KeycloakAdminError::NotConfigured(
                "ADグループ同期に失敗しました".into(),
            ));
        }
        let result: Value = response
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        if result.get("failed").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Err(KeycloakAdminError::NotConfigured(
                "ADグループ同期に失敗項目があります".into(),
            ));
        }
        Ok(())
    }
    /// テナント撤去時、所有印が一致する接続を止めて削除する。別テナントは触らない。
    pub async fn delete_directory_integrations(
        &self,
        tenant: &str,
    ) -> Result<(), KeycloakAdminError> {
        let token = self.admin_token().await?;
        let response = self
            .http
            .get(format!("{}/components", self.base))
            .bearer_auth(&token)
            .query(&[("type", "org.keycloak.storage.UserStorageProvider")])
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        let components: Vec<Component> = ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
        for component in components {
            if component.provider_id == "ldap"
                && first(&component, "shiki.tenant").as_deref() == Some(tenant)
            {
                let response = self
                    .http
                    .delete(format!(
                        "{}/components/{}",
                        self.base,
                        id_segment(&component.id)
                    ))
                    .bearer_auth(&token)
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(|e| KeycloakAdminError::Transport(e.to_string()))?;
                if !response.status().is_success()
                    && response.status() != reqwest::StatusCode::NOT_FOUND
                {
                    return Err(KeycloakAdminError::NotConfigured(
                        "AD接続の撤去に失敗しました".into(),
                    ));
                }
            }
        }
        if let Some(root) = self.find_group_path(&["__shiki_roles", tenant]).await? {
            self.delete_group(&root.id).await?;
        }
        Ok(())
    }
}
