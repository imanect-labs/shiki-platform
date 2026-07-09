//! shiki-agent-core — LLM↔ツールのループ（制約版）＋`Tool` トレイト。
//!
//! 設計の正本: docs/design.md §4.4、docs/roadmap/phase-3.md（Task 3.3/3.4/3.9）。
//!
//! - **ツールセット非依存**（[`Tool`] トレイトで差す）。Phase 3 は短ホライズン・安全ツールのみ
//!   （doc_search）。同じコアを Phase 4/5 でフルツール化できる構造。
//! - **エージェントモード時のみ作動**: 通常チャット（agent_mode OFF）は chat が llm-gateway を
//!   直叩きし、[`tools::run_doc_search`] を古典 RAG 注入に再利用する（ツールループ無し）。
//! - **ツール自動選択ポリシ**（Task 3.9）: 全ツール提示＋モデル自動選択。破壊/権限/高コスト系
//!   （`requires_confirmation()`）は事前許可が無ければ実行しない。
//! - **confused-deputy 防御**: ツールは常に発話ユーザーの `AuthContext` で権限判定する（昇格しない）。

// #[cfg(test)] は本番のみ厳格化する lint を許容する。
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::pedantic
    )
)]

pub mod agent;
pub(crate) mod agent_gate;
pub mod approval;
pub mod budget;
pub mod checkpoint;
pub mod context;
pub mod event;
pub mod loop_detect;
pub mod plan;
pub mod profile;
pub mod tool;
pub mod tools;
pub mod vocab;
pub mod workspace;

pub use agent::{run_agent, AgentStop, RunContext};
pub use approval::{ApprovalDecision, ApprovalPolicy, Approver};
pub use budget::{Budget, BudgetCheck, BudgetKind, Spent};
pub use checkpoint::Checkpoint;
pub use event::{AgentError, AgentEvent, EventSink, RecoveryAction};
pub use plan::{Plan, Subtask, SubtaskStatus};
pub use profile::{AgentOptions, AgentOutcome, AgentProfile};
pub use tool::{ArtifactRef, ArtifactStore, Citation, Tool, ToolError, ToolOutcome};
pub use tools::{
    run_doc_search, CodeInterpreterTool, DocSearchResult, DocSearchTool, FsDeleteTool, FsEditTool,
    FsListTool, FsReadTool, FsWriteTool, GrepTool, ShellTool, WebFetchTool, WebSearchTool,
};
pub use vocab::ToolName;
pub use workspace::{WorkspaceEntry, WorkspaceStore, WorkspaceWrite};
// サンドボックス契約を再輸出（chat は agent-core 経由で code_interpreter を配線する）。
// `SandboxBackend` は admin ポリシーで隔離ティアを選ぶ導線で chat 側が渡す。
pub use sandbox_client::{Sandbox, SandboxBackend};
