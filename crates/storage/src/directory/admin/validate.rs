use super::{DirectoryAdminStore, DirectoryChangeSet, ManagedRole, RoleEdit};
use crate::{db_tenant::begin_tenant_tx, error::StorageError};
use authz::AuthContext;
use std::collections::{BTreeMap, BTreeSet};

impl DirectoryAdminStore {
    /// 関連IDと祖先だけをまとめて取得し、外部システムへ書く前に全体を検証する。
    pub async fn validate(
        &self,
        ctx: &AuthContext,
        changes: &DirectoryChangeSet,
    ) -> Result<DirectoryChangeSet, StorageError> {
        let ChangeIndex {
            seed,
            edits,
            groups,
        } = index_changes(ctx, changes)?;
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let known: Vec<ManagedRole> = sqlx::query_as(
            "WITH RECURSIVE descendants AS ( \
               SELECT role_id FROM directory_managed_role WHERE tenant_id = $1 AND org = $2 AND role_id = ANY($4) \
               UNION SELECT m.role_id FROM directory_managed_role m JOIN descendants d ON m.parent_id = d.role_id WHERE m.tenant_id = $1 AND m.org = $2 \
             ), affected AS ( \
               SELECT d.role_id AS id, coalesce(m.label, d.display_name) AS label, m.parent_id, coalesce(m.version, 0) AS version \
               FROM directory_role d LEFT JOIN directory_managed_role m \
                 ON m.tenant_id = d.tenant_id AND m.org = d.org AND m.role_id = d.role_id \
               WHERE d.tenant_id = $1 AND d.org = $2 AND (d.role_id = ANY($3) OR d.role_id IN (SELECT role_id FROM descendants)) \
               UNION SELECT m.role_id, m.label, m.parent_id, m.version FROM directory_managed_role m \
                 JOIN affected a ON a.parent_id = m.role_id WHERE m.tenant_id = $1 AND m.org = $2 \
             ) SELECT id, label, parent_id, version FROM affected LIMIT 20001",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(seed.into_iter().collect::<Vec<_>>()).bind(edits.keys().collect::<Vec<_>>()).fetch_all(&mut *tx).await?;
        if known.len() > 20_000 {
            return Err(StorageError::Invalid(
                "変更の影響範囲は20,000ロール以内にしてください".into(),
            ));
        }
        let slots: BTreeMap<_, _> = known
            .iter()
            .filter(|r| edits.contains_key(&r.id))
            .map(|r| ((r.parent_id.clone(), r.label.to_lowercase()), r.id.clone()))
            .collect();
        let mut graph: BTreeMap<String, ManagedRole> =
            known.into_iter().map(|r| (r.id.clone(), r)).collect();
        for role in &changes.roles {
            if !super::is_managed_id(ctx, &role.id) {
                return Err(StorageError::Invalid(
                    "このロールはIdPで管理されています。Shiki管理のロールを作成してください".into(),
                ));
            }
            if graph.get(&role.id).is_some_and(|r| r.version == 0) {
                return Err(StorageError::Invalid(
                    "IdP管理または未確定のロールは編集できません".into(),
                ));
            }
            if graph.get(&role.id).map_or(0, |r| r.version) != role.expected_version {
                return Err(StorageError::Conflict);
            }
            if let Some(parent) = &role.parent_id {
                if !super::is_managed_id(ctx, parent) {
                    return Err(StorageError::Invalid(
                        "親にはShiki管理のロールを指定してください".into(),
                    ));
                }
                if graph.get(parent).is_some_and(|r| r.version == 0) && !edits.contains_key(parent)
                {
                    return Err(StorageError::Invalid(
                        "親には確定済みのShiki管理ロールを指定してください".into(),
                    ));
                }
                if !graph.contains_key(parent) && !edits.contains_key(parent) {
                    return Err(StorageError::NotFound);
                }
            }
            graph.insert(
                role.id.clone(),
                ManagedRole {
                    id: role.id.clone(),
                    label: role.label.clone(),
                    parent_id: role.parent_id.clone(),
                    version: role.expected_version,
                    path_ids: vec![],
                    path_labels: vec![],
                    child_count: 0,
                },
            );
        }
        let collisions: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM directory_managed_role m \
             JOIN jsonb_to_recordset($3) AS r(id text, label text, parent_id text) \
             ON m.parent_id IS NOT DISTINCT FROM r.parent_id AND lower(m.label) = lower(r.label) AND m.role_id <> r.id \
             WHERE m.tenant_id = $1 AND m.org = $2 AND NOT (m.role_id = ANY($4)))",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(serde_json::to_value(&changes.roles).map_err(|e| StorageError::Invalid(e.to_string()))?).bind(edits.keys().cloned().collect::<Vec<_>>()).fetch_one(&mut *tx).await?;
        if collisions {
            return Err(StorageError::Invalid(
                "同じ親に同名のロールがあります。名前または親を変更してください".into(),
            ));
        }
        let group_versions: Vec<(String, i64)> = sqlx::query_as(
            "SELECT group_id, mapping_version FROM directory_ad_group WHERE tenant_id = $1 AND org = $2 AND active AND group_id = ANY($3)",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(groups.into_iter().collect::<Vec<_>>()).fetch_all(&mut *tx).await?;
        let group_versions: BTreeMap<_, _> = group_versions.into_iter().collect();
        for mapping in &changes.mappings {
            if group_versions.get(&mapping.group_id) != Some(&mapping.expected_version) {
                return Err(StorageError::Conflict);
            }
            if mapping
                .role_ids
                .iter()
                .any(|id| graph.get(id).is_some_and(|r| r.version == 0) && !edits.contains_key(id))
            {
                return Err(StorageError::Invalid(
                    "対応先には確定済みのShiki管理ロールを指定してください".into(),
                ));
            }
            if mapping.role_ids.iter().any(|id| !graph.contains_key(id)) {
                return Err(StorageError::NotFound);
            }
        }
        let fresh: Vec<_> = changes
            .roles
            .iter()
            .filter(|r| r.expected_version == 0)
            .map(|r| &r.id)
            .collect();
        let collision: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM directory_role WHERE tenant_id = $1 AND org <> $2 AND role_id = ANY($3))").bind(ctx.tenant_id()).bind(ctx.org()).bind(fresh).fetch_one(&mut *tx).await?;
        if collision {
            return Err(StorageError::Conflict);
        }
        tx.commit().await?;
        let mut done = BTreeMap::new();
        let mut ordered = Vec::new();
        for id in graph.keys() {
            visit(
                id,
                &graph,
                &edits,
                &mut BTreeSet::new(),
                &mut done,
                &mut ordered,
            )?;
        }
        Ok(DirectoryChangeSet {
            idempotency_key: changes.idempotency_key,
            roles: order_by_slots(ordered, &graph, &slots)?,
            mappings: changes.mappings.clone(),
        })
    }
}

