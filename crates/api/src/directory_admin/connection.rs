//! 接続先は運用者の許可リストに限定する。組織管理者の任意ネットワーク探索を防ぐ。
use crate::{
    config::AuthConfig,
    error::ApiError,
    keycloak_admin::{ldap::LdapConnectionInput, KeycloakAdmin, KeycloakAdminError},
    AppState,
};
use authz::AuthContext;
use storage::directory::admin::{AdGroup, AD_GROUP_ROOT};

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

/// 開始記録を保持したまま外部設定を変更する。不確定な結果は確認済み復旧まで隔離する。
pub async fn save(
    state: &AppState,
    ctx: &AuthContext,
    input: &LdapConnectionInput,
    trace: Option<&str>,
) -> Result<crate::keycloak_admin::ldap::LdapConnectionStatus, ApiError> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = state.directory.admin();
    let lease = store.lease(ctx).await?.ok_or(ApiError::Conflict)?;
    if store.has_active_operation(ctx).await? {
        return Err(ApiError::Conflict);
    }
    if store.connection_recovery_required(ctx).await? && !input.recovery_confirmed {
        return Err(ApiError::invalid_input("前回の接続設定の保存結果が不明です。運用担当者が旧処理の停止とKeycloakの処理完了を確認してから、復旧確認にチェックしてください。"));
    }
    let attempt = store
        .begin_connection_change(ctx, &lease, input.recovery_confirmed, trace)
        .await?;
    let mutating = AtomicBool::new(false);
    let result = lease.while_live(async {
        let kc = KeycloakAdmin::from_config(&state.http, &state.config.auth).map_err(keycloak_error)?;
        let previous = kc.ldap_connection(ctx).await.map_err(keycloak_error)?;
        let identity_changed = !previous.identity_current;
        if previous.configured && identity_changed && !input.identity_reset_confirmed {
            return Err(ApiError::invalid_input("旧形式のAD連携をGUID方式へ更新するには、既存の対応付けを解除して再設定する確認が必要です"));
        }
        let changed = previous.connection_url.as_deref() != Some(input.connection_url.as_str())
            || previous.users_dn.as_deref() != Some(input.users_dn.as_str())
            || previous.groups_dn.as_deref() != Some(input.groups_dn.as_str())
            || previous.preserve_group_inheritance != input.preserve_group_inheritance
            || identity_changed;
        if changed && store.has_unfinished_mappings(ctx).await? {
            return Err(ApiError::invalid_input("停止中の反映を再実行して完了してから、接続先・検索DN・階層設定を変更してください。バインドDNとパスワードは変更できます。"));
        }
        // 間違ったcredentialで現在の有効なスナップショットを無効化しない。
        kc.test_ldap(ctx, input).await.map_err(keycloak_error)?;
        lease.verify().await?;
        if changed {
            store.configuration_event(ctx, if identity_changed { "identity_invalidated" } else { "snapshot_invalidated" }, trace).await?;
        }
        mutating.store(true, Ordering::SeqCst);
        let status = kc.save_ldap(ctx, input).await.map_err(keycloak_error)?;
        lease.verify().await?;
        Ok::<_, ApiError>(status)
    }).await;
    let result = result
        .map_err(ApiError::from)
        .and_then(std::convert::identity);
    // エラー前に書込みを送っていた場合や接続断の場合は開始記録を残す。
    if result.is_ok() || (!mutating.load(Ordering::SeqCst) && lease.verify().await.is_ok()) {
        store.finish_connection_change(ctx, attempt).await?;
    }
    store
        .configuration_event(
            ctx,
            if result.is_ok() {
                "succeeded"
            } else {
                "failed"
            },
            trace,
        )
        .await?;
    result
}

