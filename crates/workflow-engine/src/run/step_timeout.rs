//! step の時間の上限（ir.md §4・§7・engine.md §9.4・BUG-96・D-82）。
//!
//! ノードの `timeout_sec`（省略はノード種の既定）を、ノード種の上限で頭打ちにした時間で step を打ち切る。
//! 既定と上限はノードカタログの表（[`step_timeout_limits`]）が単一定義で、カタログの `timeout_default_sec`・
//! `timeout_max_sec`・保存時の照合（V7）・この打ち切りが同じ表を読む。
//!
//! - 打ち切りはやり直せる失敗（[`STEP_TIMEOUT`]・retryable）で、試す回数（`retry`）の規則に従う。失敗の分類は
//!   時間切れ。
//! - 制御のノード（分岐・振り分け・合流・繰り返し・待機）と既定の無い種は打ち切らない（待機と承認を待つ時間は
//!   params の期限と実行の期限が決める）。
//! - 保存済みの定義が上限より大きい値を持っていても、上限で打ち切る（保存時の照合より前に保存されたもの）。
//!   0 は省略と同じに読む。
//! - AI エージェントは自分の作業時間の上限（`max_duration_sec`）で止まる。その前に打ち切らないよう、作業時間の
//!   上限に [`AGENT_STOP_MARGIN_SEC`] を足した時間より短くしない（ノードの上限を超えても）。

use std::time::Duration;

use serde_json::Value;

use crate::ir::node::Node;
use crate::vocab::{
    step_timeout_limits, NodeType, AGENT_DURATION_RANGE_SEC, DEFAULT_AGENT_DURATION_SEC,
};

/// step を時間の上限で打ち切ったときの code（step の `error.code`・retryable・失敗の分類は時間切れ）。
pub const STEP_TIMEOUT: &str = "step_timeout";

/// AI エージェントの作業時間の上限に足す猶予（秒）。エージェントは作業時間の上限を過ぎてから手順の境界で止まるのを
/// 待ち、止まらなければ打ち切る（api の agent_port は上限の後に最大 60 秒）。そのあとに結果を書く時間を見込む。
pub const AGENT_STOP_MARGIN_SEC: u32 = 120;

/// step の時間の上限（秒・打ち切らない種は `None`）。
#[must_use]
pub fn step_timeout_sec(node: &Node) -> Option<u32> {
    let nt = NodeType::parse(&node.node_type)?;
    let (default, max) = step_timeout_limits(nt);
    let (default, max) = (default?, max?);
    let configured = node.timeout_sec.filter(|s| *s > 0).unwrap_or(default);
    let limit = configured.min(max);
    if nt == NodeType::AgentInvoke {
        return Some(
            limit.max(agent_duration_sec(&node.params).saturating_add(AGENT_STOP_MARGIN_SEC)),
        );
    }
    Some(limit)
}

/// [`step_timeout_sec`] の `Duration`。
#[must_use]
pub fn step_timeout(node: &Node) -> Option<Duration> {
    step_timeout_sec(node).map(|s| Duration::from_secs(u64::from(s)))
}

/// AI エージェントの作業時間の上限（秒）。リテラルならその値（範囲に収める）、省略は既定、参照や組み立ては
/// 実行まで分からないので範囲の最大。
#[must_use]
pub fn agent_duration_sec(params: &Value) -> u32 {
    let (min, max) = AGENT_DURATION_RANGE_SEC;
    match params.get("max_duration_sec") {
        None | Some(Value::Null) => DEFAULT_AGENT_DURATION_SEC,
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map_or(max, |n| n.clamp(min, max)),
        Some(_) => max,
    }
}

/// 打ち切ったときの step の誤りの文言（実行履歴に出る・ブロック名は実行履歴が出す）。
#[must_use]
pub fn timeout_message(limit_sec: u32) -> String {
    format!("時間の上限（{limit_sec} 秒）を過ぎたため、このブロックを止めました。")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(ty: &str, timeout_sec: Option<u32>, params: Value) -> Node {
        serde_json::from_value(json!({
            "id": "n", "type": ty, "params": params, "timeout_sec": timeout_sec
        }))
        .unwrap()
    }

    /// 既定と上限は ir.md §7 の表（カタログ）から。書いた値は上限で頭打ち、0 は省略と同じ。
    #[test]
    fn defaults_and_caps_come_from_the_catalog() {
        assert_eq!(
            step_timeout_sec(&node("llm.invoke", None, json!({}))),
            Some(120)
        );
        assert_eq!(
            step_timeout_sec(&node("llm.invoke", Some(300), json!({}))),
            Some(300)
        );
        assert_eq!(
            step_timeout_sec(&node("llm.invoke", Some(9_999), json!({}))),
            Some(600)
        );
        assert_eq!(
            step_timeout_sec(&node("llm.invoke", Some(0), json!({}))),
            Some(120)
        );
        assert_eq!(
            step_timeout_sec(&node("storage.read", None, json!({}))),
            Some(30)
        );
        assert_eq!(
            step_timeout_sec(&node("workflow.reply", Some(60), json!({}))),
            Some(10)
        );
        assert_eq!(
            step_timeout(&node("http.request", None, json!({}))),
            Some(Duration::from_secs(30))
        );
    }

    /// 制御のノードと未知の種は打ち切らない（待機の時間は params が決める）。
    #[test]
    fn control_nodes_are_not_cut() {
        for ty in [
            "control.wait",
            "control.branch",
            "control.switch",
            "control.join",
            "control.map",
            "no.such",
        ] {
            assert_eq!(
                step_timeout_sec(&node(ty, Some(5), json!({}))),
                None,
                "{ty}"
            );
        }
    }

    /// AI エージェントは、自分の作業時間の上限に猶予を足した時間より先に打ち切らない。
    #[test]
    fn agents_are_not_cut_before_their_own_budget() {
        // 省略（作業時間 600 秒）なら 720 秒。ノードの既定（600 秒）より長い。
        assert_eq!(
            step_timeout_sec(&node("agent.invoke", None, json!({}))),
            Some(720)
        );
        assert_eq!(
            step_timeout_sec(&node(
                "agent.invoke",
                Some(60),
                json!({ "max_duration_sec": 30 })
            )),
            Some(150)
        );
        assert_eq!(
            step_timeout_sec(&node(
                "agent.invoke",
                Some(3_000),
                json!({ "max_duration_sec": 30 })
            )),
            Some(3_000)
        );
        // 作業時間を参照で決めるなら最大（3600 秒）を見込む。上限（3600 秒）を超えても打ち切らない。
        assert_eq!(
            step_timeout_sec(&node(
                "agent.invoke",
                None,
                json!({ "max_duration_sec": { "$from": "input", "path": "/sec" } })
            )),
            Some(3_720)
        );
    }
}
