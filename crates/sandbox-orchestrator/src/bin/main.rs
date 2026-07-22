//! sandbox-orchestrator バイナリ（非特権プロセス）。gRPC を 127.0.0.1 で待ち受け、per-sandbox の
//! secure-exec-sidecar 子プロセスを spawn する。設定は env（figment）。

use std::sync::Arc;
use std::time::Duration;

use std::path::PathBuf;

use sandbox_client::server::SandboxServiceServer;
use sandbox_orchestrator::backend::firecracker::FirecrackerBackend;
use sandbox_orchestrator::backend::gvisor::GvisorBackend;
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
    netns_holder_bin: Option<String>,
}

/// gVisor（runsc）ティアの構成。`enabled` かつ runsc/rootfs が揃えば実バックエンドを組む。
#[derive(Debug, Default, Serialize, Deserialize)]
struct GvisorConfig {
    #[serde(default)]
    enabled: bool,
    runsc_bin: Option<String>,
    rootfs_dir: Option<String>,
    state_dir: Option<String>,
    /// メモリ watchdog の監視間隔 ms（未指定 2000・0 で無効・#346）。
    watchdog_interval_ms: Option<u64>,
}

/// Firecracker ティアの構成。`enabled` かつ bin/kernel/rootfs が揃えば実バックエンドを組む。
#[derive(Debug, Default, Serialize, Deserialize)]
struct FirecrackerConfig {
    #[serde(default)]
    enabled: bool,
    bin: Option<String>,
    kernel: Option<String>,
    rootfs: Option<String>,
    state_dir: Option<String>,
}

/// 実行ファイル隣の `shiki-netns-holder` を既定の holder バイナリとして探す。
fn default_holder_bin() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cand = exe.with_file_name("shiki-netns-holder");
    cand.is_file().then_some(cand)
}

/// gVisor バックエンドを構築する（enabled かつ runsc/rootfs が揃うとき）。失敗は warn して None。
fn build_gvisor(cfg: &GvisorConfig, holder_bin: Option<PathBuf>) -> Option<Arc<dyn Backend>> {
    if !cfg.enabled {
        return None;
    }
    let (Some(runsc), Some(rootfs)) = (&cfg.runsc_bin, &cfg.rootfs_dir) else {
        tracing::warn!("SANDBOX__GVISOR__ENABLED=1 だが RUNSC_BIN/ROOTFS_DIR 未設定。gVisor 無効");
        return None;
    };
    let state = cfg
        .state_dir
        .clone()
        .unwrap_or_else(|| "/run/sandbox/gvisor".to_string());
    let watchdog = match cfg.watchdog_interval_ms.unwrap_or(2_000) {
        0 => None,
        ms => Some(std::time::Duration::from_millis(ms)),
    };
    match GvisorBackend::new(
        runsc,
        PathBuf::from(rootfs),
        PathBuf::from(state),
        holder_bin,
        watchdog,
    ) {
        Ok(b) => {
            tracing::info!("gVisor バックエンドを構成しました（runsc={runsc}）");
            Some(Arc::new(b) as Arc<dyn Backend>)
        }
        Err(e) => {
            tracing::warn!("gVisor バックエンド構成失敗（無効化）: {e}");
            None
        }
    }
}

/// Firecracker バックエンドを構築する（enabled かつ bin/kernel/rootfs が揃うとき）。失敗は warn して None。
fn build_firecracker(cfg: &FirecrackerConfig) -> Option<Arc<dyn Backend>> {
    if !cfg.enabled {
        return None;
    }
    let (Some(bin), Some(kernel), Some(rootfs)) = (&cfg.bin, &cfg.kernel, &cfg.rootfs) else {
        tracing::warn!("SANDBOX__FIRECRACKER__ENABLED=1 だが BIN/KERNEL/ROOTFS 未設定。FC 無効");
        return None;
    };
    let state = cfg
        .state_dir
        .clone()
        .unwrap_or_else(|| "/run/sandbox/firecracker".to_string());
    match FirecrackerBackend::new(
        bin,
        PathBuf::from(kernel),
        PathBuf::from(rootfs),
        PathBuf::from(state),
    ) {
        Ok(b) => {
            tracing::info!("Firecracker バックエンドを構成しました（bin={bin}）");
            Some(Arc::new(b) as Arc<dyn Backend>)
        }
        Err(e) => {
            tracing::warn!("Firecracker バックエンド構成失敗（無効化）: {e}");
            None
        }
    }
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

    // wasm は常に構成。gVisor は enabled かつ runsc/rootfs が揃えば構成。Firecracker は PR3。
    let wasm: Arc<dyn Backend> =
        Arc::new(WasmBackend::new(config.sidecar_bin.clone(), env.clone()));

    // egress holder バイナリ（未指定なら実行ファイル隣を探す）。
    let holder_bin = config
        .netns_holder_bin
        .clone()
        .map(PathBuf::from)
        .or_else(default_holder_bin);

    let gvisor = build_gvisor(&config.gvisor, holder_bin.clone());
    let firecracker = build_firecracker(&config.firecracker);
    let backend = Arc::new(MultiBackend::new(wasm, gvisor, firecracker));
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
