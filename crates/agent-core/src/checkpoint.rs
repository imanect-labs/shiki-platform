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
use crate::loop_detect::LoopDetector;
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
    /// 失敗ループ検出器の状態（resume でも失敗履歴を引き継ぐ・旧チェックポイントは既定で空）。
    #[serde(default)]
    pub loop_detector: LoopDetector,
    /// 引用番号の台帳（再開しても `[n]` の通し番号が続く・#508。旧チェックポイントは空から）。
    #[serde(default)]
    pub citations: crate::cite::CitationLedger,
    /// 剪定の見積りに足す、実測 prompt との差（system/ツール定義ぶん・#529）。再開直後の
    /// ステップでも窓判定から落とさないために載せる。`None` は未測定（旧チェックポイント・
    /// 1 ステップも生成していない）で、実測の結果 0 になった `Some(0)` とは区別する。
    #[serde(default)]
    pub context_overhead_tokens: Option<usize>,
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
            loop_detector: LoopDetector::default(),
            citations: crate::cite::CitationLedger::default(),
            context_overhead_tokens: None,
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
        cp.spent.add_step(100, 20, 300);
        cp.step = 3;
        // ループ検出器の失敗履歴も載せる（resume で失われないこと）。
        cp.loop_detector
            .observe("shell", &serde_json::json!({"cmd": "x"}), true);
        cp.loop_detector
            .observe("shell", &serde_json::json!({"cmd": "x"}), true);

        let json = serde_json::to_string(&cp).expect("serialize");
        let back: Checkpoint = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cp, back);
        assert_eq!(back.plan.subtasks.len(), 1);
        assert_eq!(back.spent.steps, 1);
        assert_eq!(back.step, 3);
        // 復元した検出器は失敗履歴を保持: 同じ失敗の 3 回目（閾値 3）でループ判定に達する。
        let mut d = back.loop_detector;
        assert!(
            d.observe("shell", &serde_json::json!({"cmd": "x"}), true),
            "resume 後もループ検出履歴が継続する"
        );
    }

    #[test]
    fn measured_zero_overhead_is_distinct_from_unmeasured() {
        // 実測の結果 0 になった補正は、再開後も「測定済み」として残る（固定分の概算を足し直さない）。
        let mut cp = Checkpoint::start(vec![Message::text(Role::User, "goal")]);
        cp.context_overhead_tokens = Some(0);
        let json = serde_json::to_value(&cp).expect("serialize");
        let back: Checkpoint = serde_json::from_value(json.clone()).expect("deserialize");
        assert_eq!(back.context_overhead_tokens, Some(0));
        // フィールドの無い旧チェックポイントは未測定。
        let mut old = json;
        old.as_object_mut()
            .map(|o| o.remove("context_overhead_tokens"));
        let back: Checkpoint = serde_json::from_value(old).expect("deserialize");
        assert_eq!(back.context_overhead_tokens, None);
    }
}
