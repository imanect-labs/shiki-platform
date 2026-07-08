//! チェックポイント（Task 5.5）。
//!
//! サブタスク境界で run の進捗（計画・消費・会話履歴・ステップ）を**直列化可能**な状態として
//! 切り出し、中断/再開を可能にする。durable run（`generation_run` の lease/fencing・design §4.4.1）に
//! 載せて永続化するのは chat/api 側（W3/W4）で、agent-core は状態の生成/復元だけを担う。
//!
//! **不変条件**: durability はステップ境界にのみ存在する。生成途中の LLM ストリームは保存しない
//! （復元時は当該ステップを頭から再生成する・design §4.4.1）。

use llm_gateway::Message;
use serde::{Deserialize, Serialize};

use crate::budget::Spent;
use crate::plan::Plan;

/// 再開可能な run 状態のスナップショット（ステップ境界で撮る）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// 現在の計画（サブタスク列）。
    pub plan: Plan,
    /// 累積消費（step/token/cost）。予算ガードは復元後もここから継続する。
    pub spent: Spent,
    /// 会話履歴（剪定後）。次ターンの入力・復元の起点。
    pub messages: Vec<Message>,
    /// 完了済みステップ数（＝次に走るステップ index）。
    pub step: usize,
}

impl Checkpoint {
    /// 空の初期チェックポイント（履歴を起点に新規開始）。
    #[must_use]
    pub fn start(messages: Vec<Message>) -> Self {
        Checkpoint {
            plan: Plan::default(),
            spent: Spent::default(),
            messages,
            step: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_gateway::Role;

    #[test]
    fn roundtrips_through_json() {
        let mut cp = Checkpoint::start(vec![Message::text(Role::User, "goal")]);
        cp.plan.revise(vec![crate::plan::SubtaskInput {
            title: "step 1".into(),
            status: Some("doing".into()),
        }]);
        cp.spent.add_step(120, 300);
        cp.step = 3;

        let json = serde_json::to_string(&cp).expect("serialize");
        let back: Checkpoint = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cp, back);
        assert_eq!(back.plan.subtasks.len(), 1);
        assert_eq!(back.spent.steps, 1);
        assert_eq!(back.step, 3);
    }
}
