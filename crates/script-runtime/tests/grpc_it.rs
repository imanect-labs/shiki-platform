//! gRPC 経路（UDS）の結合テスト: script-runtime プロセスと同型のサーバへ ExecStart を
//! 送り、HostCall を server 側で処理し、ExecResult を受ける往復を検証する（Task 10.7）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::type_complexity,
    clippy::map_unwrap_or
)]

use std::sync::Arc;

use hyper_util::rt::TokioIo;
use script_runtime::compile::compile;
use script_runtime::engine::ScriptEngine;
use script_runtime::proto::script_runtime_client::ScriptRuntimeClient;
use script_runtime::proto::script_runtime_server::ScriptRuntimeServer;
use script_runtime::proto::{
    runtime_to_server, server_to_runtime, ExecStart, HostCallResult, ServerToRuntime,
};
use script_runtime::server::ScriptRuntimeService;
use tokio::net::UnixStream;
use tokio_stream::StreamExt;
use tonic::transport::{Endpoint, Server, Uri};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_execute_roundtrip_with_host_call() {
    let dir = std::env::temp_dir();
    let sock = dir.join(format!("shiki-sr-test-{}.sock", uuid_like()));
    let sock_path = sock.clone();
    let _ = std::fs::remove_file(&sock);

    // サーバ起動（UDS）。
    let engine = Arc::new(ScriptEngine::new().expect("engine"));
    let service = ScriptRuntimeService::new(engine);
    let listener = tokio::net::UnixListener::bind(&sock).expect("bind");
    let incoming = tokio_stream::wrappers::UnixListenerStream::new(listener);
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(ScriptRuntimeServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });

    // クライアント接続（UDS）。
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower_connector(sock_path.clone()))
        .await
        .expect("connect");
    let mut client = ScriptRuntimeClient::new(channel);

    // script は storage.read を 1 回呼び、その body を返す。
    let compiled = compile("function main(input) { return Shiki.storage.read(input.id).body; }")
        .expect("compile");
    let (tx, rx) = tokio::sync::mpsc::channel::<ServerToRuntime>(8);
    tx.send(ServerToRuntime {
        msg: Some(server_to_runtime::Msg::Start(ExecStart {
            exec_id: "e-grpc".into(),
            compiled_js: compiled.compiled_js,
            input_json: "{\"id\":\"doc-9\"}".into(),
            limits: None,
        })),
    })
    .await
    .unwrap();

    let outbound = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut inbound = client
        .execute(outbound)
        .await
        .expect("execute")
        .into_inner();

    let mut final_value: Option<String> = None;
    while let Some(item) = inbound.next().await {
        let msg = item.expect("stream item").msg.expect("msg");
        match msg {
            runtime_to_server::Msg::HostCall(hc) => {
                assert_eq!(hc.exec_id, "e-grpc");
                assert_eq!(hc.api, "storage.read");
                // server 役として結果を返す。
                tx.send(ServerToRuntime {
                    msg: Some(server_to_runtime::Msg::HostCallResult(HostCallResult {
                        exec_id: hc.exec_id.clone(),
                        seq: hc.seq,
                        ok: true,
                        payload_json: "{\"body\":\"grpc-hello\"}".into(),
                    })),
                })
                .await
                .unwrap();
            }
            runtime_to_server::Msg::Log(_) => {}
            runtime_to_server::Msg::Result(r) => {
                assert!(r.ok, "{}", r.error_message);
                final_value = Some(r.value_json);
                break;
            }
        }
    }

    assert_eq!(final_value.as_deref(), Some("\"grpc-hello\""));
    server.abort();
    let _ = std::fs::remove_file(&sock);
}

/// UDS 用の tower Connector を作る。
fn tower_connector(
    path: std::path::PathBuf,
) -> impl tower::Service<
    Uri,
    Response = TokioIo<UnixStream>,
    Error = std::io::Error,
    Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TokioIo<UnixStream>, std::io::Error>> + Send>,
    >,
> + Clone {
    tower::service_fn(move |_: Uri| {
        let path = path.clone();
        Box::pin(async move {
            let stream = UnixStream::connect(path).await?;
            Ok(TokioIo::new(stream))
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
    })
}

/// テスト用の擬似ユニーク文字列（uuid 依存を避ける）。
fn uuid_like() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