fn visit(
    id: &str,
    graph: &BTreeMap<String, ManagedRole>,
    edits: &BTreeMap<String, RoleEdit>,
    visiting: &mut BTreeSet<String>,
    done: &mut BTreeMap<String, usize>,
    ordered: &mut Vec<RoleEdit>,
) -> Result<(), StorageError> {
    if done.contains_key(id) {
        return Ok(());
    }
    if !visiting.insert(id.to_string()) || visiting.len() > 128 {
        return Err(StorageError::Invalid(
            "ロール階層が循環しているか、128階層を超えています".into(),
        ));
    }
    let depth = if let Some(parent) = graph.get(id).and_then(|r| r.parent_id.as_deref()) {
        visit(parent, graph, edits, visiting, done, ordered)?;
        done.get(parent).copied().unwrap_or(0) + 1
    } else {
        1
    };
    if depth > 128 {
        return Err(StorageError::Invalid(
            "ロール階層は128階層以内にしてください".into(),
        ));
    }
    visiting.remove(id);
    done.insert(id.to_string(), depth);
    if let Some(edit) = edits.get(id) {
        ordered.push(edit.clone());
    }
    Ok(())
}

/// 最終状態の重複だけを拒否し、改名で空く名前は先に解放する。名前の交換は中間名を使う。
fn order_by_slots(
    roles: Vec<RoleEdit>,
    graph: &BTreeMap<String, ManagedRole>,
    slots: &BTreeMap<(Option<String>, String), String>,
) -> Result<Vec<RoleEdit>, StorageError> {
    let edits: BTreeMap<_, _> = roles.into_iter().map(|r| (r.id.clone(), r)).collect();
    let mut dependencies: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut reverse: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for role in edits.values() {
        let mut deps = BTreeSet::new();
        let mut parent = role.parent_id.as_deref();
        while let Some(id) = parent {
            if edits.contains_key(id) {
                deps.insert(id.to_string());
            }
            parent = graph.get(id).and_then(|r| r.parent_id.as_deref());
        }
        if let Some(occupant) = slots.get(&(role.parent_id.clone(), role.label.to_lowercase())) {
            if occupant != &role.id {
                deps.insert(occupant.clone());
            }
        }
        for dep in &deps {
            reverse
                .entry(dep.clone())
                .or_default()
                .push(role.id.clone());
        }
        dependencies.insert(role.id.clone(), deps);
    }
    let mut ready: BTreeSet<String> = dependencies
        .iter()
        .filter(|(_, deps)| deps.is_empty())
        .map(|(id, _)| id.clone())
        .collect();
    let mut ordered = Vec::with_capacity(edits.len());
    while let Some(id) = ready.pop_first() {
        if let Some(role) = edits.get(&id) {
            ordered.push(role.clone());
        }
        if let Some(dependents) = reverse.get(&id) {
            for dependent in dependents {
                if let Some(deps) = dependencies.get_mut(dependent) {
                    deps.remove(&id);
                    if deps.is_empty() {
                        ready.insert(dependent.clone());
                    }
                }
            }
        }
    }
    if ordered.len() != edits.len() {
        return Err(StorageError::Invalid(
            "名前の入替または変更順序が循環しています。中間名を使って段階的に変更してください"
                .into(),
        ));
    }
    Ok(ordered)
}

