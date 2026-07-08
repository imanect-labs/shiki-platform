//! `Sandbox` トレイトとドメイン型（proto の正本）。
//!
//! shiki-server はこのトレイトだけに依存する。実装は `GrpcSandboxClient`（orchestrator への gRPC）と
//! `FakeSandbox`（インメモリ・テスト用）。proto へのワイヤ変換は `convert.rs`。

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::error::SandboxError;

/// 隔離バックエンド種別。既定は `Wasm`。gVisor はフル Linux（KVM 不要）、Firecracker は VM 級隔離。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Wasm,
    Gvisor,
    Firecracker,
}

/// 隔離強度クラス（PIT-24: 「VM 級隔離（NFR-1）」は KVM 前提であることを正直に表明する）。
///
/// 呼び出し側は機微度の高いワークロードで `UserspaceKernel`（gVisor）を拒否/警告する判断に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationClass {
    /// KVM microVM（Firecracker）。最強・NFR-1 を満たす。
    VmLevel,
    /// ユーザ空間カーネル（gVisor）。脱出 CVE 実績があり VM 級より一段弱い。
    UserspaceKernel,
    /// wasm プロセス隔離（secure-exec）。V8＋wasm・仮想 FS/net。
    WasmProcess,
}

impl SandboxBackend {
    /// 隔離強度クラスを返す（監査記録・機微度ポリシ判定用・PIT-24）。
    #[must_use]
    pub fn isolation_class(self) -> IsolationClass {
        match self {
            SandboxBackend::Firecracker => IsolationClass::VmLevel,
            SandboxBackend::Gvisor => IsolationClass::UserspaceKernel,
            SandboxBackend::Wasm => IsolationClass::WasmProcess,
        }
    }
}

/// egress ルール（宛先ホスト/ポート）。ホスト照合は完全一致 or 明示ワイルドカードのみ（部分一致禁止）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRule {
    pub host_pattern: String,
    /// 0 = 全ポート。
    pub port: u16,
}

/// egress ポリシ。allowlist 空＝全遮断。`dynamic_allow` は当該 run 限定（web.fetch）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Egress {
    pub static_allow: Vec<EgressRule>,
    pub dynamic_allow: Vec<EgressRule>,
    pub deny_overlay: Vec<EgressRule>,
    /// 宛先束縛の安全側。常に false（シークレット添付不可）。
    pub secret_attach: bool,
}

impl Egress {
    /// 完全遮断（code_interpreter 用）。
    pub fn blocked() -> Self {
        Egress::default()
    }
}

/// リソース上限。0 は「orchestrator 既定を使う」を意味しない（呼び出し側が必ず埋める）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxLimits {
    pub wall_clock_ms: u64,
    pub exec_timeout_ms: u64,
    pub memory_mb: u64,
    pub max_processes: u32,
    pub max_fs_bytes: u64,
    pub max_open_fds: u32,
    pub max_output_bytes: u64,
}

impl SandboxLimits {
    /// code_interpreter / web ツールの厳しめ既定（短命・小メモリ・短タイムアウト）。
    pub fn constrained() -> Self {
        SandboxLimits {
            wall_clock_ms: 30_000,
            exec_timeout_ms: 30_000,
            memory_mb: 512,
            max_processes: 8,
            max_fs_bytes: 128 * 1024 * 1024,
            max_open_fds: 256,
            max_output_bytes: 1024 * 1024,
        }
    }
}

/// サンドボックスの寿命。アルファは Ephemeral のみ（実行して破棄）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxLifetime {
    Ephemeral { ttl_ms: u64 },
    Persistent,
}

/// サンドボックス生成仕様。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    pub backend: SandboxBackend,
    /// 監査・分離のためのテナント識別（authz 判定は shiki-server 側で済ませる）。
    pub tenant_id: String,
    pub org: String,
    pub principal: String,
    pub limits: SandboxLimits,
    pub egress: Egress,
    /// 有効化するゲストコマンドパッケージ名（例 coreutils/curl/wget）。
    pub software: Vec<String>,
    /// ストレージマウント可否。アルファは false 固定（true は Unimplemented）。
    pub mounts_allowed: bool,
    pub lifetime: SandboxLifetime,
}

