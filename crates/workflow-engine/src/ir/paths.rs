//! どのブロックが毎回動くか（IR の線から求める・単一の定義・BUG-216）。
//!
//! 前のブロックがうまくいったとき、入力の値によらず必ず動くブロックを「必ず通る」とし、ほかを「道によって通る」とする。
//! 押す前の権限の確かめ（GAP-37・[`crate::RegistrationService::access_needs`]）は、必ず通るブロックが要る権限だけで
//! 使う人のボタンを止める（分岐の片方や「エラー時」の出口の先だけが使うテーブルのために、通らない入力でも押せなく
//! しない）。
//!
//! 決まり（ir.md §5・engine.md §4 の前進と同じ読み方）:
//!
//! - 本体の入口（入る線が無いブロック）は、実行の始めに必ず動く。
//! - 入る線が 1 本のブロックは、線の元が必ず通り、その線がうまくいったときに必ず取られる出口から出るときだけ必ず通る。
//!   必ず取られる出口は、出口（「エラー時」を除く）が「次へ」（`out`）だけのブロックの `out`。条件で分ける・振り分け・
//!   承認・時間切れのある待ち・振り分けのある AI で判定の出口と、「エラー時」の出口は、値や結果によって取られない。
//! - 待ち合わせ（`control.join`）は、入る線の 1 本でも必ず通るなら必ず通る（all も any も、生きた線が 1 本あれば動く）。
//! - 繰り返し（`control.map`）が必ず通るなら、中の入口（中で入る線が無いブロック）も必ず通り、中は同じ決まりで広げる
//!   （GAP-97）。リストが空の実行では中は動かないが、1 件以上の実行を基準にする（「繰り返しの中でテーブルに足す」
//!   よくある形で、権限の無い人が押した後に失敗で知るより、押す前に止めて理由を言う）。リストがリテラルの空の配列か、
//!   配列にならない値（差し込みの文字列・配列でないリテラル）なら、中は動かないので必ず通るとは言わない。入れ子の
//!   繰り返しも同じ。繰り返しの後ろのブロックは、繰り返し自身の `out` から決まる。
//! - 出口を導けないブロック（壊れた params・未知の種類）からは、必ず通るとは言わない（止めすぎない側に倒す）。
//!
//! 毎回動くブロックでも、失敗を作った人が決めた道で扱うブロック（[`failure_handled`]）は、失敗しても作った人が決めた道
//! で終わる（ir.md §7.10 の業務上の失敗の置き方）。押す前の権限の確かめは、このブロックが要る権限でも使う人のボタンを
//! 止めない（前は押せて、作った人が用意した誤りの返答で終わっていた）。扱うのは次の 2 つ。
//!
//! - `on_error: continue` で `error` の出口から線が出ているブロック（「エラー時」の出口で扱う）。
//! - 失敗を扱う繰り返しの中のブロック: 「失敗を記録して続ける」（`on_item_error: collect`）繰り返しと、それ自身が
//!   「エラー時」の出口で失敗を扱う繰り返しの中（入れ子なら外側のどれかがそうなら）。中の失敗は繰り返しの結果に残るか、
//!   繰り返しの「エラー時」の出口へ進む。

use std::collections::{HashMap, HashSet};

use super::expr::ValueExpr;
use super::params::{parse, MapItemError, MapParams};
use super::validate::output_ports;
use super::{Node, OnError, WorkflowIr};
use crate::vocab::NodeType;

/// 失敗を作った人が決めた道で扱うブロックの id（[モジュールの説明](self)）: `on_error: continue` で `error` の出口から
/// 線が出ているものと、失敗を扱う繰り返しの中のブロック。`error` の出口に線の無い `continue` は、失敗の後に何もしない
/// ので入れない。
#[must_use]
pub fn failure_handled(ir: &WorkflowIr) -> HashSet<&str> {
    let has_error_edge: HashSet<&str> = ir
        .edges
        .iter()
        .filter(|e| e.from_port == "error")
        .map(|e| e.from.as_str())
        .collect();
    let by_error_port: HashSet<&str> = ir
        .nodes
        .iter()
        .filter(|n| n.on_error == OnError::Continue && has_error_edge.contains(n.id.as_str()))
        .map(|n| n.id.as_str())
        .collect();
    let nodes: HashMap<&str, &Node> = ir.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    // 繰り返しが中の失敗を扱うか（「失敗を記録して続ける」か、繰り返し自身が「エラー時」の出口で扱う）。
    let handles_items = |m: &Node| {
        by_error_port.contains(m.id.as_str())
            || map_params(m).is_some_and(|p| p.on_item_error == MapItemError::Collect)
    };
    ir.nodes
        .iter()
        .filter(|n| {
            by_error_port.contains(n.id.as_str()) || enclosing_maps(n, &nodes).any(&handles_items)
        })
        .map(|n| n.id.as_str())
        .collect()
}