struct ChangeIndex {
    seed: BTreeSet<String>,
    edits: BTreeMap<String, RoleEdit>,
    groups: BTreeSet<String>,
}
fn index_changes(
    ctx: &AuthContext,
    changes: &DirectoryChangeSet,
) -> Result<ChangeIndex, StorageError> {
    if changes
        .mappings
        .iter()
        .map(|m| m.role_ids.len())
        .sum::<usize>()
        > 20_000
    {
        return Err(StorageError::Invalid(
            "一度に変更できる対応表のロールID合計は20,000件までです".into(),
        ));
    }
    if changes.roles.len() + changes.mappings.len() > 20_000
        || changes.roles.is_empty() && changes.mappings.is_empty()
    {
        return Err(StorageError::Invalid("変更件数は1〜20,000件です".into()));
    }
    let mut seed = BTreeSet::new();
    let mut edits = BTreeMap::new();
    let mut names = BTreeSet::new();
    for role in &changes.roles {
        authz::validate_local_id(&role.id)
            .map_err(|_| StorageError::Invalid("ロールIDが不正です".into()))?;
        if role.id.len() > 255
            || role.label.trim().is_empty()
            || role.label.chars().count() > 80
            || role.expected_version < 0
        {
            return Err(StorageError::Invalid(
                "ロールID・名前・版を確認してください".into(),
            ));
        }
        if edits.insert(role.id.clone(), role.clone()).is_some()
            || !names.insert((role.parent_id.clone(), role.label.to_lowercase()))
        {
            return Err(StorageError::Conflict);
        }
        seed.insert(role.id.clone());
        if let Some(parent) = &role.parent_id {
            seed.insert(parent.clone());
        }
    }
    let mut groups = BTreeSet::new();
    for mapping in &changes.mappings {
        if !groups.insert(mapping.group_id.clone())
            || mapping.role_ids.len() > 1000
            || mapping.expected_version < 0
        {
            return Err(StorageError::Invalid("グループ対応表が不正です".into()));
        }
        let mut unique = BTreeSet::new();
        for id in &mapping.role_ids {
            if !super::is_managed_id(ctx, id) {
                return Err(StorageError::Invalid(
                    "IdP管理ロールへの所属変更はIdPで行ってください".into(),
                ));
            }
            if !unique.insert(id) {
                return Err(StorageError::Invalid(
                    "対応するロールが重複しています".into(),
                ));
            }
            seed.insert(id.clone());
        }
    }
    Ok(ChangeIndex {
        seed,
        edits,
        groups,
    })
}
