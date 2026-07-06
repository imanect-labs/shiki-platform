//! sandbox-orchestrator バイナリ（非特権プロセス）。gRPC を 127.0.0.1 で待ち受け、per-sandbox の
//! secure-exec-sidecar 子プロセスを spawn する。設定は env（figment）。

use std::sync::Arc;
use std::time::Duration;

use sandbox_client::server::SandboxServiceServer;
use sandbox_orchestrator::backend::multi::MultiBackend;
use sandbox_orchestrator::backend::wasm::WasmBackend;
use sandbox_orchestrator::backend::Backend;
use sandbox_orchestrator::config::OrchestratorEnv;
use sandbox_orchestrator::registry::{sweep_loop, Registry};
use sandbox_orchestrator::server::SandboxSvc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    /// 待ち受けアドレス（compose 網内・127.0.0.1 バインド）。
    listen: String,
    /// secure-exec-sidecar バイナリのパス（未指定なら PATH/env）。
    sidecar_bin: Option<String>,
    /// ゲストコマンド（wasm）のフラットなディレクトリ。`/__secure_exec/commands/0` に
    /// host_dir マウントされる。未指定なら software 要求を拒否する（実行時 DL 禁止・PIT-33）。
    commands_dir: Option<String>,
    /// gVisor ティア設定（`SANDBOX__GVISOR__*`）。未設定/未 enabled なら wasm のみで起動。
    #[serde(default)]
    gvisor: GvisorConfig,
    /// Firecracker ティア設定（`SANDBOX__FIRECRACKER__*`）。
    #[serde(default)]
    firecracker: FirecrackerConfig,
    /// egress netns holder バイナリのパス（未指定なら実行ファイル隣の `shiki-netns-holder`）。
    // PR2/PR3 のネイティブバックエンドが egress スタック起動時に参照する。
    #[allow(dead_code)]
    netns_holder_bin: Option<String>,
}

/// gVisor（runsc）ティアの構成。PR2 で `enabled` 時に実バックエンドを組む。
// runsc_bin/rootfs_dir/state_dir は PR2 のバックエンド配線で参照する（先行して env スキーマを確定）。
#[allow(dead_code)]
#[derive(Debug, Default, Serialize, Deserialize)]
struct GvisorConfig {
    #[serde(default)]
    enabled: bool,
    runsc_bin: Option<String>,
    rootfs_dir: Option<String>,
    state_dir: Option<String>,
}

/// Firecracker ティアの構成。PR3 で `enabled` 時に実バックエンドを組む。
// bin/kernel/rootfs/state_dir は PR3 のバックエンド配線で参照する。
#[allow(dead_code)]
#[derive(Debug, Default, Serialize, Deserialize)]
struct FirecrackerConfig {
    #[serde(default)]
    enabled: bool,
    bin: Option<String>,
    kernel: Option<String>,
    rootfs: Option<String>,
    state_dir: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "127.0.0.1:50000".to_string(),
            sidecar_bin: None,
            commands_dir: None,
            gvisor: GvisorConfig::default(),
            firecracker: FirecrackerConfig::default(),
            netns_holder_bin: None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // ネスト節（`SANDBOX__GVISOR__ENABLED` 等）を拾うため `__` を階層区切りにする。
    // 既存のフラットキー（`SANDBOX__LISTEN` 等）は top-level フィールドとして解決される。
    let config: Config =
        figment::Figment::from(figment::providers::Serialized::defaults(Config::default()))
            .merge(figment::providers::Env::prefixed("SANDBOX__").split("__"))
            .extract()?;

    let env = OrchestratorEnv {
        commands_dir: config.commands_dir.clone().map(std::path::PathBuf::from),
        ..OrchestratorEnv::default()
    };
    let registry = Arc::new(Registry::new());

    // wasm は常に構成。gVisor/Firecracker は PR2/PR3 で enabled 時に実装バックエンドを組む。
    let wasm: Arc<dyn Backend> =
        Arc::new(WasmBackend::new(config.sidecar_bin.clone(), env.clone()));
    if config.gvisor.enabled {
        tracing::warn!(
            "SANDBOX__GVISOR__ENABLED=1 だが gVisor バックエンドは未配線（PR2）。wasm のみで起動"
        );
    }
    if config.firecracker.enabled {
        tracing::warn!(
            "SANDBOX__FIRECRACKER__ENABLED=1 だが Firecracker バックエンドは未配線（PR3）。wasm のみで起動"
        );
    }
    let backend = Arc::new(MultiBackend::new(wasm, None, None));
    let svc = SandboxSvc::new(backend, Arc::clone(&registry), env);

    tokio::spawn(sweep_loop(Arc::clone(&registry), Duration::from_secs(5)));

    let addr = config.listen.parse()?;
    tracing::info!(%addr, "sandbox-orchestrator listening");
    tonic::transport::Server::builder()
        .add_service(SandboxServiceServer::new(svc))
        .serve(addr)
        .await?;
    Ok(())
}
