//! IR から前進に必要な隣接情報を前計算する（run ごと 1 回）。

use std::collections::HashMap;

use crate::ir::WorkflowIr;
use crate::run::readiness::JoinMode;
use crate::vocab::NodeType;

/// 実行時に参照する軽量グラフ（node_id → メタ・エッジ隣接）。
pub struct RunGraph {
    /// node_id → ノード種（parse 済み・不明は None）。
    node_types: HashMap<String, Option<NodeType>>,
    /// node_id → parent（map 領域・領域外は None）。
    parents: HashMap<String, Option<String>>,
    /// node_id → 入エッジ（(from, from_port)）。
    in_edges: HashMap<String, Vec<(String, String)>>,
    /// node_id → 出エッジ（(from_port, to)）。
    out_edges: HashMap<String, Vec<(String, String)>>,
    /// control.join の待ち合わせモード（params.mode・既定 All）。
    join_modes: HashMap<String, JoinMode>,
    /// parent:null（本体）ノードの id 一覧（run 開始時に一括実体化する集合）。
    root_body_nodes: Vec<String>,
}

impl RunGraph {
    /// IR から前計算する。
    pub fn build(ir: &WorkflowIr) -> Self {
        let mut node_types = HashMap::new();
        let mut parents = HashMap::new();
        let mut in_edges: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut out_edges: HashMap<String, Vec<(String, String)>> = HashMap::new();

        let mut join_modes = HashMap::new();
        for n in &ir.nodes {
            node_types.insert(n.id.clone(), NodeType::parse(&n.node_type));
            parents.insert(n.id.clone(), n.parent.clone());
            in_edges.entry(n.id.clone()).or_default();
            out_edges.entry(n.id.clone()).or_default();
            if NodeType::parse(&n.node_type) == Some(NodeType::ControlJoin) {
                let mode = match n.params.get("mode").and_then(|v| v.as_str()) {
                    Some("any") => JoinMode::Any,
                    _ => JoinMode::All,
                };
                join_modes.insert(n.id.clone(), mode);
            }
        }
        for e in &ir.edges {
            in_edges
                .entry(e.to.clone())
                .or_default()
                .push((e.from.clone(), e.from_port.clone()));
            out_edges
                .entry(e.from.clone())
                .or_default()
                .push((e.from_port.clone(), e.to.clone()));
        }
        // 本体ノード（parent:null）を run 開始時に一括実体化する（engine.md §4.5）。
        let root_body_nodes = ir
            .nodes
            .iter()
            .filter(|n| n.parent.is_none())
            .map(|n| n.id.clone())
            .collect();

        RunGraph {
            node_types,
            parents,
            in_edges,
            out_edges,
            join_modes,
            root_body_nodes,
        }
    }

    /// control.join の待ち合わせモード（未登録＝非 join は既定 All）。
    pub fn join_mode(&self, node_id: &str) -> JoinMode {
        self.join_modes
            .get(node_id)
            .copied()
            .unwrap_or(JoinMode::All)
    }

    /// 本体（parent:null）ノード id。
    pub fn root_body_nodes(&self) -> &[String] {
        &self.root_body_nodes
    }

    /// ノードの入エッジ（(from, from_port)）。
    pub fn in_edges(&self, node_id: &str) -> &[(String, String)] {
        self.in_edges.get(node_id).map_or(&[], Vec::as_slice)
    }

    /// ノードの出エッジ（(from_port, to)）。
    pub fn out_edges(&self, node_id: &str) -> &[(String, String)] {
        self.out_edges.get(node_id).map_or(&[], Vec::as_slice)
    }

    /// ノード種（不明は None）。
    pub fn node_type(&self, node_id: &str) -> Option<NodeType> {
        self.node_types.get(node_id).copied().flatten()
    }

    /// 入エッジ 0 本の本体ノード（run 開始時に ready 化する起点）。
    pub fn is_root_source(&self, node_id: &str) -> bool {
        self.in_edges(node_id).is_empty()
    }

    /// map 領域の親（領域外は None）。
    pub fn parent(&self, node_id: &str) -> Option<&str> {
        self.parents.get(node_id).and_then(|p| p.as_deref())
    }

    /// map 領域（parent==map_id）のノード id 一覧。
    pub fn region_nodes(&self, map_id: &str) -> Vec<&str> {
        self.parents
            .iter()
            .filter_map(|(id, p)| (p.as_deref() == Some(map_id)).then_some(id.as_str()))
            .collect()
    }

    /// 領域の入口（領域内 in-edge 0・複数可）。領域閉包（V2）で in-edge は領域内に閉じる。
    pub fn region_entry_nodes(&self, map_id: &str) -> Vec<&str> {
        self.region_nodes(map_id)
            .into_iter()
            .filter(|n| self.in_edges(n).is_empty())
            .collect()
    }

    /// 領域の出口（領域内 out-edge 0・V2 でちょうど 1 つ）。
    pub fn region_exit_node(&self, map_id: &str) -> Option<&str> {
        self.region_nodes(map_id)
            .into_iter()
            .find(|n| self.out_edges(n).is_empty())
    }
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
    fn build_adjacency() {
        let g = RunGraph::build(&ir(json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "a", "type": "storage.read", "params": {} },
                { "id": "b", "type": "storage.write", "params": {} }
            ],
            "edges": [{ "from": "a", "to": "b" }]
        })));
        assert_eq!(g.root_body_nodes().len(), 2);
        assert!(g.is_root_source("a"));
        assert!(!g.is_root_source("b"));
        assert_eq!(g.in_edges("b"), &[("a".to_string(), "out".to_string())]);
        assert_eq!(g.out_edges("a"), &[("out".to_string(), "b".to_string())]);
        assert_eq!(g.node_type("a"), Some(NodeType::StorageRead));
    }
}
