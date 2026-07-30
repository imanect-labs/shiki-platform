//! 計画カード（調査計画の提示 → 開始 / 修正・#387）。
//!
//! AI が「これから何をするか」の計画を出し、ユーザーが**開始ボタンを押して初めて**実行に入る
//! ためのカード。`plan` メタツール（`AgentEvent::PlanUpdated` → 計画パネル）は**表示専用**で
//! ブロックしないため、承認を取る面がこれまで存在しなかった。
//!
//! 承認ゲート（`Approver`）は**ツール呼び出し単位**の機構で、「計画そのものへの同意」には
//! 粒度が合わない（計画は 1 個のツール呼び出しではない）。そこで genui のカードとして出し、
//! 押下を `chat.submit` の発話へ写す — カードは `generative_ui` として永続するので、
//! ページを離れて戻っても承認導線が消えない。
//!
//! 見た目は計画パネル（`web/src/components/chat/agent-progress.tsx`）と共有する
//! （プラン UI を二重化しない）。信頼境界は質問カードと同じ: 閉じた集合・
//! `deny_unknown_fields`・送信先は `chat.submit` ハンドラのみ（検証層が強制する）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::spec::ActionRef;

/// 実行前の計画とその承認導線。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PlanCardProps {
    /// カード id（文書内で一意・フォーム/質問カードと同一名前空間）。
    pub id: String,
    /// 見出し（省略時はフロントが「調査計画」を表示）。
    #[serde(default)]
    pub title: Option<String>,
    /// 導入文（何を調べるか・任意）。
    #[serde(default)]
    pub intro: Option<String>,
    /// 計画のステップ（順序付き）。
    pub steps: Vec<PlanCardStep>,
    /// 送信先アクション（`chat.submit` ハンドラのみ許可）。
    pub submit: ActionRef,
    /// 開始ボタンのラベル（省略時「この計画で開始」）。
    #[serde(default)]
    pub submit_label: Option<String>,
    /// 「修正する」の自由記述欄を出すか（既定 true）。
    /// false にすると開始のみ（提示した計画を必ず通す用途）。
    #[serde(default = "default_true")]
    pub allow_revise: bool,
}

fn default_true() -> bool {
    true
}

/// 計画の 1 ステップ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PlanCardStep {
    pub title: String,
    /// 補足（何をどこまで調べるか・任意）。
    #[serde(default)]
    pub description: Option<String>,
}

/// 計画カードの上限（防御的リミット）。
pub mod plan_card_limits {
    /// ステップ数の上限（`agent_core::plan::MAX_SUBTASKS` と同じ桁に収める）。
    pub const MAX_STEPS: usize = 24;
    /// title / description の最大文字数。
    pub const MAX_TEXT_CHARS: usize = 400;
}
