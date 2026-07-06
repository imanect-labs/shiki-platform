//! shiki-sandbox-client — サンドボックス制御の契約と client。
//!
//! shiki-server（api）はこのクレートの `Sandbox` トレイトだけに依存する（orchestrator への唯一の面）。
//! 重い V8/execution/kernel は sidecar バイナリ側にあり、orchestrator が別プロセスとして spawn する。
//!
//! - `spec` … `Sandbox` トレイトとドメイン型（proto の正本）
//! - `client` … `GrpcSandboxClient`（orchestrator への gRPC）
//! - `fake` … `FakeSandbox`（テスト用インメモリ実装）
//! - `pb` … tonic 生成のワイヤ型（`convert` で相互変換）

mod client;
mod convert;
mod error;
mod fake;
mod spec;

/// tonic 生成の proto 型（codegen が正・OUT_DIR）。
pub mod pb {
    #![allow(clippy::all, clippy::pedantic, unreachable_pub, missing_docs)]
    tonic::include_proto!("shiki.sandbox.v1");
}

pub use client::GrpcSandboxClient;
pub use error::SandboxError;
pub use fake::{FakeExecResult, FakeSandbox};
pub use spec::{
    DirEntry, Egress, EgressRule, ExecEvent, ExecRequest, LimitKind, Sandbox, SandboxBackend,
    SandboxHandle, SandboxLifetime, SandboxLimits, SandboxSpec,
};

/// orchestrator（server）実装が使う tonic server スタブ。feature="server" でのみ有効。
#[cfg(feature = "server")]
pub mod server {
    pub use crate::pb::sandbox_service_server::{SandboxService, SandboxServiceServer};
}