/// `node` を囲む繰り返し（内側から外側へ）。壊れた IR の親の輪でも、ブロックの数で止まる。
fn enclosing_maps<'a>(
    node: &'a Node,
    nodes: &'a HashMap<&'a str, &'a Node>,
) -> impl Iterator<Item = &'a Node> + 'a {
    std::iter::successors(
        node.parent.as_deref().and_then(|p| nodes.get(p).copied()),
        |m| m.parent.as_deref().and_then(|p| nodes.get(p).copied()),
    )
    .take(nodes.len())
}

/// 繰り返しの設定（繰り返しでない・読めないなら `None`）。
fn map_params(node: &Node) -> Option<MapParams> {
    if NodeType::parse(&node.node_type) != Some(NodeType::ControlMap) {
        return None;
    }
    parse::<MapParams>(&node.params).ok()
}

/// 繰り返しが動いたとき、中も動くか（リストが参照か、空でない配列のリテラル。[モジュールの説明](self)）。
fn runs_its_region(map: &Node) -> bool {
    map_params(map).is_some_and(|p| match p.items {
        ValueExpr::From(_) => true,
        ValueExpr::Literal(serde_json::Value::Array(items)) => !items.is_empty(),
        ValueExpr::Literal(_) | ValueExpr::Template(_) => false,
    })
}

/// 前のブロックがうまくいったとき、毎回必ず動くブロックの id（[モジュールの説明](self)）。
#[must_use]
pub fn always_run(ir: &WorkflowIr) -> HashSet<&str> {
    let nodes: HashMap<&str, &Node> = ir.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut incoming: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for e in &ir.edges {
        incoming
            .entry(e.to.as_str())
            .or_default()
            .push((e.from.as_str(), e.from_port.as_str()));
    }
    let mut always: HashSet<&str> = HashSet::new();
    // 線をたどって広げる（DAG なのでブロックの数の回数で止まる。壊れた IR の閉路でも、足すものが無くなれば止まる）。
    // 本体の入口は初めから、繰り返しの中の入口は繰り返しが必ず通ると分かった回から、必ず通る（GAP-97）。
    loop {
        let mut grew = false;
        for n in &ir.nodes {
            if always.contains(n.id.as_str()) {
                continue;
            }
            let ins = incoming.get(n.id.as_str()).map_or(&[][..], Vec::as_slice);
            if ins.is_empty() {
                let entered = match n.parent.as_deref() {
                    None => true,
                    Some(p) => {
                        always.contains(p) && nodes.get(p).is_some_and(|m| runs_its_region(m))
                    }
                };
                if entered {
                    always.insert(n.id.as_str());
                    grew = true;
                }
                continue;
            }
            let sure = |(from, port): &(&str, &str)| {
                always.contains(from) && nodes.get(from).is_some_and(|f| sure_port(f, port))
            };
            let join = NodeType::parse(&n.node_type) == Some(NodeType::ControlJoin);
            let reached = if join {
                ins.iter().any(sure)
            } else {
                ins.len() == 1 && sure(&ins[0])
            };
            if reached {
                always.insert(n.id.as_str());
                grew = true;
            }
        }
        if !grew {
            return always;
        }
    }
}

