//! gRPC サーバ（非特権 script-runtime プロセス側・script.md §4.2）。
//!
//! shiki-server から `Execute` 双方向ストリームで呼ばれる。1 ストリーム = 1 script 実行。
//! runtime は `ExecStart`（compiled_js・input・limits）を受けて [`ScriptEngine`] で実行し、
//! ゲストの能力呼び出しを `HostCall` フレームとして server へ送り、`HostCallResult` を待つ
//! （runtime は資格情報を持たない・INV-1）。実行結果は `ExecResult` で返す。
//!
//! ブロッキングな wasmtime 実行は専用スレッドで回し、`HostCall` の gRPC 往復は
//! チャネルで橋渡しする（実行スレッド ⇄ ストリームハンドラ）。

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use futures::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status, Streaming};

use crate::engine::{Limits, ScriptEngine};
use crate::host::{HostCall, HostResponse};
use crate::proto::script_runtime_server::ScriptRuntime;
use crate::proto::{
    self, runtime_to_server, server_to_runtime, ExecResult, HostCall as PbHostCall,
    RuntimeToServer, ServerToRuntime,
};

/// gRPC サービス実装。プリウォーム済み [`ScriptEngine`] を共有する。
pub struct ScriptRuntimeService {
    engine: Arc<ScriptEngine>,
}

impl ScriptRuntimeService {
    pub fn new(engine: Arc<ScriptEngine>) -> Self {
        ScriptRuntimeService { engine }
    }
}

/// 実行スレッド → ストリームハンドラへ渡すメッセージ。
enum ToStream {
    /// 能力呼び出しをサーバへ転送し結果を待つ（同期橋渡し）。
    HostCall(HostCall, std_mpsc::Sender<HostResponse>),
    /// 実行完了。
    Done(ExecResult),
}

#[tonic::async_trait]
impl ScriptRuntime for ScriptRuntimeService {
    type ExecuteStream =
        std::pin::Pin<Box<dyn Stream<Item = Result<RuntimeToServer, Status>> + Send>>;

    async fn execute(
        &self,
        request: Request<Streaming<ServerToRuntime>>,
    ) -> Result<Response<Self::ExecuteStream>, Status> {
        let mut inbound = request.into_inner();
        // 最初のメッセージは ExecStart。
        let Some(Ok(ServerToRuntime {
            msg: Some(server_to_runtime::Msg::Start(start)),
        })) = inbound.next().await
        else {
            return Err(Status::invalid_argument(
                "最初のメッセージは ExecStart が必要です",
            ));
        };

        let (out_tx, out_rx) = mpsc::channel::<Result<RuntimeToServer, Status>>(16);
        // 実行スレッド ⇄ ストリームの橋渡しチャネル。
        let (bridge_tx, bridge_rx) = std_mpsc::channel::<ToStream>();

        let engine = Arc::clone(&self.engine);
        let exec_id = start.exec_id.clone();
        let limits = to_limits(start.limits.as_ref());

        // wasmtime 実行は専用スレッド（ブロッキング）。
        let exec_id_run = exec_id.clone();
        std::thread::spawn(move || {
            let bridge = bridge_tx.clone();
            let host_fn: crate::engine::HostFn = Box::new(move |call: &HostCall| {
                let (resp_tx, resp_rx) = std_mpsc::channel::<HostResponse>();
                if bridge
                    .send(ToStream::HostCall(call.clone(), resp_tx))
                    .is_err()
                {
                    return host_err("bridge closed");
                }
                resp_rx
                    .recv()
                    .unwrap_or_else(|_| host_err("bridge dropped"))
            });
            let outcome = engine.run(
                &exec_id_run,
                &start.compiled_js,
                &start.input_json,
                limits,
                host_fn,
            );
            let _ = bridge_tx.send(ToStream::Done(to_exec_result(&exec_id_run, &outcome)));
        });

        // ストリームハンドラ: bridge の HostCall を server へ送り、HostCallResult を待つ。
        let exec_id_stream = exec_id.clone();
        tokio::spawn(async move {
            drive_stream(bridge_rx, inbound, out_tx, exec_id_stream).await;
        });

        let stream = ReceiverStream::new(out_rx);
        Ok(Response::new(Box::pin(stream)))
    }
}

