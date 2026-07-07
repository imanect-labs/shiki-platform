//! ワークフローエンジン（Task 10.1a〜・docs/workflow/ が正本）。
//!
//! Stage A（本 PR は 10.1a）:
//! - [`vocab`]: ワークフロー語彙の単一ソース（node type・scope・event source・run event・codegen）。
//! - [`ir`]: IR の定義と静的制約（deny-unknown・$from/$template/条件木・ノード/エッジ/トリガ）。
//! - [`ir::validate`]: 保存時検証 V1〜V7（全件エラー収集）。
//! - [`store`]: IR を artifact（kind=workflow）として保存・バージョン管理・取得する薄い層。

pub mod capability;
pub mod concurrency;
pub mod control;
pub mod delegation;
pub mod ir;
pub mod nodes;
pub mod ratelimit;
pub mod retry;
pub mod run;
pub mod scheduler;
pub mod store;
pub mod vocab;

pub use capability::{
    check_scope_ceiling, effective_scopes, CapabilityAudit, EffectJournal, JournalDecision,
    ScopeCeiling,
};
pub use concurrency::{ConcurrencyStore, ScopeKind, Slot};
pub use control::{branch_port, switch_port};
pub use delegation::{DelegationError, DelegationStore, GrantRequest, RunAdmission};
pub use ir::validate::{validate, Catalog, ValidationError};
pub use ir::WorkflowIr;
pub use nodes::{
    AgentInvokeReq, CapabilityNodeExecutor, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
pub use ratelimit::{BucketConfig, TokenBucket};
pub use retry::{backoff_with_jitter, classify, RetryClass};
pub use run::{
    NodeContext, NodeExecutor, NodeResult, RunStatus, RunStore, StepStatus, WorkerConfig,
    WorkflowRunLauncher, WorkflowWorker,
};
pub use scheduler::{LeaderLease, RunLauncher, SchedulerStore};
pub use store::{WorkflowStore, WorkflowStoreError};
