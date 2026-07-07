//! Firecracker インスタンス: vsock 経由でゲストエージェントに要求を送り、exec/ファイル/破棄を担う。
//!
//! プロトコルは 1 接続・逐次。exec は出力を集めてから ExecEvent 列として返す（server 側で出力上限を
//! 二重に強制する）。破棄は Shutdown→猶予→firecracker 子を SIGKILL（/dev/kvm・tap を即解放）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use futures::stream::{self, BoxStream};
use sandbox_client::{DirEntry, ExecEvent, ExecRequest, SandboxError};
use shiki_sandbox_agent_proto::{Event, Request};
use tokio::process::Child;
use tokio::sync::Mutex;

use super::vsock::AgentConn;
use crate::backend::egress::EgressStack;
use crate::backend::Instance;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// 生成済み microVM 1 個。
pub(super) struct FirecrackerInstance {
    id: String,
    conn: Mutex<AgentConn>,
    /// firecracker 子プロセス（kill_on_drop）。`destroy()` で take/kill して /dev/kvm・tap を即解放する。
    fc_child: std::sync::Mutex<Option<Child>>,
    _egress: Option<EgressStack>,
    state_dir: PathBuf,
    exec_timeout: Duration,
    seq: AtomicU64,
}

impl FirecrackerInstance {
    pub(super) fn new(
        id: String,
        conn: AgentConn,
        fc_child: Child,
        egress: Option<EgressStack>,
        state_dir: PathBuf,
        limits: &sandbox_client::SandboxLimits,
    ) -> Self {
        FirecrackerInstance {
            id,
            conn: Mutex::new(conn),
            fc_child: std::sync::Mutex::new(Some(fc_child)),
            _egress: egress,
            state_dir,
            exec_timeout: Duration::from_millis(limits.exec_timeout_ms.max(1)),
            seq: AtomicU64::new(0),
        }
    }

    /// 1 要求を送り、単一の応答イベントを受ける（ファイル系）。
    async fn request_one(&self, req: Request) -> Result<Event, SandboxError> {
        let mut conn = self.conn.lock().await;
        conn.send(&req).await?;
        conn.recv()
            .await?
            .ok_or_else(|| SandboxError::Unavailable("agent closed".into()))
    }
}

#[async_trait]
impl Instance for FirecrackerInstance {
    fn debug_id(&self) -> String {
        format!("firecracker:{}", self.id)
    }

    async fn exec(
        &self,
        req: ExecRequest,
    ) -> Result<BoxStream<'static, Result<ExecEvent, SandboxError>>, SandboxError> {
        let timeout_ms = self.exec_timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        let mut conn = self.conn.lock().await;

        // Python はコードをファイルに書いてから python3 で実行する。
        let argv = match &req {
            ExecRequest::Python { code, .. } => {
                let seq = self.seq.fetch_add(1, Ordering::SeqCst);
                let path = format!("/workspace/.exec/main-{seq}.py");
                conn.send(&Request::WriteFile {
                    path: path.clone(),
                    b64: B64.encode(code.as_bytes()),
                })
                .await?;
                match conn.recv().await? {
                    Some(Event::Ok) => {}
                    other => {
                        return Err(SandboxError::Internal(format!(
                            "write exec script failed: {other:?}"
                        )))
                    }
                }
                vec!["python3".to_string(), path]
            }
            ExecRequest::Shell { cmd, .. } => shlex::split(cmd)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| SandboxError::Invalid("unparseable shell command".into()))?,
        };

        conn.send(&Request::Exec { argv, timeout_ms }).await?;

        // Exited まで集めて ExecEvent 列にする（server 側で出力上限を強制）。
        let mut events: Vec<Result<ExecEvent, SandboxError>> = Vec::new();
        loop {
            match conn.recv().await? {
                Some(Event::Stdout { b64 }) => {
                    events.push(Ok(ExecEvent::Stdout(decode(&b64))));
                }
                Some(Event::Stderr { b64 }) => {
                    events.push(Ok(ExecEvent::Stderr(decode(&b64))));
                }
                Some(Event::Exited { code }) => {
                    events.push(Ok(ExecEvent::Exited { code }));
                    break;
                }
                Some(Event::Err { msg }) => {
                    events.push(Err(SandboxError::Internal(msg)));
                    break;
                }
                Some(other) => {
                    return Err(SandboxError::Internal(format!(
                        "unexpected exec event: {other:?}"
                    )))
                }
                None => {
                    events.push(Err(SandboxError::Unavailable("agent closed".into())));
                    break;
                }
            }
        }
        Ok(Box::pin(stream::iter(events)))
    }

    async fn put_file(&self, path: &str, bytes: Vec<u8>) -> Result<(), SandboxError> {
        match self
            .request_one(Request::WriteFile {
                path: path.to_string(),
                b64: B64.encode(&bytes),
            })
            .await?
        {
            Event::Ok => Ok(()),
            Event::Err { msg } => Err(SandboxError::Invalid(msg)),
            other => Err(SandboxError::Internal(format!("put_file: {other:?}"))),
        }
    }

    async fn get_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        match self
            .request_one(Request::ReadFile {
                path: path.to_string(),
            })
            .await?
        {
            Event::File { b64 } => Ok(decode(&b64)),
            Event::Err { .. } => Err(SandboxError::NotFound(path.to_string())),
            other => Err(SandboxError::Internal(format!("get_file: {other:?}"))),
        }
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        match self
            .request_one(Request::ListDir {
                path: path.to_string(),
            })
            .await?
        {
            Event::Dir { entries } => Ok(entries
                .into_iter()
                .map(|e| DirEntry {
                    name: e.name,
                    is_dir: e.is_dir,
                    size: e.size,
                })
                .collect()),
            Event::Err { .. } => Err(SandboxError::NotFound(path.to_string())),
            other => Err(SandboxError::Internal(format!("list_dir: {other:?}"))),
        }
    }

    async fn destroy(&self) -> Result<(), SandboxError> {
        // 電源オフ要求→猶予。
        {
            let mut conn = self.conn.lock().await;
            let _ = conn.send(&Request::Shutdown).await;
            let _ = tokio::time::timeout(Duration::from_millis(500), conn.recv()).await;
        }
        // firecracker 子を**即時** kill して /dev/kvm・tap を解放する（Drop 待ちにしない）。
        if let Ok(mut c) = self.fc_child.lock() {
            if let Some(mut child) = c.take() {
                let _ = child.start_kill();
            }
        }
        let _ = tokio::fs::remove_dir_all(&self.state_dir).await;
        Ok(())
    }
}

fn decode(b64: &str) -> Vec<u8> {
    B64.decode(b64.as_bytes()).unwrap_or_default()
}
