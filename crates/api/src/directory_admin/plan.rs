//! 自動作成はサーバ側のADスナップショットから決定する。安定IDで再実行しても増殖しない。
use super::super::{error::ApiError, AppState};
use authz::AuthContext;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use storage::directory::admin::{AdGroup, DirectoryChangeSet, GroupRoleMapping, RoleEdit};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Deserialize, ToSchema)]
pub struct AutoPlanInput {
    /// nullは全グループ。選択時も祖先は名前と階層だけ作成し、対応付けは選択した分だけ。
    pub group_ids: Option<Vec<String>>,
    pub hierarchy: AutoHierarchy,
}
#[derive(Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoHierarchy {
    Ou,
    Group,
}
#[derive(Serialize, ToSchema)]
pub struct DirectoryPlan {
    pub changes: DirectoryChangeSet,
    pub nodes: Vec<RoleEdit>,
    pub sources: Vec<AdGroup>,
    pub before_roles: Vec<storage::directory::admin::ManagedRole>,
    pub group_count: usize,
    pub existing_roles: usize,
}

pub async fn create(
    state: &AppState,
    ctx: &AuthContext,
    input: AutoPlanInput,
) -> Result<DirectoryPlan, ApiError> {
    let store = state.directory.admin();
    if input
        .group_ids
        .as_ref()
        .is_some_and(|ids| ids.is_empty() || ids.len() > 10_000)
    {
        return Err(super::input_error(
            "作成元を1〜10,000グループ選択してください".into(),
        ));
    }
    let all = store
        .groups_for_plan(
            ctx,
            input.group_ids.as_deref(),
            matches!(input.hierarchy, AutoHierarchy::Group),
        )
        .await
        .map_err(|e| super::public_error(e.into()))?;
    if all.len() > 10_000 {
        return Err(super::input_error("作成元は10,000グループまでです".into()));
    }
    let groups: BTreeMap<_, _> = all.iter().map(|g| (g.id.clone(), g)).collect();
    let selected: BTreeSet<String> = input.group_ids.map_or_else(
        || groups.keys().cloned().collect(),
        |ids| ids.into_iter().collect(),
    );
    if selected.iter().any(|id| !groups.contains_key(id)) {
        return Err(ApiError::Conflict);
    }
    let desired = build(ctx, &groups, &selected, input.hierarchy)?;
    let ids: Vec<_> = desired.keys().cloned().collect();
    let mut existing = BTreeMap::new();
    for role in store.role_definitions(ctx, &ids).await? {
        existing.insert(role.id.clone(), role);
    }
    let mut roles = Vec::new();
    for role in desired.values() {
        if let Some(current) = existing.get(&role.id) {
            if current.label != role.label || current.parent_id != role.parent_id {
                return Err(super::input_error(format!(
                    "自動作成済みロール「{}」は変更されています。ロール管理で確認してください",
                    current.label
                )));
            }
        } else {
            roles.push(role.clone());
        }
    }
    let mut mappings = Vec::new();
    for id in &selected {
        let group = groups.get(id).ok_or(ApiError::Conflict)?;
        let target = stable_id(ctx, "group", id);
        if !group.role_ids.contains(&target) {
            let mut role_ids = group.role_ids.clone();
            role_ids.push(target);
            mappings.push(GroupRoleMapping {
                group_id: id.clone(),
                expected_version: group.mapping_version,
                role_ids,
            });
        }
    }
    let changes = DirectoryChangeSet {
        idempotency_key: Uuid::new_v4(),
        roles,
        mappings,
    };
    let changes = if changes.roles.is_empty() && changes.mappings.is_empty() {
        changes
    } else {
        store
            .validate(ctx, &changes)
            .await
            .map_err(|e| super::public_error(e.into()))?
    };
    Ok(DirectoryPlan {
        changes,
        before_roles: vec![],
        nodes: desired.values().cloned().collect(),
        sources: selected
            .iter()
            .filter_map(|id| groups.get(id).map(|g| (*g).clone()))
            .collect(),
        group_count: selected.len(),
        existing_roles: existing.len(),
    })
}

fn stable_id(ctx: &AuthContext, kind: &str, key: &str) -> String {
    format!(
        "{}{}",
        storage::directory::admin::role_id_prefix(ctx),
        hex::encode(Sha256::digest(format!("{kind}\0{key}").as_bytes()))
    )
}

fn build(
    ctx: &AuthContext,
    groups: &BTreeMap<String, &AdGroup>,
    selected: &BTreeSet<String>,
    hierarchy: AutoHierarchy,
) -> Result<BTreeMap<String, RoleEdit>, ApiError> {
    let mut desired = BTreeMap::new();
    for id in selected {
        let mut path = BTreeSet::new();
        add_group(ctx, id, groups, hierarchy, &mut path, &mut desired)?;
    }
    Ok(desired)
}