impl SandboxSpec {
    /// code_interpreter 用（ネット遮断・まっさら・短命・Python 実行に必要な最小 software）。
    pub fn code_interpreter(tenant_id: String, org: String, principal: String) -> Self {
        SandboxSpec {
            backend: SandboxBackend::Wasm,
            tenant_id,
            org,
            principal,
            limits: SandboxLimits::constrained(),
            egress: Egress::blocked(),
            software: Vec::new(),
            mounts_allowed: false,
            lifetime: SandboxLifetime::Ephemeral { ttl_ms: 60_000 },
        }
    }

    /// 自律エージェントの shell 用（Task 5.4）。ネット遮断・短命・coreutils 等のゲストコマンド同梱。
    ///
    /// ワークスペースは seed→exec→sync（host 側が `put_file`/`get_file`）で round-trip する（永続 mount は
    /// post-alpha のため `mounts_allowed=false`）。egress は既定遮断（ネットワークは承認ゲート対象・5.6）。
    pub fn agent_shell(
        tenant_id: String,
        org: String,
        principal: String,
        software: Vec<String>,
    ) -> Self {
        SandboxSpec {
            backend: SandboxBackend::Wasm,
            tenant_id,
            org,
            principal,
            limits: SandboxLimits::constrained(),
            egress: Egress::blocked(),
            software,
            mounts_allowed: false,
            lifetime: SandboxLifetime::Ephemeral { ttl_ms: 60_000 },
        }
    }

    /// web_fetch 用（**当該 run 限定の dynamic_allow に取得先ホストのみ**を載せる・design §4.4）。
    ///
    /// 静的 allowlist は空＝取得先以外は全遮断。シークレット添付は不可（`secret_attach=false` 固定）。
    /// 管理者 `deny_overlay` は orchestrator 側で重なる。
    pub fn web_fetch(
        tenant_id: String,
        org: String,
        principal: String,
        host: String,
        port: u16,
    ) -> Self {
        SandboxSpec {
            backend: SandboxBackend::Wasm,
            tenant_id,
            org,
            principal,
            limits: SandboxLimits::constrained(),
            egress: Egress {
                static_allow: Vec::new(),
                dynamic_allow: vec![EgressRule {
                    host_pattern: host,
                    port,
                }],
                deny_overlay: Vec::new(),
                secret_attach: false,
            },
            software: Vec::new(),
            mounts_allowed: false,
            lifetime: SandboxLifetime::Ephemeral { ttl_ms: 60_000 },
        }
    }
}

/// 生成済みサンドボックスのハンドル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxHandle {
    pub id: String,
}

/// 実行要求。Python はコードを guest に書いて実行、Shell は `sh -c`（ls/curl 等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecRequest {
    Python {
        code: String,
        timeout_ms: Option<u64>,
    },
    Shell {
        cmd: String,
        timeout_ms: Option<u64>,
    },
}

/// 実行ストリームのイベント。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exited { code: i32 },
    LimitExceeded { kind: LimitKind, detail: String },
}

/// リソース超過の種別（人間可読な理由へ整形するため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    WallClock,
    Memory,
    Output,
    Process,
    Filesystem,
}

/// ディレクトリエントリ（成果物回収の差分検出用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// サンドボックス制御の可搬トレイト（差し替え点）。shiki-server はこれだけに依存する。
#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError>;
    async fn exec(
        &self,
        handle: &SandboxHandle,
        req: ExecRequest,
    ) -> Result<BoxStream<'static, Result<ExecEvent, SandboxError>>, SandboxError>;
    async fn put_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SandboxError>;
    async fn get_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, SandboxError>;
    async fn list_dir(
        &self,
        handle: &SandboxHandle,
        path: &str,
    ) -> Result<Vec<DirEntry>, SandboxError>;
    async fn destroy(&self, handle: &SandboxHandle) -> Result<(), SandboxError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_class_per_backend() {
        assert_eq!(
            SandboxBackend::Firecracker.isolation_class(),
            IsolationClass::VmLevel
        );
        assert_eq!(
            SandboxBackend::Gvisor.isolation_class(),
            IsolationClass::UserspaceKernel
        );
        assert_eq!(
            SandboxBackend::Wasm.isolation_class(),
            IsolationClass::WasmProcess
        );
    }
}
