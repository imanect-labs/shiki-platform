//! ワークフローエンジン（Task 10.1a〜・docs/workflow/ が正本）。
//!
//! Stage A（本 PR は 10.1a）:
//! - [`vocab`]: ワークフロー語彙の単一ソース（node type・scope・event source・run event・codegen）。
//! - [`ir`]: IR の定義と静的制約（deny-unknown・$from/$template/条件木・ノード/エッジ/トリガ）。
//! - [`ir::validate`]: 保存時検証 V1〜V7（全件エラー収集）。
//! - [`store`]: IR を artifact（kind=workflow）として保存・バージョン管理・取得する薄い層。

pub mod approval;
pub mod capability;
pub mod concurrency;
pub mod control;
pub mod delegation;
pub mod ir;
pub mod layout;
pub mod nodes;
pub mod ratelimit;
pub mod registration;
pub mod retry;
pub mod run;
pub mod scheduler;
pub mod store;
pub mod summary;
pub mod vocab;

pub use approval::{
    approval_id_for, AcceptedDecision, ApprovalChoiceSpec, ApprovalResolution, ApprovalRow,
    ApprovalStore, ApprovalSuspend, AssigneeKind, AssigneeRef, DecisionStart, DecisionTx,
    InboxCursor,
};
pub use capability::{
    check_scope_ceiling, effective_scopes, CapabilityAudit, EffectJournal, JournalDecision,
    ScopeCeiling,
};
pub use concurrency::{ConcurrencyStore, ScopeKind, Slot};
pub use control::{branch_port, switch_port};
pub use delegation::{
    active_delegator, delegated_objects, DelegationError, DelegationStore, GrantRequest,
    RunAdmission,
};
pub use ir::scopes::complete_declared_scopes;
pub use ir::validate::{
    enable_blockers, pattern_within, required_text_empty, run_blockers, save_warnings,
    semantics_warnings, valid_host_pattern, validate, Catalog, CatalogTable, ValidationError,
    APPROVAL_STATUS_FIELD_SHOWN, APPROVAL_TIMEOUTS_EXCEED_RUN, REPLY_WITHOUT_INTERACTIVE,
    REQUIRED_TEXT_EMPTY, SEMANTICS_CHANGED, TEMPLATE_ESCAPED_VAR, TEMPLATE_UNCLOSED_BRACE,
    WRITE_FOLDER_REQUIRED,
};
pub use ir::WorkflowIr;
pub use layout::{EditorLayoutStore, LayoutError};
pub use nodes::{
    AgentInvokeReq, AgentLimits, AgentToolGateway, ApprovalTarget, CapabilityNodeExecutor,
    CodeRunReq, CsvPatchReq, CsvWriteReq, DataWriteOp, DataWriteReq, DocReadReq, ExecCtx,
    HttpSendReq, HttpSendResp, LlmInvokeReq, NodePorts, PortError, ResolvedSecretView,
    ResolvedSkillView, StorageWriteReq, WebFetchReq,
};
pub use ratelimit::{BucketConfig, TokenBucket};
pub use registration::{
    DelegationView, EnableError, RegistrationService, RegistrationView, SuggestedGrant,
};
pub use retry::{backoff_with_jitter, classify, RetryClass};
pub use run::{
    ApprovalWait, CancelSignal, ConcurrencyLimits, DataWaitCandidate, EventWaitGate,
    EventWaitSubscriber, NodeContext, NodeExecutor, NodeResult, PendingApproval, ResolvedApproval,
    RunOrigin, RunStatus, RunStore, RunStoreError, StepStatus, WorkerConfig, WorkflowRunLauncher,
    WorkflowWorker, DEFAULT_LEASE_SECS, WORKFLOW_GC_QUEUE,
};
pub use run::{
    CancelOutcome, GcReport, HistoryGcWorker, PromoteReport, ResumeOutcome, RunDetail, RunEventRow,
    RunListFilter, RunListItem, RunStatusReader, StepDetail, StepOverview,
};
pub use run::{
    FailureCategory, LiveState, OwnOutcome, ReplyBody, ReplyFile, ReplySection, ReplyTone,
    RunFailure, RunOutcomeReader, RunReply, WaitingFor,
};
pub use scheduler::{
    Coalesce, DataTriggerCandidate, EventTriggerRef, FileTriggerCandidate, FireOutcome,
    LaunchOutcome, LeaderLease, RunLauncher, SchedulerStore, SkipReason,
};
pub use store::{WorkflowStore, WorkflowStoreError};
pub use summary::{WorkflowSummary, WorkflowSummaryStore};
