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
//! - 効果の記録（effect_journal）の予約を step の外の Tx で取る種は、打ち切りで予約が結果の無いまま残ることがある。
//!   やり直しは予約を回収できるまで待つ（[`retry_floor_after_timeout_sec`]）。

use std::time::Duration;

use serde_json::Value;

use crate::capability::journal::RESERVATION_RECLAIM_SECS;
use crate::ir::node::Node;
use crate::vocab::{
    step_timeout_limits, NodeType, AGENT_DURATION_RANGE_SEC, DEFAULT_AGENT_DURATION_SEC,
};

/// step を時間の上限で打ち切ったときの code（step の `error.code`・retryable・失敗の分類は時間切れ）。
pub const STEP_TIMEOUT: &str = "step_timeout";

/// AI エージェントの作業時間の上限に足す猶予（秒）。エージェントは作業時間の上限を過ぎてから手順の境界で止まるのを
/// 待ち、止まらなければ打ち切る（api の agent_port は上限の後に最大 60 秒）。そのあとに結果を書く時間を見込む。
pub const AGENT_STOP_MARGIN_SEC: u32 = 120;

/// step の時間の上限（秒・打ち切らない種は `None`）。外部 API 呼び出しの応答の待ちは既定のまま
/// （[`step_timeout_sec_for`]）。
#[must_use]
pub fn step_timeout_sec(node: &Node) -> Option<u32> {
    step_timeout_sec_for(node, None)
}

/// step の時間の上限（秒・打ち切らない種は `None`）。`http_wait_ms` は外部 API 呼び出しが応答を待つ時間（設定
/// `workflow.http_timeout_ms`）で、時間の上限を空欄にした外部 API 呼び出しはこれより先に打ち切らない（管理者が応答の
/// 待ちを延ばしたら、上限の既定もそれに従う。作成者が書いた値はそのまま使う）。
#[must_use]
pub fn step_timeout_sec_for(node: &Node, http_wait_ms: Option<u64>) -> Option<u32> {
    let nt = NodeType::parse(&node.node_type)?;
    let (default, max) = step_timeout_limits(nt);
    let (default, max) = (default?, max?);
    let written = node.timeout_sec.filter(|s| *s > 0);
    let limit = written.unwrap_or(default).min(max);
    if nt == NodeType::HttpRequest && written.is_none() {
        if let Some(ms) = http_wait_ms {
            return Some(limit.max(u32::try_from(ms.div_ceil(1000)).unwrap_or(u32::MAX)));
        }
    }
    if nt == NodeType::AgentInvoke {
        return Some(
            limit.max(agent_duration_sec(&node.params).saturating_add(AGENT_STOP_MARGIN_SEC)),
        );
    }
    Some(limit)
}

/// 予約の回収の猶予に足す余白（秒・時計のずれと、打ち切りから checkpoint までの間を吸収する）。
const RECLAIM_MARGIN_SEC: i64 = 30;

/// 効果の記録の予約を step の外の Tx で取り、副作用の後に結果を書く種か（打ち切りで予約が残りうる）。
///
/// 書き込みと記録を 1 つの Tx にする `storage.write`（打ち切りで両方が巻き戻る）と、予約を取らない種は含まない。
/// 予約を取るのは `nodes/capability.rs`（workflow.start・csv.patch・csv.write）と `nodes/capability_data.rs`
/// （data の書き込み）。script・skill の `Shiki.*` は、打ち切っても script のスレッドが呼び出しを終えるまで走る
/// （ワーカーが待つ・[`runs_on_blocking_thread`]）ので、予約は結果を書いてから外れる。
fn holds_effect_reservation(nt: NodeType) -> bool {
    matches!(
        nt,
        NodeType::DataRecordCreate
            | NodeType::DataRecordUpdate
            | NodeType::DataRecordDelete
            | NodeType::DataTransition
            | NodeType::DataBulkUpsert
            | NodeType::CsvPatch
            | NodeType::CsvWrite
            | NodeType::WorkflowStart
    )
}

/// 打ち切った step のやり直しまでの最短の待ち（秒・待たなくてよいなら `None`）。
///
/// 予約の残りうる種（[`holds_effect_reservation`]）を打ち切ったあと、回収の猶予（[`RESERVATION_RECLAIM_SECS`]）より
/// 前にやり直すと、予約が「別のワーカーが処理中」（`effect_in_progress`・retryable）を返し、試す回数を使い切って
/// 失敗する（効果が済んでいても）。回収できるまで待ってからやり直す。回収した予約はもう一度実行するので、打ち切りの
/// 前に済んでいた効果がもう一度起きることはある（回収窓の at-least-once・engine.md §7.3・§9.4）。
#[must_use]
pub fn retry_floor_after_timeout_sec(code: &str, nt: Option<NodeType>) -> Option<i64> {
    (code == STEP_TIMEOUT && nt.is_some_and(holds_effect_reservation))
        .then(|| i64::from(RESERVATION_RECLAIM_SECS) + RECLAIM_MARGIN_SEC)
}

