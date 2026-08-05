//! SSE で配信する生成イベント（`generation_event.payload` と同型）。
//!
//! [`crate::model`] から切り出した（1 ファイル 500 行のゲート）。ドメイン型（`ContentBlock` 等）と
//! 違い、こちらは**ワイヤ形式**であり、`generation_event` に append されて replay される。
//! そのため**フィールド追加は必ず後方互換**（`#[serde(default)]`）にすること — 過去 run の
//! payload をデシリアライズできなくなると、再訪時に会話が壊れる。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::model::{Citation, RunStatus};

/// SSE で配信する構造化イベント（`generation_event.payload` と一致）。
///
/// フロント `StreamHandlers`（onToken/onThinking/onToolCall/onToolResult/onCitation/onError）
/// と対応する。各イベントは `generation_event(run_id, seq)` に append され、SSE では
/// `id: <seq>` を付けて配信する（Last-Event-ID で replay-then-subscribe）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEventKind {
    /// 本文トークン（差分）。
    Token { text: String },
    /// 思考トークン（差分）。
    Thinking { text: String },
    /// ツール呼び出し開始（エージェントモード可視化）。
    ///
    /// `step` は同一ループステップで出た呼び出しに共通の通し番号（0 始まり）。
    /// UI は これで「並行して N 件」を判定する（到達順から推測しない）。
    ///
    /// **`None` は「不明」**（フィールド追加前の run を replay した場合）。0 で埋めると
    /// 逐次実行だった過去のツール群が並行実行に見えてしまう。
    ///
    /// `via_subagent` はサブエージェントが親の UI へ中継した呼び出し（#391）。**子は自分の
    /// ループ番号で `step` を数える**ため、そのまま流すと親の step と衝突し「並行して N 件」が
    /// 実態とずれる（実測: 子の `web_fetch` が step 5 として親の step 5 に混ざった）。中継側で
    /// `step` を落とし、この印だけを立てる。
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u32>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        via_subagent: bool,
    },
    /// ツール結果。
    ToolResult {
        tool_call_id: String,
        ok: bool,
        content: String,
    },
    /// 引用。
    Citation(Citation),
    /// ツール成果物のファイル参照（code_interpreter の保存済み成果物・Task 4.11）。
    FileRef { node_id: String, name: String },
    /// 宣言的 UI（Phase 6）。
    GenerativeUi { spec: serde_json::Value },
    /// 保存済みワークフローへの参照カード（emit_workflow・Task 10.13）。
    WorkflowRef { workflow: serde_json::Value },
    /// 保存済みノートへの参照カード（save_note・Task 11P.5）。
    NoteRef { note: serde_json::Value },
    /// 未保存の下書きノートカード（save_note の下書き確定型・issue #282）。
    NoteDraft { draft: serde_json::Value },
    /// 未保存の下書きスライドカード（save_slide の下書き確定型・Task 11.3）。
    SlideDraft { draft: serde_json::Value },
    /// 未保存の下書き CSV カード（save_csv の下書き確定型・Task 11.11）。
    CsvDraft { draft: serde_json::Value },
    /// AI が作成/編集した文書への参照カード（#381）。`document = {id, name, kind, version}`。
    DocumentRef { document: serde_json::Value },
    /// **レガシー**: 未保存の下書き Word 文書カード（#332・#381 で廃止）。
    /// 新規に発火しない。`generation_event` の replay 互換のためだけに残す。
    DocumentDraft { draft: serde_json::Value },
    /// skill ツールの発動記録（#344）。`skill = {skill_id, skill_version, name}`。
    /// `generation_event` に append され replay 可能（監査・再現性）。content へは projection
    /// しない（instructions は tool_result block として履歴に残る）。UI はチップ表示に使う。
    SkillInvoked { skill: serde_json::Value },
    /// サブエージェント委譲の記録（#391）。
    /// `subagent = {objective, boundary, steps, tool_calls, tokens}`。
    /// `SkillInvoked` と同型で `generation_event` に append され replay 可能（監査・再現性）。
    /// content へは projection しない（findings は tool_result block として履歴に残る）。
    /// UI はツール実行表示の展開で担当範囲とステップ数を出すのに使う。
    SubagentRun { subagent: serde_json::Value },
    /// 計画の改訂（自律エージェント・Task 5.2）。サブタスク列を丸ごと配信する。
    Plan { subtasks: Vec<PlanSubtask> },
    /// 予算上限への接近警告（Task 5.7）。
    BudgetWarning { kind: String, used: u64, limit: u64 },
    /// 承認要求（破壊系/egress/高コスト・Task 5.6）。UI が承認ダイアログを出す。
    ApprovalRequested {
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
        reason: String,
    },
    /// 承認結果（許可/却下・Task 5.6）。
    ApprovalResolved {
        tool_call_id: String,
        approved: bool,
    },
    /// 失敗回復の判断（自己修正リトライ／ループ検出停止・Task 5.5）。
    FailureRecovery { detail: String, action: String },
    /// 状態遷移（running/waiting_approval/done/failed/cancelled）。UI の生成状態表示に使う。
    Status { status: RunStatus },
    /// エラー（生成失敗）。
    Error { message: String },
    /// 完了（確定した assistant message id）。
    Done { message_id: Uuid },
}

/// 計画のサブタスク 1 件（SSE `plan` イベント用・agent-core `Subtask` のミラー）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlanSubtask {
    pub id: String,
    pub title: String,
    /// todo / doing / done / blocked。
    pub status: String,
}

impl StreamEventKind {
    /// `generation_event.type` 列に入れる短い種別名（デバッグ/索引用）。
    pub fn tag(&self) -> &'static str {
        match self {
            StreamEventKind::Token { .. } => "token",
            StreamEventKind::Thinking { .. } => "thinking",
            StreamEventKind::ToolCall { .. } => "tool_call",
            StreamEventKind::ToolResult { .. } => "tool_result",
            StreamEventKind::Citation(_) => "citation",
            StreamEventKind::FileRef { .. } => "file_ref",
            StreamEventKind::GenerativeUi { .. } => "generative_ui",
            StreamEventKind::WorkflowRef { .. } => "workflow_ref",
            StreamEventKind::NoteRef { .. } => "note_ref",
            StreamEventKind::NoteDraft { .. } => "note_draft",
            StreamEventKind::SlideDraft { .. } => "slide_draft",
            StreamEventKind::CsvDraft { .. } => "csv_draft",
            StreamEventKind::DocumentRef { .. } => "document_ref",
            StreamEventKind::DocumentDraft { .. } => "document_draft",
            StreamEventKind::SkillInvoked { .. } => "skill_invoked",
            StreamEventKind::SubagentRun { .. } => "subagent_run",
            StreamEventKind::Plan { .. } => "plan",
            StreamEventKind::BudgetWarning { .. } => "budget_warning",
            StreamEventKind::ApprovalRequested { .. } => "approval_requested",
            StreamEventKind::ApprovalResolved { .. } => "approval_resolved",
            StreamEventKind::FailureRecovery { .. } => "failure_recovery",
            StreamEventKind::Status { .. } => "status",
            StreamEventKind::Error { .. } => "error",
            StreamEventKind::Done { .. } => "done",
        }
    }
}

/// SSE / replay の 1 イベント（seq 付き）。`id: <seq>` で重複排除する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StreamEvent {
    /// run ごと単調増加の seq（＝SSE の `id` / `Last-Event-ID`）。
    pub seq: i64,
    #[serde(flatten)]
    pub event: StreamEventKind,
}
