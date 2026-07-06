//! wasm インスタンス: 1 VM に対する exec / ファイル操作 / 破棄。

use async_trait::async_trait;
use base64::Engine;
use futures::stream::BoxStream;
use sandbox_client::{DirEntry, ExecEvent, ExecRequest, SandboxError};
use secure_exec_client::wire;
use secure_exec_client::SidecarTransport;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;

use super::{request, vm_scope};

const ENTRYPOINT: &str = "/workspace/main.py";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// 生成済み VM 1 個。破棄時に DisposeVm＋子プロセス kill。
pub struct WasmInstance {
    transport: Arc<SidecarTransport>,
    connection_id: String,
    session_id: String,
    vm_id: String,
    /// プロセス横断で一意な識別（各 sidecar は vm_id を "vm-1" から振り直すため）。
    uid: String,
}

impl WasmInstance {
    pub(crate) fn new(
        transport: Arc<SidecarTransport>,
        connection_id: String,
        session_id: String,
        vm_id: String,
    ) -> Self {
        WasmInstance {
            transport,
            connection_id,
            session_id,
            vm_id,
            uid: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn scope(&self) -> wire::OwnershipScope {
        vm_scope(&self.connection_id, &self.session_id, &self.vm_id)
    }

    /// GuestFilesystemCall を 1 回発行する。
    async fn fs_call(
        &self,
        op: wire::GuestFilesystemOperation,
        path: &str,
        content: Option<String>,
        encoding: Option<wire::RootFilesystemEntryEncoding>,
    ) -> Result<wire::GuestFilesystemResultResponse, SandboxError> {
        let req = wire::GuestFilesystemCallRequest {
            operation: op,
            path: path.to_string(),
            destination_path: None,
            target: None,
            content,
            encoding,
            recursive: false,
            max_depth: None,
            mode: None,
            uid: None,
            gid: None,
            atime_ms: None,
            mtime_ms: None,
            len: None,
            offset: None,
        };
        let resp = request(
            &self.transport,
            self.scope(),
            wire::RequestPayload::GuestFilesystemCallRequest(req),
        )
        .await?;
        match resp {
            wire::ResponsePayload::GuestFilesystemResultResponse(r) => Ok(r),
            wire::ResponsePayload::RejectedResponse(r) => {
                Err(SandboxError::Invalid(format!("filesystem rejected: {r:?}")))
            }
            other => Err(SandboxError::Internal(format!(
                "unexpected fs response: {other:?}"
            ))),
        }
    }
}

/// ExecuteRequest を組み立てる（Python は entrypoint、Shell は sh -c command）。
fn build_execute(process_id: String, req: &ExecRequest) -> wire::ExecuteRequest {
    match req {
        ExecRequest::Python { .. } => {
            // Pyodide 同梱 wheel（numpy/pandas）を import 時に利用可能にする。
            let mut env = std::collections::HashMap::new();
            env.insert(
                "AGENTOS_PYTHON_PRELOAD_PACKAGES".to_string(),
                r#"["numpy","pandas"]"#.to_string(),
            );
            wire::ExecuteRequest {
                process_id,
                command: None,
                runtime: Some(wire::GuestRuntimeKind::Python),
                entrypoint: Some(ENTRYPOINT.to_string()),
                args: Vec::new(),
                env,
                cwd: None,
                wasm_permission_tier: None,
            }
        }
        // sidecar の command はシェル行ではなく「PATH 解決される単一コマンド名」。
        // シェル行は software（coreutils）が投影する `sh` に -c で渡す。
        ExecRequest::Shell { cmd, .. } => wire::ExecuteRequest {
            process_id,
            command: Some("sh".to_string()),
            runtime: None,
            entrypoint: None,
            args: vec!["-c".to_string(), cmd.clone()],
            env: std::collections::HashMap::new(),
            cwd: None,
            wasm_permission_tier: None,
        },
    }
}

#[async_trait]
impl super::super::Instance for WasmInstance {
    fn debug_id(&self) -> String {
        format!("vm:{}:{}", self.vm_id, self.uid)
    }

    async fn exec(
        &self,
        req: ExecRequest,
    ) -> Result<BoxStream<'static, Result<ExecEvent, SandboxError>>, SandboxError> {
        // Python はコードを /workspace/main.py に書いてから実行。
        if let ExecRequest::Python { code, .. } = &req {
            self.fs_call(
                wire::GuestFilesystemOperation::WriteFile,
                ENTRYPOINT,
                Some(code.clone()),
                Some(wire::RootFilesystemEntryEncoding::Utf8),
            )
            .await?;
        }

        let process_id = uuid::Uuid::new_v4().to_string();
        // イベント購読は execute より前に開始する（取りこぼし防止）。
        let mut events = self.transport.subscribe_wire_events();

        let started = request(
            &self.transport,
            self.scope(),
            wire::RequestPayload::ExecuteRequest(build_execute(process_id.clone(), &req)),
        )
        .await?;
        match started {
            wire::ResponsePayload::ProcessStartedResponse(_) => {}
            wire::ResponsePayload::RejectedResponse(r) => {
                return Err(SandboxError::Invalid(format!("execute rejected: {r:?}")))
            }
            other => {
                return Err(SandboxError::Internal(format!(
                    "unexpected execute response: {other:?}"
                )))
            }
        }

        // process_id に一致するイベントを ExecEvent に写像し mpsc へ流す。
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ExecEvent, SandboxError>>(64);
        let want = process_id;
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok((_ownership, payload)) => match payload {
                        wire::EventPayload::ProcessOutputEvent(o) if o.process_id == want => {
                            let ev = match o.channel {
                                wire::StreamChannel::Stdout => ExecEvent::Stdout(o.chunk),
                                wire::StreamChannel::Stderr => ExecEvent::Stderr(o.chunk),
                            };
                            if tx.send(Ok(ev)).await.is_err() {
                                break;
                            }
                        }
                        wire::EventPayload::ProcessExitedEvent(e) if e.process_id == want => {
                            let _ = tx.send(Ok(ExecEvent::Exited { code: e.exit_code })).await;
                            break;
                        }
                        _ => {}
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // ラグはドロップして継続（出力は上限で打ち切られる前提）。
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        let _ = tx
                            .send(Err(SandboxError::Unavailable(
                                "sidecar event stream closed".into(),
                            )))
                            .await;
                        break;
                    }
                }
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    async fn put_file(&self, path: &str, bytes: Vec<u8>) -> Result<(), SandboxError> {
        let encoded = B64.encode(&bytes);
        self.fs_call(
            wire::GuestFilesystemOperation::WriteFile,
            path,
            Some(encoded),
            Some(wire::RootFilesystemEntryEncoding::Base64),
        )
        .await?;
        Ok(())
    }

    async fn get_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        let r = self
            .fs_call(
                wire::GuestFilesystemOperation::ReadFile,
                path,
                None,
                Some(wire::RootFilesystemEntryEncoding::Base64),
            )
            .await?;
        let content = r
            .content
            .ok_or_else(|| SandboxError::NotFound(path.to_string()))?;
        // 応答の encoding に従って復号する（UTF-8 テキストはそのまま・バイナリは Base64）。
        match r.encoding {
            Some(wire::RootFilesystemEntryEncoding::Base64) => B64
                .decode(content.as_bytes())
                .map_err(|e| SandboxError::Internal(format!("base64 decode: {e}"))),
            _ => Ok(content.into_bytes()),
        }
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        let r = self
            .fs_call(wire::GuestFilesystemOperation::ReadDir, path, None, None)
            .await?;
        let entries = r
            .entries
            .unwrap_or_default()
            .into_iter()
            .map(|e| DirEntry {
                name: e.name,
                is_dir: e.is_directory,
                size: e.size,
            })
            .collect();
        Ok(entries)
    }

    async fn destroy(&self) -> Result<(), SandboxError> {
        let _ = request(
            &self.transport,
            self.scope(),
            wire::RequestPayload::DisposeVmRequest(wire::DisposeVmRequest {
                reason: wire::DisposeReason::Requested,
            }),
        )
        .await;
        // 子プロセスを確実に落とす（wedge 対策）。
        self.transport.kill_child();
        Ok(())
    }
}
