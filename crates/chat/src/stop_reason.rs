//! 回答が途中で止まった理由（GAP-106）。
//!
//! agent-core の停止理由（[`agent_core::AgentStop`]）のうち、**自然な終了と利用者の停止以外**を、
//! アシスタントのメッセージ（`message.stop_reason`）と SSE の `done` イベントに残す。画面は回答の下に
//! 短い注記を出す（生成の直後も、読み直したときも）。
//!
//! プロバイダの拒否（`refusal`）は、agent-core が自然な終了（`Completed`）にまとめるので、ここでは
//! 区別できない（llm-gateway は `StopReason::Refusal` を返すが、ループの停止理由に残らない）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use agent_core::{AgentStop, BudgetKind};

/// 回答が途中で止まった理由（自然に終わった回答・利用者が止めた回答には付かない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// 1 回の応答の出力の上限（`max_tokens`）で切れた。
    Truncated,
    /// 手数（ループのステップ数）の上限に達した。agent-core はツールを外した 1 回で、
    /// ここまでの内容をまとめさせてから止める（`land_on_budget`）。
    StepLimit,
    /// 時間の上限に達した（自律で期限を付けたときだけ）。
    TimeLimit,
    /// 1 回の回答で扱うトークン量の上限に達した。
    TokenLimit,
    /// 1 回の回答の費用の上限に達した。
    CostLimit,
    /// 同じツール呼び出しの失敗の繰り返しを検知して止めた（自律のみ）。
    LoopDetected,
}

impl StopReason {
    /// DB（`message.stop_reason`）とワイヤ形式の値。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            StopReason::Truncated => "truncated",
            StopReason::StepLimit => "step_limit",
            StopReason::TimeLimit => "time_limit",
            StopReason::TokenLimit => "token_limit",
            StopReason::CostLimit => "cost_limit",
            StopReason::LoopDetected => "loop_detected",
        }
    }

    /// [`Self::as_str`] の逆（知らない値は `None`）。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "truncated" => StopReason::Truncated,
            "step_limit" => StopReason::StepLimit,
            "time_limit" => StopReason::TimeLimit,
            "token_limit" => StopReason::TokenLimit,
            "cost_limit" => StopReason::CostLimit,
            "loop_detected" => StopReason::LoopDetected,
            _ => return None,
        })
    }

    /// agent-core の停止理由から（自然な終了・利用者の停止は `None`）。
    #[must_use]
    pub fn from_agent(stop: AgentStop) -> Option<Self> {
        match stop {
            AgentStop::Completed | AgentStop::Cancelled => None,
            AgentStop::Truncated => Some(StopReason::Truncated),
            AgentStop::Budget(BudgetKind::Steps) => Some(StopReason::StepLimit),
            AgentStop::Budget(BudgetKind::Time) => Some(StopReason::TimeLimit),
            AgentStop::Budget(BudgetKind::Tokens) => Some(StopReason::TokenLimit),
            AgentStop::Budget(BudgetKind::Cost) => Some(StopReason::CostLimit),
            AgentStop::LoopDetected => Some(StopReason::LoopDetected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [StopReason; 6] = [
        StopReason::Truncated,
        StopReason::StepLimit,
        StopReason::TimeLimit,
        StopReason::TokenLimit,
        StopReason::CostLimit,
        StopReason::LoopDetected,
    ];

    #[test]
    fn wire_value_matches_db_value_and_round_trips() {
        for r in ALL {
            assert_eq!(StopReason::parse(r.as_str()), Some(r));
            assert_eq!(
                serde_json::to_value(r).unwrap(),
                serde_json::json!(r.as_str())
            );
        }
        assert_eq!(StopReason::parse("refusal"), None);
    }

    /// DB の check 制約が全ての値を許す（足した値を migration に書き忘れると確定が落ちる）。
    #[test]
    fn db_check_constraint_lists_every_value() {
        let sql = include_str!("../../../migrations/0156_message_stop_reason.sql");
        for r in ALL {
            assert!(sql.contains(&format!("'{}'", r.as_str())), "{}", r.as_str());
        }
    }

    #[test]
    fn natural_end_and_user_stop_have_no_reason() {
        assert_eq!(StopReason::from_agent(AgentStop::Completed), None);
        assert_eq!(StopReason::from_agent(AgentStop::Cancelled), None);
        assert_eq!(
            StopReason::from_agent(AgentStop::Truncated),
            Some(StopReason::Truncated)
        );
        assert_eq!(
            StopReason::from_agent(AgentStop::Budget(BudgetKind::Steps)),
            Some(StopReason::StepLimit)
        );
        assert_eq!(
            StopReason::from_agent(AgentStop::Budget(BudgetKind::Time)),
            Some(StopReason::TimeLimit)
        );
        assert_eq!(
            StopReason::from_agent(AgentStop::Budget(BudgetKind::Tokens)),
            Some(StopReason::TokenLimit)
        );
        assert_eq!(
            StopReason::from_agent(AgentStop::Budget(BudgetKind::Cost)),
            Some(StopReason::CostLimit)
        );
        assert_eq!(
            StopReason::from_agent(AgentStop::LoopDetected),
            Some(StopReason::LoopDetected)
        );
    }
}
