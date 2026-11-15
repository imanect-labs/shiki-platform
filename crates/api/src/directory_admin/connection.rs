//! 接続先は運用者の許可リストに限定する。組織管理者の任意ネットワーク探索を防ぐ。
use crate::{
    config::AuthConfig,
    error::ApiError,
    keycloak_admin::{ldap::LdapConnectionInput, KeycloakAdmin, KeycloakAdminError},
    AppState,
};
use authz::AuthContext;
use storage::directory::admin::AdGroup;

pub fn validate(config: &AuthConfig, input: &LdapConnectionInput) -> Result<(), ApiError> {
    let url = reqwest::Url::parse(&input.connection_url)
        .map_err(|_| super::input_error("LDAPS接続先が不正です".into()))?;
    if url.scheme() != "ldaps"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
        || !config.directory_ldap_allowed_hosts.iter().any(|allowed| {
            url.host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case(allowed))
        })
        || url.port().is_some_and(|port| port != 636)
    {
        return Err(super::input_error(
            "運用設定で許可されたホストのLDAPS（636）を指定してください".into(),
        ));
    }
    for dn in [&input.users_dn, &input.groups_dn, &input.bind_dn] {
        if dn.trim().is_empty() || dn.len() > 4096 || dn.contains(['\n', '\r', '\0']) {
            return Err(super::input_error(
                "検索・バインドDNを確認してください".into(),
            ));
        }
    }
    if input
        .bind_password
        .as_ref()
        .is_some_and(|password| password.len() > 4096)
    {
        return Err(super::input_error("パスワードが長すぎます".into()));
    }
    Ok(())
}

pub fn keycloak_error(_error: KeycloakAdminError) -> ApiError {
    // Keycloakの応答本文はバインドcredentialを含む可能性がある。
    ApiError::ServiceUnavailable("Keycloak / ADの設定・接続・証明書を確認してください".into())
}

/// 専用同期先のみを辿り、完全に取得できたスナップショットだけを確定する。
pub async fn sync(
    state: &AppState,
    ctx: &AuthContext,
    operation: uuid::Uuid,
) -> Result<usize, ApiError> {
    let store = state.directory.admin();
    let kc = KeycloakAdmin::from_config(&state.http, &state.config.auth).map_err(keycloak_error)?;
    kc.sync_ldap_groups(ctx).await.map_err(keycloak_error)?;
    let root = kc
        .find_group_path(&["tenants", ctx.tenant_id(), ctx.org(), "ad"])
        .await
        .map_err(keycloak_error)?
        .ok_or(ApiError::NotFound)?;
    let prefix = format!("/tenants/{}/{}/ad/", ctx.tenant_id(), ctx.org());
    let mut pending = vec![(root.id, None, 0usize)];
    let mut groups = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    use futures::{stream::FuturesUnordered, StreamExt};
    let fetch = |parent: String, parent_group: Option<String>, depth: usize| {
        let kc = &kc;
        async move {
            let mut rows = Vec::new();
            let mut first = 0;
            loop {
                let page = kc
                    .directory_children(&parent, first, 100)
                    .await
                    .map_err(keycloak_error)?;
                let count = page.len();
                rows.extend(page);
                if rows.len() > 10_000 {
                    return Err(super::input_error(
                        "一つの階層に10,000件を超えるグループがあります".into(),
                    ));
                }
                if count < 100 {
                    break;
                }
                first += 100;
            }
            Ok::<_, ApiError>((parent_group, depth, rows))
        }
    };
    let mut running = FuturesUnordered::new();
    loop {
        while running.len() < 4 {
            let Some((parent, parent_group, depth)) = pending.pop() else {
                break;
            };
            if depth > 128 {
                return Err(super::input_error(
                    "ADグループが128階層を超えています".into(),
                ));
            }
            running.push(fetch(parent, parent_group, depth));
        }
        let Some(result) = running.next().await else {
            break;
        };
        let (parent_group, depth, page) = result?;
        for group in page {
            if !seen.insert(group.id.clone()) || !group.path.starts_with(&prefix) {
                return Err(super::input_error(
                    "ADグループの階層または同期先を確認してください".into(),
                ));
            }
            let dn = group
                .attributes
                .get("distinguishedName")
                .and_then(|v| v.first())
                .cloned();
            let ou_path = dn
                .as_deref()
                .map(super::dn::ou_path)
                .transpose()
                .map_err(|error| super::input_error(error.to_string()))?
                .unwrap_or_default();
            if group.sub_group_count != Some(0) {
                pending.push((group.id.clone(), Some(group.id.clone()), depth + 1));
            }
            groups.push(AdGroup {
                id: group.id,
                name: group.name,
                claim_path: group.path,
                distinguished_name: dn,
                mapping_version: 0,
                role_ids: vec![],
                ou_path,
                parent_ids: parent_group.iter().cloned().collect(),
            });
            if groups.len() > 10_000 {
                return Err(super::input_error(
                    "一度に同期できるグループは10,000件です。検索ベースを絞ってください".into(),
                ));
            }
        }
    }
    store.replace_groups(ctx, &groups, Some(operation)).await?;
    Ok(groups.len())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]
    use super::*;
    #[test]
    fn connection_targets_are_not_arbitrary_urls() {
        let config: AuthConfig = serde_json::from_value(serde_json::json!({
            "directory_ldap_allowed_hosts": ["ad.example.test"], "issuer": "", "internal_base_url": null, "jwks_uri": null,
            "audience": "", "jwks_ttl_secs": 60, "client_id": "", "client_secret": null, "redirect_uri": "", "post_logout_redirect_uri": "", "scopes": "", "tenancy": "single", "tenant_id": "default"
        })).expect("config");
        let mut input = LdapConnectionInput {
            connection_url: "ldaps://ad.example.test:636".into(),
            users_dn: "OU=Users,DC=example".into(),
            groups_dn: "OU=Groups,DC=example".into(),
            bind_dn: "CN=reader,DC=example".into(),
            bind_password: None,
            preserve_group_inheritance: false,
        };
        assert!(validate(&config, &input).is_ok());
        for url in [
            "ldap://ad.example.test",
            "ldaps://other.test",
            "ldaps://ad.example.test:8443",
            "ldaps://user:pass@ad.example.test",
            "ldaps://ad.example.test/path",
            "ldaps://ad.example.test?x=1",
            "ldaps://ad.example.test#x",
            "http://127.0.0.1",
            "not a url",
        ] {
            input.connection_url = url.into();
            assert!(validate(&config, &input).is_err(), "{url}");
        }
    }
}
