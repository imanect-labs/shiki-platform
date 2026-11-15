//! 手編集の確認も、サーバで参照と版を検証し、変更前と変更後を返す。
use super::{plan::DirectoryPlan, public_error};
use crate::{error::ApiError, AppState};
use authz::AuthContext;
use std::collections::{BTreeMap, BTreeSet};
use storage::directory::admin::{DirectoryChangeSet, RoleEdit};
pub async fn create(
    state: &AppState,
    ctx: &AuthContext,
    request: &DirectoryChangeSet,
) -> Result<DirectoryPlan, ApiError> {
    let store = state.directory.admin();
    let changes = store
        .validate(ctx, request)
        .await
        .map_err(|e| public_error(e.into()))?;
    let group_ids: Vec<_> = changes
        .mappings
        .iter()
        .map(|m| m.group_id.clone())
        .collect();
    let sources = store.groups_for_plan(ctx, Some(&group_ids), false).await?;
    let seed: BTreeSet<String> = changes
        .roles
        .iter()
        .flat_map(|r| std::iter::once(r.id.clone()).chain(r.parent_id.clone()))
        .collect();
    let mut before_roles = Vec::new();
    let mut ancestors = BTreeSet::new();
    for role in store
        .roles_for_preview(ctx, &seed.into_iter().collect::<Vec<_>>())
        .await?
    {
        ancestors.extend(role.path_ids.clone());
        before_roles.push(role);
    }
    let ancestors: Vec<_> = ancestors.into_iter().collect();
    let definitions = store.role_definitions(ctx, &ancestors).await?;
    let mut nodes: BTreeMap<_, _> = definitions
        .into_iter()
        .map(|r| {
            (
                r.id.clone(),
                RoleEdit {
                    id: r.id,
                    label: r.label,
                    parent_id: r.parent_id,
                    expected_version: r.version,
                },
            )
        })
        .collect();
    for role in &changes.roles {
        nodes.insert(role.id.clone(), role.clone());
    }
    before_roles.retain(|r| changes.roles.iter().any(|edit| edit.id == r.id));
    Ok(DirectoryPlan {
        group_count: sources.len(),
        existing_roles: nodes.len().saturating_sub(changes.roles.len()),
        nodes: nodes.into_values().collect(),
        before_roles,
        sources,
        changes,
    })
}