fn add_group(
    ctx: &AuthContext,
    id: &str,
    groups: &BTreeMap<String, &AdGroup>,
    hierarchy: AutoHierarchy,
    visiting: &mut BTreeSet<String>,
    desired: &mut BTreeMap<String, RoleEdit>,
) -> Result<String, ApiError> {
    let role_id = stable_id(ctx, "group", id);
    if desired.contains_key(&role_id) {
        return Ok(role_id);
    }
    if !visiting.insert(id.to_string()) || visiting.len() > 128 {
        return Err(super::input_error(
            "ADグループに循環または128階層を超える関係があります".into(),
        ));
    }
    let group = groups.get(id).ok_or_else(|| {
        super::input_error("親グループが同期されていません。再同期してください".into())
    })?;
    let parent = match hierarchy {
        AutoHierarchy::Ou => {
            if group.distinguished_name.is_none() {
                return Err(super::input_error(format!(
                    "「{}」のdistinguishedNameがありません。ADを再同期してください",
                    group.name
                )));
            }
            let mut parent = None;
            let mut path = Vec::new();
            for name in &group.ou_path {
                path.push(name.clone());
                let key =
                    serde_json::to_string(&path).map_err(|e| ApiError::Internal(e.to_string()))?;
                let id = stable_id(ctx, "ou", &key);
                desired.entry(id.clone()).or_insert_with(|| RoleEdit {
                    id: id.clone(),
                    label: name.clone(),
                    parent_id: parent.clone(),
                    expected_version: 0,
                });
                parent = Some(id);
            }
            parent
        }
        AutoHierarchy::Group => {
            if group.parent_ids.len() > 1 {
                return Err(super::input_error(format!(
                    "「{}」には複数の親があります。OU階層を選択してください",
                    group.name
                )));
            }
            match group.parent_ids.first() {
                Some(parent) => Some(add_group(
                    ctx, parent, groups, hierarchy, visiting, desired,
                )?),
                None => None,
            }
        }
    };
    desired.insert(
        role_id.clone(),
        RoleEdit {
            id: role_id.clone(),
            label: group.name.clone(),
            parent_id: parent,
            expected_version: 0,
        },
    );
    if desired.len() > 10_000 {
        return Err(super::input_error(
            "作成されるロールが10,000件を超えます。対象を絞ってください".into(),
        ));
    }
    visiting.remove(id);
    Ok(role_id)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::pedantic)]
    use super::*;
    fn ctx(org: &str) -> AuthContext {
        AuthContext::new(
            authz::Principal {
                id: "a".into(),
                kind: authz::PrincipalKind::User,
                email: None,
                roles: vec![],
                groups: vec![],
                tenant_id: Some("tenant".into()),
            },
            org.to_string(),
            authz::TenantId::parse("tenant").unwrap(),
        )
    }
    fn group(id: &str, parent: &[&str]) -> AdGroup {
        AdGroup {
            id: id.into(),
            name: id.into(),
            claim_path: format!("/tenants/tenant/org/ad-guid-v1/{id}"),
            distinguished_name: Some(format!("CN={id},OU=営業,OU=本社,DC=example")),
            ou_path: vec!["本社".into(), "営業".into()],
            parent_ids: parent.iter().map(|s| (*s).to_string()).collect(),
            role_ids: vec![],
            mapping_version: 0,
        }
    }
    #[test]
    fn ou_and_membership_are_separate_and_ids_are_stable() {
        let a = group("a", &[]);
        let b = group("b", &["a"]);
        let groups = BTreeMap::from([("a".to_string(), &a), ("b".to_string(), &b)]);
        let selected = BTreeSet::from(["b".to_string()]);
        let ou = build(&ctx("org"), &groups, &selected, AutoHierarchy::Ou).unwrap();
        assert_eq!(ou.len(), 3);
        assert!(!ou.values().any(|r| r.label == "a"));
        let membership = build(&ctx("org"), &groups, &selected, AutoHierarchy::Group).unwrap();
        assert_eq!(membership.len(), 2);
        assert_eq!(
            membership
                .get(&stable_id(&ctx("org"), "group", "b"))
                .unwrap()
                .parent_id,
            Some(stable_id(&ctx("org"), "group", "a"))
        );
        assert_eq!(
            stable_id(&ctx("org"), "group", "a"),
            stable_id(&ctx("org"), "group", "a")
        );
        assert_ne!(
            stable_id(&ctx("org"), "group", "a"),
            stable_id(&ctx("other"), "group", "a")
        );
    }
    #[test]
    fn ambiguous_or_missing_hierarchy_is_blocked() {
        for parents in [vec!["a"], vec!["missing"], vec!["x", "y"]] {
            let a = group("a", &parents);
            let groups = BTreeMap::from([("a".into(), &a)]);
            assert!(build(
                &ctx("org"),
                &groups,
                &BTreeSet::from(["a".into()]),
                AutoHierarchy::Group
            )
            .is_err());
        }
        let mut a = group("a", &[]);
        a.distinguished_name = None;
        assert!(build(
            &ctx("org"),
            &BTreeMap::from([("a".into(), &a)]),
            &BTreeSet::from(["a".into()]),
            AutoHierarchy::Ou
        )
        .is_err());
    }
}
