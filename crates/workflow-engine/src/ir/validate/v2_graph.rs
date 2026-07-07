//! V2: グラフ制約（id 一意・エッジ参照・DAG・入エッジ制約・領域閉包・到達性・ir.md §8）。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::ValidationError;
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

/// V2 検証を実行しエラーを収集する。
pub(super) fn check(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    // id 一意・id 形式。
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    // 静的パターンなので必ず成功する。失敗時は id 形式チェックを飛ばす（fail-open せず後段で捕捉）。
    let id_re = regex::Regex::new(crate::ir::node::NODE_ID_RE).ok();
    for node in &ir.nodes {
        if id_re.as_ref().is_some_and(|re| !re.is_match(&node.id)) {
            errors.push(
                ValidationError::new("ir.bad_node_id", format!("不正なノード id: {}", node.id))
                    .at_node(&node.id),
            );
        }
        if !ids.insert(node.id.as_str()) {
            errors.push(
                ValidationError::new(
                    "ir.duplicate_node_id",
                    format!("ノード id が重複: {}", node.id),
                )
                .at_node(&node.id),
            );
        }
    }

    // parent 参照が map ノードを指すか（領域閉包の前提）。
    let node_types: HashMap<&str, Option<NodeType>> = ir
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), NodeType::parse(&n.node_type)))
        .collect();
    for node in &ir.nodes {
        if let Some(parent) = &node.parent {
            match node_types.get(parent.as_str()) {
                Some(Some(NodeType::ControlMap)) => {}
                Some(_) => errors.push(
                    ValidationError::new(
                        "ir.bad_region",
                        format!("parent {parent} は control.map ではありません"),
                    )
                    .at_node(&node.id),
                ),
                None => errors.push(
                    ValidationError::new(
                        "ir.bad_region",
                        format!("parent {parent} が存在しません"),
                    )
                    .at_node(&node.id),
                ),
            }
        }
    }

    // エッジ参照存在＋領域閉包（同一領域内でしか繋げない）。
    let parent_of: HashMap<&str, Option<&str>> = ir
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.parent.as_deref()))
        .collect();
    let mut in_edges: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &ir.edges {
        let edge_label = format!("{} -> {}", e.from, e.to);
        let from_ok = ids.contains(e.from.as_str());
        let to_ok = ids.contains(e.to.as_str());
        if !from_ok || !to_ok {
            errors.push(
                ValidationError::new(
                    "ir.bad_edge_ref",
                    format!("エッジが存在しないノードを参照: {edge_label}"),
                )
                .at_edge(&edge_label),
            );
            continue;
        }
        // 領域閉包: from と to は同じ parent（同一領域）に属す。
        if parent_of.get(e.from.as_str()).copied().flatten()
            != parent_of.get(e.to.as_str()).copied().flatten()
        {
            errors.push(
                ValidationError::new(
                    "ir.region_leak",
                    format!("エッジが map 領域境界を跨いでいます: {edge_label}"),
                )
                .at_edge(&edge_label),
            );
        }
        *in_edges.entry(e.to.as_str()).or_insert(0) += 1;
    }

    // 入エッジ制約: join 以外は入エッジ高々 1 本（engine.md・ir.md §5）。
    for node in &ir.nodes {
        let count = in_edges.get(node.id.as_str()).copied().unwrap_or(0);
        let is_join = matches!(
            NodeType::parse(&node.node_type),
            Some(NodeType::ControlJoin)
        );
        if !is_join && count > 1 {
            errors.push(
                ValidationError::new(
                    "ir.multiple_in_edges",
                    format!("join 以外のノードに入エッジが複数あります: {}", node.id),
                )
                .at_node(&node.id),
            );
        }
    }

    // DAG（循環検出）。
    if let Some(cycle_node) = detect_cycle(ir) {
        errors.push(ValidationError::new(
            "ir.graph_cycle",
            format!("グラフに循環があります（例: {cycle_node} を含む）"),
        ));
    }
}

/// DFS で循環を検出し、循環に含まれるノード id を 1 つ返す。
fn detect_cycle(ir: &WorkflowIr) -> Option<String> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &ir.nodes {
        adj.entry(n.id.as_str()).or_default();
    }
    for e in &ir.edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    let mut state: HashMap<&str, u8> = HashMap::new(); // 0=未訪問, 1=訪問中, 2=完了
    for n in &ir.nodes {
        if state.get(n.id.as_str()).copied().unwrap_or(0) == 0 {
            if let Some(c) = dfs_cycle(n.id.as_str(), &adj, &mut state) {
                return Some(c.to_string());
            }
        }
    }
    None
}

fn dfs_cycle<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    state: &mut HashMap<&'a str, u8>,
) -> Option<&'a str> {
    state.insert(node, 1);
    if let Some(next) = adj.get(node) {
        for &m in next {
            match state.get(m).copied().unwrap_or(0) {
                1 => return Some(m), // back-edge = 循環
                0 => {
                    if let Some(c) = dfs_cycle(m, adj, state) {
                        return Some(c);
                    }
                }
                _ => {}
            }
        }
    }
    state.insert(node, 2);
    None
}

/// 到達可能なノード集合（起点=入エッジ 0 のノード）。未使用ノード検出に使える。
#[allow(dead_code)]
fn reachable(ir: &WorkflowIr) -> HashSet<&str> {
    let mut has_in: HashSet<&str> = HashSet::new();
    for e in &ir.edges {
        has_in.insert(e.to.as_str());
    }
    let roots: Vec<&str> = ir
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| !has_in.contains(id))
        .collect();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &ir.edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = roots;
    while let Some(n) = stack.pop() {
        if seen.insert(n) {
            if let Some(next) = adj.get(n) {
                stack.extend(next.iter().copied());
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)]
    fn ir(v: serde_json::Value) -> WorkflowIr {
        WorkflowIr::from_json(&v).unwrap()
    }

    #[test]
    fn detects_duplicate_ids() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "a", "type": "storage.read", "params": {} }
                ],
                "edges": []
            })),
            &mut errs,
        );
        assert!(errs.iter().any(|e| e.code == "ir.duplicate_node_id"));
    }

    #[test]
    fn detects_cycle() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "b", "type": "storage.read", "params": {} }
                ],
                "edges": [
                    { "from": "a", "to": "b" },
                    { "from": "b", "to": "a" }
                ]
            })),
            &mut errs,
        );
        assert!(errs.iter().any(|e| e.code == "ir.graph_cycle"));
    }

    #[test]
    fn detects_bad_edge_ref() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [{ "id": "a", "type": "storage.read", "params": {} }],
                "edges": [{ "from": "a", "to": "ghost" }]
            })),
            &mut errs,
        );
        assert!(errs.iter().any(|e| e.code == "ir.bad_edge_ref"));
    }

    #[test]
    fn non_join_multiple_in_edges_rejected() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "b", "type": "storage.read", "params": {} },
                    { "id": "c", "type": "storage.read", "params": {} }
                ],
                "edges": [
                    { "from": "a", "to": "c" },
                    { "from": "b", "to": "c" }
                ]
            })),
            &mut errs,
        );
        assert!(errs.iter().any(|e| e.code == "ir.multiple_in_edges"));
    }

    #[test]
    fn join_allows_multiple_in_edges() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "b", "type": "storage.read", "params": {} },
                    { "id": "j", "type": "control.join", "params": { "mode": "all" } }
                ],
                "edges": [
                    { "from": "a", "to": "j" },
                    { "from": "b", "to": "j" }
                ]
            })),
            &mut errs,
        );
        assert!(!errs.iter().any(|e| e.code == "ir.multiple_in_edges"));
    }
}
