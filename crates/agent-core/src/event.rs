//! エージェントループが外へ流すイベント（[`AgentEvent`]）と受け口（[`EventSink`]）。
//!
//! chat ワーカーが [`EventSink`] を実装し、各イベントを `generation_event` へ append
//! （真実のソース）＋ Redis pub/sub 配信する。ループはツール実行/トークンをこのイベントで
//! 逐次外部化し、chat 側で SSE の [`StreamEventKind`](../../chat) へ写す。

use crate::budget::BudgetKind;
use crate::plan::{Plan, SubtaskStatus};
use crate::tool::{ArtifactRef, Citation};

/// 失敗回復（Task 5.5）でループが取った行動。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// エラー観測をモデルへ戻して自己修正を促した（継続）。
    Retry,
    /// 同一失敗のループを検出して安全停止した。
    StopLooping,
}

impl RecoveryAction {
    /// 監査・UI 表示用の安定文字列。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RecoveryAction::Retry => "retry",
            RecoveryAction::StopLooping => "stop_looping",
        }
    }
}

/// エージェントループが外へ流すイベント（プロバイダ非依存・chat 非依存）。
///
/// Phase 3 の 6 種（Text/Thinking/ToolCall/ToolResult/Citation/Artifact）に、Phase 5 の
/// 自律セッション可視化（5.9）・監査（5.10）・承認（5.6）・予算（5.7）用の構造化イベントを足す。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// 本文テキストの差分。
    Text(String),
    /// 思考テキストの差分。
    Thinking(String),
    /// ツール呼び出し（id/name/入力確定）。
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// ツール結果。
    ToolResult {
        tool_call_id: String,
        ok: bool,
        content: String,
    },
    /// 引用（doc_search）。
    Citation(Citation),
    /// ツールが保存した成果物（code_interpreter）。chat 側で FileRef へ写す。
    Artifact {
        tool_call_id: String,
        artifact: ArtifactRef,
    },
    /// 検証済み generative UI スペック（emit_ui・Phase 6 Task 6.4）。
    /// **検証層（gui::SpecValidator）を通過した JSON のみ**がここへ乗る（生スペックは流さない）。
    GenerativeUi { spec: serde_json::Value },
    /// 保存済みワークフローへの参照（emit_workflow・Task 10.13）。
    /// **保存パイプライン（V1〜V7）を通過し artifact 化された参照のみ**がここへ乗る。
    WorkflowRef { workflow: serde_json::Value },
    /// 保存済みノートへの参照（save_note・Task 11P.5）。
    /// **StorageService へ作成済みのノードのみ**がここへ乗る（chat 側で note_ref へ写る）。
    NoteRef { note: serde_json::Value },
    /// 計画が改訂された（全サブタスク列・revision 付き・Task 5.2）。
    PlanUpdated(Plan),
    /// 単一サブタスクの状態遷移（軽量更新・Task 5.2）。
    SubtaskUpdated { id: String, status: SubtaskStatus },
    /// 予算上限への接近警告（種別・現在値・上限・Task 5.7）。
    BudgetWarning {
        kind: BudgetKind,
        used: u64,
        limit: u64,
    },
    /// 破壊系/egress/高コスト操作の承認要求（実行前にブロック・Task 5.6）。
    /// API 結線は W3。ここでは種別を定義し、ループが承認境界で発火できるようにする。
    ApprovalRequested {
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
        reason: String,
    },
    /// 承認/却下の結果（Task 5.6）。
    ApprovalResolved {
        tool_call_id: String,
        approved: bool,
    },
    /// 失敗回復の判断（自己修正リトライ／ループ検出停止・Task 5.5）。
    FailureRecovery {
        detail: String,
        action: RecoveryAction,
    },
}

/// エージェントループのエラー。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// LLM ゲートウェイ側の障害。
    #[error("llm error: {0}")]
    Llm(String),
    /// イベント永続化（sink）側の障害。
    #[error("sink error: {0}")]
    Sink(String),
    /// キャンセル要求で停止した（ユーザー明示停止）。
    #[error("cancelled")]
    Cancelled,
}

/// ループ外へイベントを流す受け口。chat ワーカーが実装する。
///
/// `emit` は append-only 永続化（＋pub/sub）を行うため async。`is_cancelled` はステップ境界と
/// ストリーム読取ループでの協調キャンセル検知に使う（ユーザー明示停止のみ・ページ離脱≠キャンセル）。
#[async_trait::async_trait]
pub trait EventSink: Send {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError>;

    /// キャンセル要求が来ているか（協調キャンセル）。
    fn is_cancelled(&self) -> bool;
}
