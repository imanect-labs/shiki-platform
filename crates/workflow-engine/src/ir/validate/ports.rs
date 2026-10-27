//! ノードの出口（ポート）の導出（ir.md §5.2・単一定義・V2 から分離）。
//!
//! 出口の決まり方はカタログの [`PortRule`]（`vocab::output_port_rule`）が決め、ここが params から実際の
//! 集合を導く。保存時検証 V2（`from_port` の存在）と、検証 API の `node_ports`（エディタのハンドル）が
//! この関数を使う。web に同じ導出を書き写さない（手書きの対応物を作らない・codegen が正）。
//!
//! どの規則でも、`on_error: continue` なら `error`、wait(event) で `on_timeout: continue` なら `timeout`
//! （V1 が wait(event) に限る）が加わる（構造ポート）。実行時に発しないポート（switch の `out`・値で振り分ける
//! ai.decide の `out`）は含めない。

use std::collections::BTreeMap;

use crate::ir::params::AiDecideParams;
use crate::ir::{Node, OnError, WorkflowIr};
use crate::vocab::{approval_ports, output_port_rule, NodeType, PortRule};

fn push(ports: &mut Vec<String>, port: &str) {
    if !ports.iter().any(|p| p == port) {
        ports.push(port.to_string());
    }
}

/// ノードの出口（順序は表示の順・重なりなし）。
///
/// 未知の type は `None`（V3 が拒否する）。ai.decide の params が契約に合わないときも `None`（V1 が報告する。
/// 出口を決められないので V2 は検査しない＝同じ誤りを二重に出さない）。
#[must_use]
pub fn output_ports(node: &Node) -> Option<Vec<String>> {
    let nt = NodeType::parse(&node.node_type)?;
    let (statics, rule) = output_port_rule(nt);
    let mut ports = Vec::new();
    match rule {
        PortRule::Static => {
            for p in statics {
                push(&mut ports, p);
            }
        }
        PortRule::SwitchCases => {
            let cases = node
                .params
                .get("cases")
                .and_then(serde_json::Value::as_array);
            for c in cases.into_iter().flatten() {
                if let Some(p) = c.get("port").and_then(serde_json::Value::as_str) {
                    push(&mut ports, p);
                }
            }
            push(&mut ports, "default");
        }
        // 却下・差し戻し・時間切れは指定したときだけ（GAP-08）。
        PortRule::ApprovalChoices => {
            for p in approval_ports(&node.params) {
                push(&mut ports, p);
            }
        }
        PortRule::DecideRoute => {
            let params: AiDecideParams = serde_json::from_value(node.params.clone()).ok()?;
            params.check_cross_fields().ok()?;
            for p in params.ports()? {
                push(&mut ports, &p);
            }
        }
    }
    if nt == NodeType::ControlWait
        && node
            .params
            .get("on_timeout")
            .and_then(serde_json::Value::as_str)
            == Some("continue")
    {
        push(&mut ports, "timeout");
    }
    if node.on_error == OnError::Continue {
        push(&mut ports, "error");
    }
    Some(ports)
}

/// IR の各ノードの出口（ノード id → 出口・導けないノードは載せない）。検証 API の `node_ports`。
#[must_use]
pub fn node_ports(ir: &WorkflowIr) -> BTreeMap<String, Vec<String>> {
    ir.nodes
        .iter()
        .filter_map(|n| output_ports(n).map(|p| (n.id.clone(), p)))
        .collect()
}

/// 無い出口を指すエッジの文言（使える出口と、設定で出せる出口の手がかりを添える）。
pub(super) fn unknown_port_message(node: &Node, port: &str, ports: &[String]) -> String {
    let list = ports.join(", ");
    let decide = node.node_type == NodeType::AiDecide.as_str();
    let hint = match port {
        "error" => "。error 出口は on_error を continue にしたときだけ使えます",
        "timeout" if node.node_type == NodeType::HumanApproval.as_str() => {
            "。timeout 出口は期限を決めて on_timeout を continue にしたときだけ使えます"
        }
        "timeout" => {
            "。timeout 出口は wait(event) で on_timeout を continue にしたときだけ使えます"
        }
        "rejected" => "。rejected 出口は reject（却下）を指定したときだけ使えます",
        "returned" => "。returned 出口は return（差し戻し）を指定したときだけ使えます",
        "out" if node.node_type == "control.switch" => {
            "。振り分けは cases の port か default から出ます"
        }
        "uncertain" if decide => "。uncertain 出口は route（振り分け）を書いたときだけ使えます",
        _ if decide => {
            "。判定の出口は route で決まります（無し＝out・確からしさで分ける＝out と uncertain・\
             値で分ける＝選択肢のキー（はい/いいえは true と false）と uncertain）"
        }
        _ => "",
    };
    format!(
        "ノード {} に出口 '{port}' はありません（使える出口: {list}）{hint}",
        node.id
    )
}

#[cfg(test)]
mod tests;
