//! CSVの全行をサーバで検証し、未指定グループを変更しない置換案を返す。
use super::plan::DirectoryPlan;
use crate::{error::ApiError, AppState};
use authz::AuthContext;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use storage::directory::admin::{DirectoryChangeSet, GroupRoleMapping};
use utoipa::ToSchema;
#[derive(Deserialize, ToSchema)]
pub struct DirectoryImportInput {
    pub csv: String,
}

pub async fn create(
    state: &AppState,
    ctx: &AuthContext,
    input: DirectoryImportInput,
) -> Result<DirectoryPlan, ApiError> {
    let imported = parse(&input.csv)?;
    let ids: Vec<_> = imported.keys().cloned().collect();
    let store = state.directory.admin();
    let known = store.groups_for_plan(ctx, Some(&ids), false).await?;
    let known: BTreeMap<_, _> = known.into_iter().map(|g| (g.id.clone(), g)).collect();
    let mut mappings = Vec::new();
    for (group_id, role_ids) in imported {
        let group = known.get(&group_id).ok_or_else(|| {
            super::input_error(format!("グループID「{group_id}」が見つかりません"))
        })?;
        let role_ids: Vec<_> = role_ids.into_iter().collect();
        if role_ids != group.role_ids {
            mappings.push(GroupRoleMapping {
                group_id,
                role_ids,
                expected_version: group.mapping_version,
            });
        }
    }
    let changes = DirectoryChangeSet {
        idempotency_key: uuid::Uuid::new_v4(),
        roles: vec![],
        mappings,
    };
    let changes = if changes.mappings.is_empty() {
        changes
    } else {
        store
            .validate(ctx, &changes)
            .await
            .map_err(|e| super::public_error(e.into()))?
    };
    Ok(DirectoryPlan {
        group_count: ids.len(),
        existing_roles: 0,
        changes,
        nodes: vec![],
        before_roles: vec![],
        sources: known.into_values().collect(),
    })
}
fn parse(input: &str) -> Result<BTreeMap<String, BTreeSet<String>>, ApiError> {
    if input.len() > 1_000_000 {
        return Err(super::input_error("CSVは1MB以内にしてください".into()));
    }
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(input.trim_start_matches('\u{feff}').as_bytes());
    if reader
        .headers()
        .map_err(|_| super::input_error("CSVヘッダーが不正です".into()))?
        .iter()
        .collect::<Vec<_>>()
        != ["group_id", "role_id"]
    {
        return Err(super::input_error(
            "ヘッダーはgroup_id,role_idを指定してください".into(),
        ));
    }
    let mut result: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut clear = BTreeSet::new();
    for (index, record) in reader.records().enumerate() {
        if index >= 1000 {
            return Err(super::input_error("CSVは1,000行までです".into()));
        }
        let record = record.map_err(|_| {
            super::input_error(format!("データ{}行目の列・引用符が不正です", index + 1))
        })?;
        let group = record.get(0).unwrap_or("");
        let role = record.get(1).unwrap_or("");
        if group.is_empty() || record.len() != 2 || clear.contains(group) {
            return Err(super::input_error(format!(
                "データ{}行目のグループ・解除指定が不正です",
                index + 1
            )));
        }
        if role.is_empty() {
            if result.contains_key(group) {
                return Err(super::input_error(
                    "同じグループの解除・付与は混在できません".into(),
                ));
            }
            clear.insert(group.to_string());
            result.insert(group.to_string(), BTreeSet::new());
        } else if !result
            .entry(group.to_string())
            .or_default()
            .insert(role.to_string())
        {
            return Err(super::input_error("同じ対応が重複しています".into()));
        }
    }
    if result.is_empty() {
        return Err(super::input_error("CSVのデータがありません".into()));
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]
    use super::*;
    #[test]
    fn csv_validation() {
        assert_eq!(
            parse("\u{feff}group_id,role_id\r\ng1,r1\r\ng1,r2\r\ng2,\r\n")
                .expect("valid")
                .len(),
            2
        );
        assert!(parse("group_id,role_id\n\"a,b\",\"r\"\"1\"\n").is_ok());
        for bad in [
            "wrong,header\ng,r",
            "group_id,role_id\n",
            "group_id,role_id\n,r",
            "group_id,role_id\ng,r\ng,r",
            "group_id,role_id\ng,\ng,r",
            "group_id,role_id\ng,r\ng,",
            "group_id,role_id\ng,r,extra",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
