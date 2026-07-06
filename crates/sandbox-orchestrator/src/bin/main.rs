//! sandbox-orchestrator バイナリ（非特権プロセス）。gRPC を 127.0.0.1 で待ち受け、per-sandbox の
//! secure-exec-sidecar 子プロセスを spawn する。設定は env（figment）。

use std::sync::Arc;
use std::time::Duration;

use sandbox_client::server::SandboxServiceServer;
use sandbox_orchestrator::backend::wasm::WasmBackend;
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "127.0.0.1:50000".to_string(),
            sidecar_bin: None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let config: Config =
        figment::Figment::from(figment::providers::Serialized::defaults(Config::default()))
            .merge(figment::providers::Env::prefixed("SANDBOX__"))
            .extract()?;

    let env = OrchestratorEnv::default();
    let registry = Arc::new(Registry::new());
    let backend = Arc::new(WasmBackend::new(config.sidecar_bin.clone(), env.clone()));
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
