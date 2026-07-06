//! バックエンド抽象。`Backend` が spec からサンドボックスインスタンスを作り、`Instance` が
//! exec/ファイル操作/破棄を担う。wasm 実装（secure-exec-sidecar 駆動）は `wasm` サブモジュール。
//! テストは `fake` のインメモリ実装で server ロジックを検証する。

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use sandbox_client::{DirEntry, ExecEvent, ExecRequest, SandboxError, SandboxSpec};

pub mod egress;
pub mod fake;
pub mod multi;
pub mod native;
pub mod wasm;

/// 生成済みサンドボックス 1 個の駆動面（handle に紐づく）。
#[async_trait]
pub trait Instance: Send + Sync {
    /// バックエンド固有のプロセス識別（子 PID 等・分離テスト用）。
    fn debug_id(&self) -> String;

    async fn exec(
        &self,
        req: ExecRequest,
    ) -> Result<BoxStream<'static, Result<ExecEvent, SandboxError>>, SandboxError>;
    async fn put_file(&self, path: &str, bytes: Vec<u8>) -> Result<(), SandboxError>;
    async fn get_file(&self, path: &str) -> Result<Vec<u8>, SandboxError>;
    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError>;
    async fn destroy(&self) -> Result<(), SandboxError>;
}

/// spec からインスタンスを作るファクトリ（隔離バックエンド）。
#[async_trait]
pub trait Backend: Send + Sync {
    async fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Instance>, SandboxError>;
}