/// bridge（実行スレッド）と inbound（server）の間を仲介する。
async fn drive_stream(
    bridge_rx: std_mpsc::Receiver<ToStream>,
    mut inbound: Streaming<ServerToRuntime>,
    out_tx: mpsc::Sender<Result<RuntimeToServer, Status>>,
    exec_id: String,
) {
    loop {
        // bridge はブロッキングチャネルなので spawn_blocking で待つ。
        let msg = tokio::task::block_in_place(|| bridge_rx.recv());
        let Ok(msg) = msg else { break };
        match msg {
            ToStream::HostCall(call, resp_tx) => {
                let pb = RuntimeToServer {
                    msg: Some(runtime_to_server::Msg::HostCall(PbHostCall {
                        exec_id: call.exec_id.clone(),
                        seq: call.seq,
                        api: call.api.clone(),
                        args_json: call.args.to_string(),
                    })),
                };
                if out_tx.send(Ok(pb)).await.is_err() {
                    let _ = resp_tx.send(host_err("stream closed"));
                    break;
                }
                // server から HostCallResult を待つ。
                let resp = match inbound.next().await {
                    Some(Ok(ServerToRuntime {
                        msg: Some(server_to_runtime::Msg::HostCallResult(r)),
                    })) if r.exec_id == exec_id => {
                        let payload: serde_json::Value = serde_json::from_str(&r.payload_json)
                            .unwrap_or(serde_json::Value::Null);
                        if r.ok {
                            HostResponse::Ok(payload)
                        } else {
                            HostResponse::Err {
                                message: payload
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("host error")
                                    .to_string(),
                                code: payload
                                    .get("code")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("internal")
                                    .to_string(),
                                retryable: payload
                                    .get("retryable")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false),
                            }
                        }
                    }
                    Some(Ok(ServerToRuntime {
                        msg: Some(server_to_runtime::Msg::Cancel(_)),
                    })) => host_err("cancelled"),
                    _ => host_err("expected HostCallResult"),
                };
                let _ = resp_tx.send(resp);
            }
            ToStream::Done(result) => {
                let _ = out_tx
                    .send(Ok(RuntimeToServer {
                        msg: Some(runtime_to_server::Msg::Result(result)),
                    }))
                    .await;
                break;
            }
        }
    }
}

fn to_limits(limits: Option<&proto::Limits>) -> Limits {
    let d = Limits::default();
    match limits {
        Some(l) => Limits {
            fuel: if l.fuel == 0 { d.fuel } else { l.fuel },
            memory_bytes: if l.memory_bytes == 0 {
                d.memory_bytes
            } else {
                l.memory_bytes as usize
            },
            epoch_deadline: if l.epoch_deadline_ms == 0 {
                d.epoch_deadline
            } else {
                std::time::Duration::from_millis(u64::from(l.epoch_deadline_ms))
            },
            max_host_calls: if l.max_host_calls == 0 {
                d.max_host_calls
            } else {
                u64::from(l.max_host_calls)
            },
        },
        None => d,
    }
}

fn to_exec_result(exec_id: &str, outcome: &crate::engine::ExecOutcome) -> ExecResult {
    let (error_message, error_code, retryable) = outcome
        .error
        .clone()
        .unwrap_or_else(|| (String::new(), String::new(), false));
    ExecResult {
        exec_id: exec_id.to_string(),
        ok: outcome.ok,
        value_json: outcome
            .value
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default(),
        error_message,
        error_code,
        retryable,
        termination: format!("{:?}", outcome.termination),
    }
}

fn host_err(msg: &str) -> HostResponse {
    HostResponse::Err {
        message: msg.to_string(),
        code: "internal".into(),
        retryable: true,
    }
}
