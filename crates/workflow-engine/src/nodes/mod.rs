//! 能力ノードのアダプタ（Task 10.6a/10.8/10.10・engine.md §9.5）。
//!
//! 各ノードは能力ゲートウェイ（[`capability`](crate::capability)）を通して既存チョークポイント
//! （StorageService / SearchService / LlmGateway / Sandbox）を呼ぶ薄いアダプタ。本モジュールは
//! 純粋・自己完結な部分（http.request の宛先束縛照合・レスポンス要約）を提供する。ストレージ/
//! RAG/LLM/エージェントの実結線は server 側（AppState のチョークポイント注入）で行う。

pub mod capability;
pub mod capability_ai;
pub mod exec;
pub mod http;
pub mod ports;
pub mod resolver;
pub mod script;

pub use exec::CapabilityNodeExecutor;
pub use ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