/// `node` がうまくいったとき、必ず `port` から出るか（出口が「次へ」だけで、それが `port`）。
fn sure_port(node: &Node, port: &str) -> bool {
    if port != "out" {
        return false;
    }
    output_ports(node).is_some_and(|ports| {
        let mut normal = ports.iter().filter(|p| p.as_str() != "error");
        normal.next().is_some_and(|p| p == "out") && normal.next().is_none()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ir(nodes: &serde_json::Value, edges: &serde_json::Value) -> WorkflowIr {
        WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "declared_scopes": [],
            "nodes": nodes, "edges": edges
        }))
        .expect("test IR")
    }

    fn sorted(ir: &WorkflowIr) -> Vec<&str> {
        let mut v: Vec<&str> = always_run(ir).into_iter().collect();
        v.sort_unstable();
        v
    }

    /// 入口と、「次へ」だけの出口でつながる先は必ず通る。条件で分けた先・「エラー時」の出口の先・その後ろは道による。
    #[test]
    fn branches_and_error_paths_are_not_always() {
        let ir = ir(
            &json!([
                { "id": "a", "type": "transform.date", "params": {}, "on_error": "continue" },
                { "id": "b", "type": "control.branch", "params": { "condition": { "cmp": { "left": 1, "op": "eq", "right": 1 } } } },
                { "id": "yes", "type": "data.query", "params": {} },
                { "id": "yes2", "type": "data.query", "params": {} },
                { "id": "no", "type": "data.query", "params": {} },
                { "id": "err", "type": "data.query", "params": {} },
                { "id": "solo", "type": "data.query", "params": {} }
            ]),
            &json!([
                { "from": "a", "to": "b" },
                { "from": "a", "from_port": "error", "to": "err" },
                { "from": "b", "from_port": "true", "to": "yes" },
                { "from": "yes", "to": "yes2" },
                { "from": "b", "from_port": "false", "to": "no" }
            ]),
        );
        assert_eq!(sorted(&ir), vec!["a", "b", "solo"]);
    }

    /// 待ち合わせは、入る線の 1 本でも必ず通れば必ず通る。分かれた流れだけが入る待ち合わせは道による。
    #[test]
    fn joins_follow_any_sure_input() {
        let ir = ir(
            &json!([
                { "id": "b", "type": "control.branch", "params": { "condition": { "cmp": { "left": 1, "op": "eq", "right": 1 } } } },
                { "id": "yes", "type": "data.query", "params": {} },
                { "id": "no", "type": "data.query", "params": {} },
                { "id": "j1", "type": "control.join", "params": { "mode": "any" } },
                { "id": "after", "type": "data.query", "params": {} },
                { "id": "s", "type": "data.query", "params": {} },
                { "id": "j2", "type": "control.join", "params": {} },
                { "id": "end", "type": "data.query", "params": {} }
            ]),
            &json!([
                { "from": "b", "from_port": "true", "to": "yes" },
                { "from": "b", "from_port": "false", "to": "no" },
                { "from": "yes", "to": "j1" },
                { "from": "no", "to": "j1" },
                { "from": "j1", "to": "after" },
                { "from": "yes", "to": "j2" },
                { "from": "s", "to": "j2" },
                { "from": "j2", "to": "end" }
            ]),
        );
        assert_eq!(sorted(&ir), vec!["b", "end", "j2", "s"]);
    }

    /// 失敗を「エラー時」の出口で扱うのは、`on_error: continue` で `error` の出口から線が出ているブロックだけ。
    /// `continue` でも線が無いもの・`fail_run` で `error` の線だけがある（保存では止まる）ものは入れない。
    #[test]
    fn failure_handled_needs_continue_and_an_error_edge() {
        let ir = ir(
            &json!([
                { "id": "add", "type": "data.record.create", "params": {}, "on_error": "continue" },
                { "id": "loose", "type": "data.record.create", "params": {}, "on_error": "continue" },
                { "id": "strict", "type": "data.record.create", "params": {} },
                { "id": "ok", "type": "workflow.reply", "params": {} },
                { "id": "ng", "type": "workflow.reply", "params": {} }
            ]),
            &json!([
                { "from": "add", "to": "ok" },
                { "from": "add", "from_port": "error", "to": "ng" },
                { "from": "strict", "from_port": "error", "to": "ng" }
            ]),
        );
        let mut got: Vec<&str> = failure_handled(&ir).into_iter().collect();
        got.sort_unstable();
        assert_eq!(got, vec!["add"]);
        // 毎回動くかの決まりは変えない（入口のブロックは毎回動く）。
        assert!(always_run(&ir).contains("add"));
    }

    /// 中が動かない繰り返し（リストがリテラルの空の配列）の中は道による。繰り返しの後ろは繰り返しの「次へ」から必ず
    /// 通る。振り分け・承認・時間切れのある待ち・振り分けのある AI で判定の出口も道による。
    #[test]
    fn map_regions_and_multi_exit_blocks() {
        let ir = ir(
            &json!([
                { "id": "m", "type": "control.map", "params": { "items": [] } },
                { "id": "inside", "type": "data.query", "parent": "m", "params": {} },
                { "id": "after", "type": "data.query", "params": {} },
                { "id": "sw", "type": "control.switch", "params": { "value": "x", "cases": [{ "value": "x", "port": "x" }] } },
                { "id": "by_case", "type": "data.query", "params": {} },
                { "id": "w", "type": "control.wait", "params": { "kind": "event", "source": "storage.write", "on_timeout": "continue" } },
                { "id": "after_wait", "type": "data.query", "params": {} },
                { "id": "t", "type": "control.wait", "params": { "kind": "duration", "duration_sec": 60 } },
                { "id": "after_timer", "type": "data.query", "params": {} }
            ]),
            &json!([
                { "from": "m", "to": "after" },
                { "from": "sw", "from_port": "x", "to": "by_case" },
                { "from": "w", "to": "after_wait" },
                { "from": "t", "to": "after_timer" }
            ]),
        );
        assert_eq!(
            sorted(&ir),
            vec!["after", "after_timer", "m", "sw", "t", "w"]
        );
    }

    /// GAP-97: 必ず通る繰り返しの中の入口と、そこから「次へ」だけでつながるブロックは必ず通る（リストが空でない実行を
    /// 基準にする）。中の分かれた流れの先と、中が動かない繰り返し（空の配列・差し込みの文字列）の中、道によって通る
    /// 繰り返しの中は道による。入れ子の繰り返しも同じ決まりで広げる。
    #[test]
    fn regions_of_always_running_maps_are_always() {
        let from_input = json!({ "$from": "input", "path": "/rows" });
        let each = json!({ "$from": "each", "path": "/item/lines" });
        let ir = ir(
            &json!([
                { "id": "m", "type": "control.map", "params": { "items": from_input } },
                { "id": "add", "type": "data.record.create", "parent": "m", "params": {} },
                { "id": "then", "type": "data.query", "parent": "m", "params": {} },
                { "id": "check", "type": "control.branch", "parent": "m", "params": { "condition": { "cmp": { "left": 1, "op": "eq", "right": 1 } } } },
                { "id": "only_yes", "type": "data.query", "parent": "m", "params": {} },
                { "id": "inner", "type": "control.map", "parent": "m", "params": { "items": each } },
                { "id": "inner_add", "type": "data.record.create", "parent": "inner", "params": {} },
                { "id": "literal", "type": "control.map", "params": { "items": [1, 2] } },
                { "id": "in_literal", "type": "data.query", "parent": "literal", "params": {} },
                { "id": "templated", "type": "control.map", "params": { "items": { "$template": "{{a}}", "vars": { "a": "x" } } } },
                { "id": "in_template", "type": "data.query", "parent": "templated", "params": {} },
                { "id": "b", "type": "control.branch", "params": { "condition": { "cmp": { "left": 1, "op": "eq", "right": 1 } } } },
                { "id": "m_on_branch", "type": "control.map", "params": { "items": from_input } },
                { "id": "in_branch_map", "type": "data.record.create", "parent": "m_on_branch", "params": {} }
            ]),
            &json!([
                { "from": "add", "to": "then" },
                { "from": "then", "to": "check" },
                { "from": "check", "from_port": "true", "to": "only_yes" },
                { "from": "b", "from_port": "true", "to": "m_on_branch" }
            ]),
        );
        assert_eq!(
            sorted(&ir),
            vec![
                "add",
                "b",
                "check",
                "in_literal",
                "inner",
                "inner_add",
                "literal",
                "m",
                "templated",
                "then",
            ]
        );
    }

    /// GAP-97: 失敗を扱う繰り返し（「失敗を記録して続ける」と、繰り返し自身が「エラー時」の出口で扱うもの）の中の
    /// ブロックは、失敗を作った人が決めた道で扱う（入れ子なら外側のどれかが扱えば中も）。ふつうの繰り返しの中は扱わない。
    #[test]
    fn regions_of_maps_that_handle_failures_are_failure_handled() {
        let rows = json!({ "$from": "input", "path": "/rows" });
        let ir = ir(
            &json!([
                { "id": "collect", "type": "control.map", "params": { "items": rows, "on_item_error": "collect" } },
                { "id": "in_collect", "type": "data.record.create", "parent": "collect", "params": {} },
                { "id": "nested", "type": "control.map", "parent": "collect", "params": { "items": rows } },
                { "id": "in_nested", "type": "data.record.create", "parent": "nested", "params": {} },
                { "id": "routed", "type": "control.map", "params": { "items": rows }, "on_error": "continue" },
                { "id": "in_routed", "type": "data.record.create", "parent": "routed", "params": {} },
                { "id": "ng", "type": "workflow.reply", "params": {} },
                { "id": "plain", "type": "control.map", "params": { "items": rows } },
                { "id": "in_plain", "type": "data.record.create", "parent": "plain", "params": {} }
            ]),
            &json!([
                { "from": "in_collect", "to": "nested" },
                { "from": "routed", "from_port": "error", "to": "ng" }
            ]),
        );
        let mut got: Vec<&str> = failure_handled(&ir).into_iter().collect();
        got.sort_unstable();
        assert_eq!(
            got,
            vec!["in_collect", "in_nested", "in_routed", "nested", "routed"]
        );
        // 毎回動くかの決まりは変えない。
        assert!(always_run(&ir).contains("in_plain"));
    }
}
