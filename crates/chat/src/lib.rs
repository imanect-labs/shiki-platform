//! shiki-chat — チャットドメイン（thread / message / content blocks）＋接続非依存生成。
//!
//! 設計の正本: docs/design.md §4.4 / §4.4.1、docs/roadmap/phase-3.md（Task 3.1/3.5/3.7/3.11）。
//!
//! - **ドメイン型** [`model`]: `content = ContentBlock[]`（フロント `chat-api.ts` と同型）、
//!   SSE イベント [`model::StreamEventKind`]。
//! - **生成は接続非依存ジョブ**（Task 3.11・design §4.4.1）: `POST /messages` は outbox TX で
//!   保存＋jobq enqueue して 202、SSE は `generation_event` を replay-then-subscribe で購読する。
//!   真実のソースは append-only な `generation_event(run_id, seq)`、`message.content` はその projection。
//!
//! Phase 3 は制約版（短ホライズン・doc_search 等の安全なツールのみ）。

// #[cfg(test)] は本番のみ厳格化する lint を許容する（他クレートに倣う）。
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

pub mod approver;
pub mod artifacts;
pub mod csv_tool;
pub mod document_tool;
pub mod error;
pub mod gui_actions;
pub mod model;
pub mod office_tool;
pub mod selection;
pub(crate) mod skill;
pub mod slide_templates;
pub mod slide_tool;
pub mod store;
pub mod worker;
pub mod workflow_tool;
pub mod workspace;

pub use approver::DbApprover;
pub use artifacts::StorageArtifactStore;
pub use csv_tool::{CsvPatchTool, CsvQueryTool, CsvWriteTool};
pub use document_tool::{DocumentEditTool, DocumentEmbedTool, DocumentReadTool, SaveNoteTool};
pub use error::ChatError;
pub use gui_actions::ChatSubmitHandler;
pub use model::{
    Attachment, Citation, ContentBlock, Message, PlanSubtask, Role, RunStatus, SelectionContext,
    SelectionKind, StreamEvent, StreamEventKind, Thread, ThreadRole,
};
pub use slide_tool::{SaveSlideTool, SlideEditTool, SlideReadTool};
pub use store::{ChatStore, ClaimedRun, PostResult, ThreadOrigin, CHAT_GENERATION_QUEUE};
pub use worker::{ChatWorker, WorkerConfig, WorkerDeps};
pub use workflow_tool::{EmitWorkflowTool, ReadWorkflowTool, WorkflowCatalogSource};
pub use workspace::StorageWorkspaceStore;
