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
//! - 繰り返し（`control.map`）の中のブロックは、要素が無ければ動かないので必ず通らない（繰り返しの後ろのブロックは、
//!   繰り返し自身の `out` から決まる）。
//! - 出口を導けないブロック（壊れた params・未知の種類）からは、必ず通るとは言わない（止めすぎない側に倒す）。
//!
//! 毎回動くブロックでも、失敗を「エラー時」の出口で扱うブロック（[`failure_handled`]・`on_error: continue` で `error`
//! の出口から線が出ているもの）は、失敗しても作った人が決めた道で終わる（ir.md §7.10 の業務上の失敗の置き方）。押す前の
//! 権限の確かめは、このブロックが要る権限でも使う人のボタンを止めない（前は押せて、作った人が用意した誤りの返答で
//! 終わっていた）。

use std::collections::{HashMap, HashSet};

use super::validate::output_ports;
use super::{Node, OnError, WorkflowIr};
use crate::vocab::NodeType;

/// 失敗を「エラー時」の出口で扱うブロックの id（`on_error: continue` で、`error` の出口から線が出ているもの・
/// [モジュールの説明](self)）。`error` の出口に線の無い `continue` は、失敗の後に何もしないので入れない。
#[must_use]
pub fn failure_handled(ir: &WorkflowIr) -> HashSet<&str> {
    let handled: HashSet<&str> = ir
        .edges
        .iter()
        .filter(|e| e.from_port == "error")
        .map(|e| e.from.as_str())
        .collect();
    ir.nodes
        .iter()
        .filter(|n| n.on_error == OnError::Continue && handled.contains(n.id.as_str()))
        .map(|n| n.id.as_str())
        .collect()
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
    let body: Vec<&Node> = ir.nodes.iter().filter(|n| n.parent.is_none()).collect();
    let mut always: HashSet<&str> = body
        .iter()
        .filter(|n| incoming.get(n.id.as_str()).is_none_or(Vec::is_empty))
        .map(|n| n.id.as_str())
        .collect();
    // 線をたどって広げる（DAG なのでブロックの数の回数で止まる。壊れた IR の閉路でも、足すものが無くなれば止まる）。
    loop {
        let mut grew = false;
        for n in &body {
            if always.contains(n.id.as_str()) {
                continue;
            }
            let ins = incoming.get(n.id.as_str()).map_or(&[][..], Vec::as_slice);
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

    /// 繰り返しの中は要素が無ければ動かないので道による。繰り返しの後ろは繰り返しの「次へ」から必ず通る。
    /// 振り分け・承認・時間切れのある待ち・振り分けのある AI で判定の出口も道による。
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
}
