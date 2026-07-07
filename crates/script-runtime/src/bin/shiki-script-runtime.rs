//! shiki script-runtime プロセス（非特権・gRPC over UDS）。
//!
//! shiki-server から独立した最小権限プロセスとして起動し、`Execute` ストリームで
//! script を実行する。資格情報・シークレット・AuthContext は一切持たない（script.md §5）。
//!
//! 使い方: `shiki-script-runtime <uds-path>`（省略時は `SHIKI_SCRIPT_RUNTIME_UDS` か既定）。
//! ※ seccomp/namespace 等の OS レベル隔離はデプロイ側（compose/systemd）で付与する。

use std::sync::Arc;

use script_runtime::engine::ScriptEngine;
use script_runtime::proto::script_runtime_server::ScriptRuntimeServer;
use script_runtime::server::ScriptRuntimeService;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let uds_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SHIKI_SCRIPT_RUNTIME_UDS").ok())
        .unwrap_or_else(|| "/tmp/shiki-script-runtime.sock".to_string());

    // 既存ソケットを掃除してから bind。
    let _ = std::fs::remove_file(&uds_path);
    let listener = UnixListener::bind(&uds_path)?;
    let incoming = UnixListenerStream::new(listener);

    // ゲスト wasm を 1 回コンパイルしてプリウォーム（コールドスタート ms 級の前提）。
    let engine = Arc::new(ScriptEngine::new().map_err(std::io::Error::other)?);
    let service = ScriptRuntimeService::new(engine);

    tracing::info!(uds = %uds_path, "script-runtime を起動しました");
    tonic::transport::Server::builder()
        .add_service(ScriptRuntimeServer::new(service))
        .serve_with_incoming(incoming)
        .await?;
    Ok(())
}
