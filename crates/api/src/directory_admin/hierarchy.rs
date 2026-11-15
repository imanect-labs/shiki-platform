//! 不変GUID名で分離したflatなKeycloak群から、DNでADの直接memberOf関係を再構成する。
use super::dn;
use crate::error::ApiError;
use std::collections::{BTreeMap, BTreeSet};
use storage::directory::admin::AdGroup;

pub(super) fn restore(
    groups: &mut [AdGroup],
    memberships: &BTreeMap<String, Vec<String>>,
    base: &str,
    enabled: bool,
) -> Result<(), ApiError> {
    let invalid = |error: &str| super::input_error(error.to_string());
    let base = dn::key(base).map_err(invalid)?;
    let mut identities = BTreeMap::new();
    for group in groups.iter() {
        let name = group
            .distinguished_name
            .as_deref()
            .ok_or_else(|| invalid("ADグループのDNがありません"))?;
        if identities
            .insert(dn::key(name).map_err(invalid)?, group.id.clone())
            .is_some()
        {
            return Err(invalid("ADグループのDNが重複しています"));
        }
    }
    for group in groups.iter_mut() {
        group.parent_ids.clear();
        if !enabled {
            continue;
        }
        let mut parents = BTreeSet::new();
        for parent in memberships.get(&group.id).into_iter().flatten() {
            let key = dn::key(parent).map_err(invalid)?;
            if let Some(id) = identities.get(&key) {
                parents.insert(id.clone());
            } else if key.within(&base) {
                return Err(invalid(
                    "検索範囲内の親グループを取得できませんでした。再同期してください",
                ));
            }
        }
        group.parent_ids = parents.into_iter().collect();
    }
    let graph: BTreeMap<_, _> = groups
        .iter()
        .map(|group| (group.id.as_str(), group.parent_ids.as_slice()))
        .collect();
    let mut done = BTreeMap::new();
    for id in graph.keys() {
        depth(id, &graph, &mut BTreeSet::new(), &mut done)?;
    }
    Ok(())
}
fn depth<'a>(
    id: &'a str,
    graph: &BTreeMap<&'a str, &'a [String]>,
    visiting: &mut BTreeSet<&'a str>,
    done: &mut BTreeMap<&'a str, usize>,
) -> Result<usize, ApiError> {
    if let Some(depth) = done.get(id) {
        return Ok(*depth);
    }
    if !visiting.insert(id) || visiting.len() > 128 {
        return Err(super::input_error(
            "ADグループの入れ子が循環しているか128階層を超えています".into(),
        ));
    }
    let mut result = 1;
    for parent in graph.get(id).into_iter().flat_map(|parents| parents.iter()) {
        result = result.max(depth(parent, graph, visiting, done)? + 1);
    }
    visiting.remove(id);
    if result > 128 {
        return Err(super::input_error(
            "ADグループが128階層を超えています".into(),
        ));
    }
    done.insert(id, result);
    Ok(result)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn group(id: usize) -> AdGroup {
        AdGroup {
            id: format!("g{id}"),
            name: format!("Group {id}"),
            claim_path: format!("/ad/g{id}"),
            distinguished_name: Some(format!("CN=g{id},OU=Groups,DC=example")),
            ou_path: vec!["Groups".into()],
            parent_ids: vec![],
            role_ids: vec![],
            mapping_version: 0,
        }
    }
    #[test]
    fn direct_membership_depth_allows_128_and_rejects_129_cycle_and_missing_parent() {
        let mut groups: Vec<_> = (1..=128).map(group).collect();
        let mut links = BTreeMap::new();
        for id in 2..=129 {
            links.insert(
                format!("g{id}"),
                vec![format!("cn=g{},ou=groups,dc=EXAMPLE", id - 1)],
            );
        }
        restore(&mut groups, &links, "OU=Groups,DC=example", true).unwrap();
        assert_eq!(groups[127].parent_ids, vec!["g127"]);
        groups.push(group(129));
        assert!(restore(&mut groups, &links, "OU=Groups,DC=example", true).is_err());
        groups.truncate(2);
        links.insert("g1".into(), vec!["CN=g2,OU=Groups,DC=example".into()]);
        assert!(restore(&mut groups, &links, "OU=Groups,DC=example", true).is_err());
        links.insert("g1".into(), vec!["CN=missing,OU=Groups,DC=example".into()]);
        assert!(restore(&mut groups, &links, "OU=Groups,DC=example", true).is_err());
        links.insert("g1".into(), vec!["CN=outside,OU=Other,DC=example".into()]);
        restore(&mut groups, &links, "OU=Groups,DC=example", true).unwrap();
        assert!(groups[0].parent_ids.is_empty());
        restore(&mut groups, &links, "OU=Groups,DC=example", false).unwrap();
        assert!(groups.iter().all(|g| g.parent_ids.is_empty()));
    }
}
