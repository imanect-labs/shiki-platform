//! shiki script 処理系（Task 10.7・script.md が正本）。
//!
//! 構成:
//! - [`compile`]: swc で TS→JS（型剥がし・ES2020）＋禁止構文 lint（保存時検証 V6 と共用）。
//! - [`engine`]: wasmtime 上で QuickJS ゲスト wasm を駆動（fuel/メモリ/epoch 上限）。
//!   ゲストは非特権プロセス内で使い捨て（1 実行 = 1 Store/Instance）。
//! - [`frames`]: server ⇄ runtime フレームの全数検証（敵対的入力前提・PIT-35・INV-4）。
//! - [`host`]: 能力呼び出しの委譲窓口（`HostCallHandler` トレイト。実際の認可・実行は
//!   呼び出し側 = shiki-server / workflow-engine が担う。runtime は資格情報を持たない・INV-1）。
//!
//! 生成 proto（tonic）は [`proto`] に再エクスポートする。

pub mod compile;
pub mod engine;
pub mod frames;
pub mod host;
pub mod server;
mod wasi_stub;

/// tonic 生成コード（`ServerToRuntime` / `RuntimeToServer` / `ScriptRuntime` サービス）。
#[allow(clippy::all, clippy::pedantic, unreachable_pub, missing_docs)]
pub mod proto {
    tonic::include_proto!("shiki.script_runtime.v1");
}

pub use compile::{compile, CompileError, CompiledScript};
pub use engine::{ExecOutcome, Limits, ScriptEngine, Termination};
pub use frames::{validate_host_call, FrameError};
pub use host::{HostCall, HostCallHandler, HostResponse};