/// 専用同期先のみを辿り、完全に取得できたスナップショットだけを確定する。
pub async fn sync(
    state: &AppState,
    ctx: &AuthContext,
    operation: uuid::Uuid,
    lease: &storage::directory::admin::DirectoryLease,
    uncertain: &std::sync::atomic::AtomicBool,
) -> Result<usize, ApiError> {
    let store = state.directory.admin();
    let kc = KeycloakAdmin::from_config(&state.http, &state.config.auth).map_err(keycloak_error)?;
    lease.verify().await?;
    let status = kc.ldap_connection(ctx).await.map_err(keycloak_error)?;
    if !status.configured || !status.enabled {
        return Err(super::input_error(
            "AD接続を設定してから同期してください".into(),
        ));
    }
    uncertain.store(true, std::sync::atomic::Ordering::SeqCst);
    kc.sync_ldap_groups(ctx).await.map_err(keycloak_error)?;
    uncertain.store(false, std::sync::atomic::Ordering::SeqCst);
    let root = kc
        .find_group_path(&["tenants", ctx.tenant_id(), ctx.org(), AD_GROUP_ROOT])
        .await
        .map_err(keycloak_error)?
        .ok_or(ApiError::NotFound)?;
    let prefix = format!(
        "/tenants/{}/{}/{AD_GROUP_ROOT}/",
        ctx.tenant_id(),
        ctx.org()
    );
    let mut pending = vec![(root.id, None, 0usize)];
    let mut groups = Vec::new();
    let mut memberships = std::collections::BTreeMap::new();
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
        let (_parent_group, depth, page) = result?;
        for group in page {
            if depth >= 128 {
                return Err(super::input_error(
                    "ADグループが128階層を超えています".into(),
                ));
            }
            if !seen.insert(group.id.clone()) || !group.path.starts_with(&prefix) {
                return Err(super::input_error(
                    "ADグループの階層または同期先を確認してください".into(),
                ));
            }
            memberships.insert(
                group.id.clone(),
                group
                    .attributes
                    .get("memberOf")
                    .cloned()
                    .unwrap_or_default(),
            );
            if group.sub_group_count != Some(0) {
                pending.push((group.id.clone(), Some(group.id.clone()), depth + 1));
            }
            groups.push(snapshot_group(group)?);
            if groups.len() > 10_000 {
                return Err(super::input_error(
                    "一度に同期できるグループは10,000件です。検索ベースを絞ってください".into(),
                ));
            }
        }
    }
    super::hierarchy::restore(
        &mut groups,
        &memberships,
        status
            .groups_dn
            .as_deref()
            .ok_or_else(|| super::input_error("ADグループ検索DNが未設定です".into()))?,
        status.preserve_group_inheritance,
    )?;
    lease.verify().await?;
    store.replace_groups(ctx, &groups, Some(operation)).await?;
    uncertain.store(false, std::sync::atomic::Ordering::SeqCst);
    Ok(groups.len())
}

fn snapshot_group(group: crate::keycloak_admin::KcGroup) -> Result<AdGroup, ApiError> {
    if !uuid::Uuid::parse_str(&group.name).is_ok_and(|id| id.to_string() == group.name) {
        return Err(super::input_error(
            "ADグループの不変GUIDを取得できませんでした".into(),
        ));
    }
    let dn = group
        .attributes
        .get("distinguishedName")
        .and_then(|v| v.first())
        .cloned()
        .ok_or_else(|| super::input_error("ADグループのDNを取得できませんでした".into()))?;
    let ou_path = super::dn::ou_path(&dn).map_err(|error| super::input_error(error.to_string()))?;
    if ou_path.len() > 128 {
        return Err(super::input_error("OUが128階層を超えています".into()));
    }
    let name = group
        .attributes
        .get("cn")
        .and_then(|v| v.first())
        .cloned()
        .map_or_else(|| super::dn::common_name(&dn), Ok)
        .map_err(|error| super::input_error(error.to_string()))?;
    Ok(AdGroup {
        id: group.id,
        name,
        claim_path: group.path,
        distinguished_name: Some(dn),
        mapping_version: 0,
        role_ids: vec![],
        ou_path,
        parent_ids: vec![],
    })
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
            recovery_confirmed: false,
            identity_reset_confirmed: false,
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
