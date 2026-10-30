//! V2: グラフ制約（id 一意・エッジ参照・ポート存在・DAG・入エッジ制約・領域閉包・到達性・ir.md §8）。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::ports::{output_ports, unknown_port_message};
use super::{block_name, ValidationError};
use crate::ir::{Node, WorkflowIr};
use crate::vocab::NodeType;

/// 警告のコード: 線が 1 本も無いブロック（ほかにもブロックがあるとき・BUG-106）。
///
/// 前から線が来ないブロックは実行の始めにほかの入口と同時に動く（engine.md §4.5 の一括実体化）。ドラッグで置いて
/// 線をつなぎ忘れたブロックが勝手に動くので、保存は止めずに注意する（意図して並べた入口もあるので誤りにしない）。
pub const UNCONNECTED_NODE: &str = "ir.unconnected_node";

/// [`UNCONNECTED_NODE`] の文言（ブロックに付けて出すので、ブロック名は前に置かない）。
const UNCONNECTED_NODE_MESSAGE: &str =
    "線でつながっていないので、実行の始めにほかのブロックと同時に動きます。\
                                        線でつなぐか、要らなければ削除してください。";

/// 保存を止めない警告: 本体（map の領域の外）のブロックで、入る線も出る線も無いもの（本体に 2 つ以上ブロックが
/// あるときだけ。1 つだけなら線が無いのが当たり前）。map の領域の中は、領域の出口がちょうど 1 つの決まり（V2）が
/// 線の無いブロックを誤りにする。
pub(super) fn warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let body: Vec<&Node> = ir.nodes.iter().filter(|n| n.parent.is_none()).collect();
    if body.len() < 2 {
        return Vec::new();
    }
    let linked: HashSet<&str> = ir
        .edges
        .iter()
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();
    body.into_iter()
        .filter(|n| !linked.contains(n.id.as_str()))
        .map(|n| ValidationError::new(UNCONNECTED_NODE, UNCONNECTED_NODE_MESSAGE).at_node(&n.id))
        .collect()
}

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
    let nodes_by_id: HashMap<&str, &Node> = ir.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut in_edges: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, e) in ir.edges.iter().enumerate() {
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
        // ポート存在（ir.md §5.2）: 無い出口のエッジは実行時に dead になり、下流が黙って skip される。
        if let Some(from) = nodes_by_id.get(e.from.as_str()) {
            if let Some(ports) = output_ports(from) {
                if !ports.contains(&e.from_port) {
                    errors.push(
                        ValidationError::new(
                            "ir.unknown_port",
                            unknown_port_message(from, &e.from_port, &ports),
                        )
                        .at_node(&e.from)
                        .at_edge(&edge_label)
                        .at_path(format!("/edges/{i}/from_port")),
                    );
                }
            }
        }
        // 領域閉包: from と to は同じ parent（同一領域）に属す。
        let from_region = parent_of.get(e.from.as_str()).copied().flatten();
        let to_region = parent_of.get(e.to.as_str()).copied().flatten();
        if from_region != to_region {
            let name = |id: &str| {
                nodes_by_id
                    .get(id)
                    .map_or_else(|| id.to_string(), |n| block_name(n))
            };
            // 中と外をまたぐ繰り返し（線の元が中にあればその繰り返し・無ければ線の先の繰り返し）。
            let region = from_region.or(to_region).unwrap_or_default();
            errors.push(
                ValidationError::new(
                    "ir.region_leak",
                    format!(
                        "「{}」から「{}」への線が、「{}」の中と外をまたいでいます。中のブロックは、中のブロックとだけつなげます。",
                        name(&e.from),
                        name(&e.to),
                        name(region)
                    ),
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

    // map 領域の構造（入口・出口・ネスト深さ・ir.md §5.3）。
    check_regions(ir, &node_types, &parent_of, &in_edges, errors);

    // DAG（循環検出）。
    if let Some(cycle_node) = detect_cycle(ir) {
        errors.push(ValidationError::new(
            "ir.graph_cycle",
            format!("グラフに循環があります（例: {cycle_node} を含む）"),
        ));
    }
}

/// map 領域の構造制約（ir.md §5.3）: 各領域は入口 ≥1・**出口ちょうど 1**・**ネスト最大深さ 2**。
///
/// 中にブロックの無い領域は誤りにしない（エディタで置いた直後の繰り返しを保存できるように・D-85）。保存時の
/// 警告と、実行・自動実行のオンの前に止める理由は readiness（`ir.empty_region`）が出す。
fn check_regions(
    ir: &WorkflowIr,
    node_types: &HashMap<&str, Option<NodeType>>,
    parent_of: &HashMap<&str, Option<&str>>,
    in_edges: &BTreeMap<&str, usize>,
    errors: &mut Vec<ValidationError>,
) {
    // 領域ノードの出次数（領域閉包で出エッジは領域内に閉じるため全出エッジが領域内）。
    let mut out_deg: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &ir.edges {
        *out_deg.entry(e.from.as_str()).or_insert(0) += 1;
    }
    for node in &ir.nodes {
        if node_types.get(node.id.as_str()).copied().flatten() != Some(NodeType::ControlMap) {
            continue;
        }
        let map_id = node.id.as_str();
        let map_name = block_name(node);
        // ネスト深さ: map の親 map 連鎖が 2 以上（＝深さ 3 以上）は不可。中が空でも決める（後から中に足した
        // 時点で突然誤りにしない）。
        let mut ancestors = 0;
        let mut cur = parent_of.get(map_id).copied().flatten();
        while let Some(p) = cur {
            ancestors += 1;
            cur = parent_of.get(p).copied().flatten();
        }
        if ancestors >= 2 {
            errors.push(
                ValidationError::new(
                    "ir.region_too_deep",
                    format!(
                        "「{}」の中に置けるのは 2 段までです。",
                        crate::vocab::label_ja(NodeType::ControlMap)
                    ),
                )
                .at_node(map_id),
            );
        }
        // 領域ノード（parent==map_id）。空の領域は readiness が止める（入口・出口の決まりは中がある領域だけ）。
        let region: Vec<&str> = ir
            .nodes
            .iter()
            .filter(|n| n.parent.as_deref() == Some(map_id))
            .map(|n| n.id.as_str())
            .collect();
        if region.is_empty() {
            continue;
        }
        let entries = region
            .iter()
            .filter(|n| in_edges.get(*n).copied().unwrap_or(0) == 0)
            .count();
        let exits = region
            .iter()
            .filter(|n| out_deg.get(*n).copied().unwrap_or(0) == 0)
            .count();
        if entries == 0 {
            // DAG なら通常到達不能だが防御的に明示（全ノードが領域内サイクル等）。
            errors.push(
                ValidationError::new(
                    "ir.region_no_entry",
                    format!(
                        "「{map_name}」の中に、最初に動くブロック（前から線が来ないブロック）がありません。"
                    ),
                )
                .at_node(map_id),
            );
        }
        if exits != 1 {
            errors.push(
                ValidationError::new(
                    "ir.region_exit",
                    format!(
                        "「{map_name}」の中の流れは、最後のブロックを 1 つにしてください（いま {exits} 個）。分かれた流れは「{}」でまとめます。",
                        crate::vocab::label_ja(NodeType::ControlJoin)
                    ),
                )
                .at_node(map_id),
            );
        }
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
    fn region_requires_exactly_one_exit() {
        // 領域に出口（out-edge 0）が 2 つ → ir.region_exit。
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "m", "type": "control.map", "params": { "items": [] } },
                    { "id": "a", "type": "storage.read", "parent": "m", "params": {} },
                    { "id": "b", "type": "storage.read", "parent": "m", "params": {} }
                ],
                "edges": []
            })),
            &mut errs,
        );
        assert!(errs.iter().any(|e| e.code == "ir.region_exit"));
    }

    /// D-85: 中にブロックの無い繰り返しは保存を止めない（置いた直後に保存できる。実行と自動実行のオンの前に
    /// readiness が止める）。入口・出口の決まりも、中があるときだけ見る。
    #[test]
    fn empty_region_is_not_a_save_error() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "m", "type": "control.map", "params": { "items": "" } },
                    { "id": "after", "type": "storage.read", "params": {} }
                ],
                "edges": [{ "from": "m", "to": "after" }]
            })),
            &mut errs,
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    /// 中の流れの誤りは、繰り返しの名前（付けた名前・無ければ種類の名前）と、つないだブロックの名前で言う
    /// （内部の id や「領域」を出さない・付録 D の N3.1.4）。
    #[test]
    fn region_errors_name_the_blocks() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "m", "type": "control.map", "label": "申請ごとに", "params": { "items": [] } },
                    { "id": "a", "type": "storage.read", "label": "読む", "parent": "m", "params": {} },
                    { "id": "b", "type": "storage.read", "parent": "m", "params": {} },
                    { "id": "out", "type": "storage.read", "label": "外", "params": {} }
                ],
                "edges": [{ "from": "out", "to": "a" }]
            })),
            &mut errs,
        );
        let msg = |code: &str| {
            errs.iter()
                .find(|e| e.code == code)
                .map_or_else(String::new, |e| e.message.clone())
        };
        assert_eq!(
            msg("ir.region_leak"),
            "「外」から「読む」への線が、「申請ごとに」の中と外をまたいでいます。中のブロックは、中のブロックとだけつなげます。"
        );
        assert_eq!(
            msg("ir.region_exit"),
            "「申請ごとに」の中の流れは、最後のブロックを 1 つにしてください（いま 2 個）。分かれた流れは「合流」でまとめます。"
        );
        // 線の元が中のとき（中から外へ）も、またいだ繰り返しの名前で言う。名前の無いブロックは種類の名前。
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "m", "type": "control.map", "params": { "items": [] } },
                    { "id": "a", "type": "storage.read", "parent": "m", "params": {} },
                    { "id": "dst", "type": "storage.read", "params": {} }
                ],
                "edges": [{ "from": "a", "to": "dst" }]
            })),
            &mut errs,
        );
        let leak = errs.iter().find(|e| e.code == "ir.region_leak").unwrap();
        assert_eq!(
            leak.message,
            "「ファイルを読む」から「ファイルを読む」への線が、「繰り返し」の中と外をまたいでいます。中のブロックは、中のブロックとだけつなげます。"
        );
        assert_eq!(leak.edge.as_deref(), Some("a -> dst"));
    }

    #[test]
    fn region_valid_single_node() {
        // 単一ノード領域（入口=出口）は妥当。
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "m", "type": "control.map", "params": { "items": [] } },
                    { "id": "w", "type": "storage.read", "parent": "m", "params": {} },
                    { "id": "after", "type": "storage.read", "params": {} }
                ],
                "edges": [{ "from": "m", "to": "after" }]
            })),
            &mut errs,
        );
        assert!(
            !errs
                .iter()
                .any(|e| e.code.starts_with("ir.region") || e.code == "ir.empty_region"),
            "単一ノード領域は妥当: {errs:?}"
        );
    }

    #[test]
    fn map_nesting_depth_capped_at_two() {
        // m1 > m2 > m3（深さ 3）は ir.region_too_deep。
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "m1", "type": "control.map", "params": { "items": [] } },
                    { "id": "m2", "type": "control.map", "parent": "m1", "params": { "items": [] } },
                    { "id": "m3", "type": "control.map", "parent": "m2", "params": { "items": [] } },
                    { "id": "leaf", "type": "storage.read", "parent": "m3", "params": {} }
                ],
                "edges": []
            })),
            &mut errs,
        );
        let deep = errs
            .iter()
            .find(|e| e.code == "ir.region_too_deep")
            .unwrap();
        assert_eq!(deep.message, "「繰り返し」の中に置けるのは 2 段までです。");
        assert_eq!(deep.node_id.as_deref(), Some("m3"));

        // 3 段目の繰り返しの中が空でも、深さの誤りにする（中に足した時点で突然誤りにしない）。
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "m1", "type": "control.map", "params": { "items": [] } },
                    { "id": "m2", "type": "control.map", "parent": "m1", "params": { "items": [] } },
                    { "id": "m3", "type": "control.map", "parent": "m2", "params": { "items": [] } }
                ],
                "edges": []
            })),
            &mut errs,
        );
        assert!(
            errs.iter()
                .any(|e| e.code == "ir.region_too_deep" && e.node_id.as_deref() == Some("m3")),
            "{errs:?}"
        );
    }

    /// 2 ノード（`a` の出口 `port` → `b`）の IR を V2 に通し、ir.unknown_port のエラーを返す。
    #[allow(clippy::needless_pass_by_value)] // json! をそのまま渡す。
    fn unknown_ports(a: serde_json::Value, port: &str) -> Vec<ValidationError> {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [ a, { "id": "b", "type": "storage.read", "params": {} } ],
                "edges": [ { "from": "a", "from_port": port, "to": "b" } ]
            })),
            &mut errs,
        );
        errs.retain(|e| e.code == "ir.unknown_port");
        errs
    }

    /// ir.md §5.2・§8 V2: 無い出口のエッジ（AI が書きがちな next / yes）は保存時に拒否する。
    #[test]
    fn edge_from_unknown_port_is_rejected() {
        let read = json!({ "id": "a", "type": "storage.read", "params": {} });
        assert!(unknown_ports(read.clone(), "out").is_empty());
        let errs = unknown_ports(read, "next");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].node_id.as_deref(), Some("a"));
        assert_eq!(errs[0].edge.as_deref(), Some("a -> b"));
        assert_eq!(errs[0].path.as_deref(), Some("/edges/0/from_port"));
        assert!(
            errs[0].message.contains("使える出口: out"),
            "{}",
            errs[0].message
        );

        let branch = json!({ "id": "a", "type": "control.branch", "params": { "condition":
            { "cmp": { "left": 1, "op": "eq", "right": 1 } } } });
        for ok in ["true", "false"] {
            assert!(unknown_ports(branch.clone(), ok).is_empty(), "{ok}");
        }
        for bad in ["yes", "out"] {
            assert_eq!(unknown_ports(branch.clone(), bad).len(), 1, "{bad}");
        }
    }

    #[test]
    fn switch_ports_are_its_cases_and_default() {
        let sw = json!({ "id": "a", "type": "control.switch", "params": {
            "value": { "$from": "input", "path": "/kind" },
            "cases": [ { "port": "pdf", "equals": "pdf" }, { "port": "img", "equals": "png" } ] } });
        for ok in ["pdf", "img", "default"] {
            assert!(unknown_ports(sw.clone(), ok).is_empty(), "{ok}");
        }
        // switch は out を発しない（エディタの古い版が作った out のエッジは dead だった）。
        let errs = unknown_ports(sw.clone(), "out");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("default"), "{}", errs[0].message);
        assert_eq!(unknown_ports(sw, "doc").len(), 1);
    }

    #[test]
    fn error_and_timeout_ports_follow_the_settings() {
        let cont =
            json!({ "id": "a", "type": "storage.read", "on_error": "continue", "params": {} });
        assert!(unknown_ports(cont.clone(), "error").is_empty());
        assert!(unknown_ports(cont, "out").is_empty());
        let fail = json!({ "id": "a", "type": "storage.read", "params": {} });
        let errs = unknown_ports(fail, "error");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("on_error"), "{}", errs[0].message);

        let wait_cont = json!({ "id": "a", "type": "control.wait", "params": {
            "kind": "event", "source": "storage.write", "timeout_sec": 60, "on_timeout": "continue" } });
        assert!(unknown_ports(wait_cont.clone(), "timeout").is_empty());
        assert!(unknown_ports(wait_cont, "out").is_empty());
        let wait_fail = json!({ "id": "a", "type": "control.wait", "params": {
            "kind": "event", "source": "storage.write", "timeout_sec": 60 } });
        assert_eq!(unknown_ports(wait_fail, "timeout").len(), 1);
        // map も on_error: continue なら error 出口を持つ（領域の失敗を流す）。
        let map = json!({ "id": "a", "type": "control.map", "on_error": "continue",
            "params": { "items": [] } });
        assert!(unknown_ports(map, "error").is_empty());
    }

    /// 線が 1 本も無い本体のブロックは注意する（ドラッグで置いてつなぎ忘れたもの・BUG-106）。線のある入口・
    /// map の領域の中・ブロックが 1 つだけの定義は注意しない。
    #[test]
    fn unconnected_blocks_are_warned() {
        let g = ir(json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "a", "type": "storage.read", "params": {} },
                { "id": "b", "type": "storage.read", "params": {} },
                { "id": "dropped", "type": "storage.read", "params": {} },
                { "id": "entry2", "type": "storage.read", "params": {} },
                { "id": "c", "type": "storage.read", "params": {} },
                { "id": "m", "type": "control.map", "params": {} },
                { "id": "inner", "type": "storage.read", "params": {}, "parent": "m" }
            ],
            "edges": [
                { "from": "a", "to": "b" },
                { "from": "entry2", "to": "c" },
                { "from": "b", "to": "m" }
            ]
        }));
        let w = warnings(&g);
        let at: Vec<(&str, Option<&str>)> = w
            .iter()
            .map(|e| (e.code.as_str(), e.node_id.as_deref()))
            .collect();
        assert_eq!(at, [(UNCONNECTED_NODE, Some("dropped"))]);
        assert!(w[0]
            .message
            .starts_with("線でつながっていないので、実行の始めに"));
        let single = ir(json!({
            "ir_version": 1, "name": "wf",
            "nodes": [{ "id": "only", "type": "storage.read", "params": {} }],
            "edges": []
        }));
        assert!(warnings(&single).is_empty());
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
