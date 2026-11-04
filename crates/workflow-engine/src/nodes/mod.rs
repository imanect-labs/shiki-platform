//! 能力ノードのアダプタ（Task 10.6a/10.8/10.10・engine.md §9.5）。
//!
//! 各ノードは能力ゲートウェイ（[`capability`](crate::capability)）を通して既存チョークポイント
//! （StorageService / SearchService / LlmGateway / Sandbox）を呼ぶ薄いアダプタ。本モジュールは
//! 純粋・自己完結な部分（http.request の宛先束縛照合・レスポンス要約）を提供する。ストレージ/
//! RAG/LLM/エージェントの実結線は server 側（AppState のチョークポイント注入）で行う。

mod agent_gateway;
pub mod approval;
pub mod capability;
pub mod capability_agent;
pub mod capability_ai;
pub mod capability_data;
mod capability_data_keyed;
mod csv_compose;
mod csv_encoding;
mod data_upsert;
mod date;
pub mod decide;
mod decide_reply;
pub mod exec;
pub mod http;
pub mod http_headers;
pub mod llm_structured;
pub mod ports;
pub mod reply;
pub mod resolver;
pub mod script;
pub mod skill;

pub use capability_agent::AgentLimits;
pub use exec::CapabilityNodeExecutor;
pub use ports::{
    AgentInvokeReq, AgentToolGateway, ApprovalTarget, CodeRunReq, CsvPatchReq, CsvWriteReq,
    DataWriteOp, DataWriteReq, DecideCache, DocReadReq, ExecCtx, HttpSendReq, HttpSendResp,
    LlmDecideReq, LlmInvokeReq, LlmRepair, NodePorts, PortError, ResolvedSecretView,
    ResolvedSkillView, StorageListReq, StorageWriteReq, WebFetchReq,
};
