//! V7: ノードの時間の上限（`timeout_sec`・ir.md §4「上限超過値は保存拒否」・BUG-96・D-82）。
//!
//! 範囲はノード種の上限（[`step_timeout_limits`]・実行の打ち切りと同じ表）。AI エージェントは自分の作業時間の上限
//! （`max_duration_sec`）より短くできない（その前に打ち切らない・付録 D の N2.6）。制御のノードなど打ち切らない種に
//! 書いた値は読まないだけで、保存は止めない（エディタが共通の欄として書いた保存済みの定義を壊さない）。

use super::ValidationError;
use crate::ir::WorkflowIr;
use crate::run::step_timeout::agent_duration_sec;
use crate::vocab::{step_timeout_limits, NodeType};

/// 欄の見出し（付録 D の N2.6）。
const LABEL: &str = "時間の上限";

/// 4 桁以上を 3 桁ごとにカンマで区切る（付録 C の P11・3,600）。
fn grouped(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(super) fn v7(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    for node in &ir.nodes {
        let (Some(t), Some(nt)) = (node.timeout_sec, NodeType::parse(&node.node_type)) else {
            continue;
        };
        let Some(max) = step_timeout_limits(nt).1 else {
            continue;
        };
        let mut push = |message: String| {
            errors.push(
                ValidationError::new("ir.limit_exceeded", message)
                    .at_node(&node.id)
                    .at_path("/timeout_sec"),
            );
        };
        if !(1..=max).contains(&t) {
            push(format!(
                "「{LABEL}」は 1〜{} 秒にしてください。",
                grouped(max)
            ));
            continue;
        }
        if nt == NodeType::AgentInvoke {
            let duration = agent_duration_sec(&node.params);
            if t < duration {
                let d = grouped(duration);
                push(format!(
                    "「{LABEL}」は「AI の作業時間の上限」（{d} 秒）より短くできません。{d} 秒以上にするか、空欄にしてください。"
                ));
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::needless_pass_by_value)]
mod tests {
    use serde_json::json;

    use super::super::{validate, Catalog};

    fn ir(node: serde_json::Value) -> serde_json::Value {
        json!({ "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
                "nodes": [node], "edges": [] })
    }

    fn timeout_errors(node: serde_json::Value) -> Vec<String> {
        match validate(&ir(node), &Catalog::default()) {
            Ok(_) => Vec::new(),
            Err(errs) => errs
                .into_iter()
                .filter(|e| e.path.as_deref() == Some("/timeout_sec"))
                .map(|e| {
                    assert_eq!(e.code, "ir.limit_exceeded");
                    e.message
                })
                .collect(),
        }
    }

    /// 上限を超える値と 0 は保存で止める（ノード種の上限・ir.md §7 の表）。範囲の中と省略は通る。
    #[test]
    fn node_time_limits_are_checked_against_the_catalog() {
        let llm = |t: serde_json::Value| json!({ "id": "ask", "type": "llm.invoke", "timeout_sec": t, "params": { "prompt": "x" } });
        assert!(timeout_errors(llm(json!(600))).is_empty());
        assert!(timeout_errors(llm(json!(1))).is_empty());
        assert!(timeout_errors(llm(json!(null))).is_empty());
        assert_eq!(
            timeout_errors(llm(json!(601))),
            ["「時間の上限」は 1〜600 秒にしてください。"]
        );
        assert_eq!(timeout_errors(llm(json!(0))).len(), 1);
        // 打ち切らない種（待機）に書いた値は読まないだけで、保存は止めない。
        assert!(timeout_errors(
            json!({ "id": "w", "type": "control.wait", "timeout_sec": 99_999,
            "params": { "kind": "duration", "duration_sec": 60 } })
        )
        .is_empty());
    }

    /// AI エージェントは作業時間の上限より短くできない（省略は 600 秒）。
    #[test]
    fn agents_cannot_be_cut_before_their_own_budget() {
        let agent = |t: u32, params: serde_json::Value| {
            let mut p = json!({ "instruction": "要点をまとめて" });
            p.as_object_mut()
                .unwrap()
                .extend(params.as_object().unwrap().clone());
            json!({ "id": "a", "type": "agent.invoke", "timeout_sec": t, "params": p })
        };
        assert!(timeout_errors(agent(900, json!({}))).is_empty());
        let errs = timeout_errors(agent(300, json!({})));
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("（600 秒）より短くできません"), "{errs:?}");
        assert!(timeout_errors(agent(300, json!({ "max_duration_sec": 300 }))).is_empty());
        let long = timeout_errors(agent(3_000, json!({ "max_duration_sec": 3_600 })));
        assert!(long[0].contains("（3,600 秒）"), "{long:?}");
        let over = timeout_errors(
            json!({ "id": "a", "type": "agent.invoke", "timeout_sec": 4_000,
            "params": { "instruction": "x" } }),
        );
        assert_eq!(over, ["「時間の上限」は 1〜3,600 秒にしてください。"]);
    }
}
