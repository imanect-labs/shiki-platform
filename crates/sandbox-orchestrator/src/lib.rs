//! shiki-sandbox-orchestrator — 非特権の特権分離プロセス。
//!
//! shiki-server から gRPC（sandbox-client 契約）を受け、per-sandbox の secure-exec-sidecar 子プロセスを
//! spawn して Python/シェルを実行する。ゲスト由来入力は全て敵対的として `validate` で検証する（PIT-23）。

pub mod backend;
pub mod config;
pub mod registry;
pub mod server;
pub mod software;
pub mod validate;