/// 本体を `spawn_blocking` の中で走らせる種か（フューチャを捨てても止まらない・script と skill）。
///
/// ワーカーは打ち切ったあとも、本体が終わるまで（[`BLOCKING_DRAIN_SEC`] まで）待ってから並行上限の枠を返す。
/// 捨てたまま枠を返すと、動いている script の数を並行上限が縛らなくなる。
#[must_use]
pub fn runs_on_blocking_thread(nt: Option<NodeType>) -> bool {
    matches!(nt, Some(NodeType::ScriptRun | NodeType::SkillInvoke))
}

/// 打ち切った script・skill の本体の終わりを待つ上限（秒）。止める合図の後の `Shiki.*` は断られ、wasm は step の
/// 期限で止まる（epoch）ので、待つのは打ち切りの時点で走っていた `Shiki.*` 呼び出しの残り（外部 API の応答の待ち
/// など）だけ。これを過ぎても終わらなければ待たずに進める（以前と同じ）。
pub const BLOCKING_DRAIN_SEC: u64 = 60;

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

/// 打ち切ったときの step の誤りの文言（実行履歴に出る・ブロック名は実行履歴が出す・期間は分と秒で言う）。
#[must_use]
pub fn timeout_message(limit_sec: u32) -> String {
    let (m, s) = (limit_sec / 60, limit_sec % 60);
    let period = match (m, s) {
        (0, s) => format!("{s} 秒"),
        (m, 0) => format!("{m} 分"),
        (m, s) => format!("{m} 分 {s} 秒"),
    };
    format!("時間の上限（{period}）を過ぎたため、このブロックを止めました。")
}

#[cfg(test)]
#[allow(clippy::needless_pass_by_value)]
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

    /// 予約の残りうる種の打ち切りだけ、やり直しを回収の猶予より後にする（ほかの失敗とほかの種は待たない）。
    #[test]
    fn timeouts_of_reserving_kinds_wait_for_the_reclaim() {
        let floor = i64::from(RESERVATION_RECLAIM_SECS) + RECLAIM_MARGIN_SEC;
        for nt in [
            NodeType::CsvWrite,
            NodeType::CsvPatch,
            NodeType::DataRecordCreate,
            NodeType::DataBulkUpsert,
            NodeType::WorkflowStart,
        ] {
            assert_eq!(
                retry_floor_after_timeout_sec(STEP_TIMEOUT, Some(nt)),
                Some(floor),
                "{nt:?}"
            );
            assert_eq!(retry_floor_after_timeout_sec("unavailable", Some(nt)), None);
        }
        for nt in [
            NodeType::StorageWrite,
            NodeType::StorageRead,
            NodeType::LlmInvoke,
            NodeType::HttpRequest,
            NodeType::ScriptRun,
        ] {
            assert_eq!(
                retry_floor_after_timeout_sec(STEP_TIMEOUT, Some(nt)),
                None,
                "{nt:?}"
            );
        }
        assert_eq!(retry_floor_after_timeout_sec(STEP_TIMEOUT, None), None);
        assert!(floor > i64::from(RESERVATION_RECLAIM_SECS));
    }

    #[test]
    fn messages_say_the_period_in_minutes() {
        assert_eq!(
            timeout_message(30),
            "時間の上限（30 秒）を過ぎたため、このブロックを止めました。"
        );
        assert!(timeout_message(120).contains("（2 分）"));
        assert!(timeout_message(150).contains("（2 分 30 秒）"));
    }

    /// 時間の上限を空欄にした外部 API 呼び出しは、設定の応答の待ちより先に打ち切らない。書いた値と既定の設定はそのまま。
    #[test]
    fn http_defaults_follow_the_configured_response_wait() {
        let http = |t: Option<u32>| node("http.request", t, json!({}));
        assert_eq!(step_timeout_sec_for(&http(None), Some(60_000)), Some(60));
        assert_eq!(step_timeout_sec_for(&http(None), Some(60_500)), Some(61));
        // 既定の設定（30 秒）では既定の上限（30 秒）のまま。短くはしない。
        assert_eq!(step_timeout_sec_for(&http(None), Some(30_000)), Some(30));
        assert_eq!(step_timeout_sec_for(&http(None), Some(5_000)), Some(30));
        assert_eq!(step_timeout_sec_for(&http(None), None), Some(30));
        // 作成者が書いた値はそのまま（設定より短くても）。
        assert_eq!(
            step_timeout_sec_for(&http(Some(10)), Some(60_000)),
            Some(10)
        );
        // ほかの種は設定を見ない。
        assert_eq!(
            step_timeout_sec_for(&node("storage.read", None, json!({})), Some(60_000)),
            Some(30)
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
