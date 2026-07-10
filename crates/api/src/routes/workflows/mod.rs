//! ワークフロー API（保存/検証・run・有効化）。500 行ゲートのためサブモジュール分割。
//!
//! - [`save`] — IR の保存（V1〜V7 検証）・取得・検証のみ（Task 10.1a/10.12）
//! - [`runs`] — run 起動・状態取得（Task 10.2/10.14）
//! - [`registration`] — 有効化・同意・トリガ実体化（Task 10.4a・engine.md §10）

pub mod registration;
pub mod runs;
pub mod save;

pub use registration::{
    consent_plan, disable_workflow, enable_workflow, get_registration, ConsentPlanResponse,
    DelegationItem, EnableRequest, EnableResponse, GrantItem, RegistrationResponse,
    SuggestedGrantItem,
};
pub use runs::{
    get_workflow_run, start_workflow_run, RunStatusResponse, StartRunRequest, StartRunResponse,
    StepStatusItem,
};
pub use save::{
    create_workflow, get_workflow, get_workflow_version, update_workflow, validate_workflow,
    SaveWorkflowRequest, SaveWorkflowResponse, ValidateWorkflowRequest, ValidateWorkflowResponse,
    ValidationErrorResponse, WorkflowVersionResponse,
};
