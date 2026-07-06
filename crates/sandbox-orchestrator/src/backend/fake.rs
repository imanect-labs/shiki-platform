//! `FakeBackend` — server ロジック（validate・registry・出力上限・イベント写像）を実 sidecar 無しで
//! 決定的にテストするためのインメモリバックエンド。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use sandbox_client::{DirEntry, ExecEvent, ExecRequest, SandboxError, SandboxSpec};

use super::{Backend, Instance};

/// exec 応答スクリプト。
#[derive(Clone, Default)]
pub struct FakeExec {
    pub events: Vec<ExecEvent>,
    /// exec 後に見える成果物（path→bytes）。
    pub artifacts: Vec<(String, Vec<u8>)>,
}

#[derive(Default)]
pub struct FakeBackend {
    next_id: AtomicU64,
    exec_script: Mutex<Vec<FakeExec>>,
    fail_create: bool,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_exec(self, exec: FakeExec) -> Self {
        self.exec_script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(exec);
        self
    }

    #[must_use]
    pub fn failing(mut self) -> Self {
        self.fail_create = true;
        self
    }
}

#[async_trait]
impl Backend for FakeBackend {
    async fn create(&self, _spec: SandboxSpec) -> Result<Arc<dyn Instance>, SandboxError> {
        if self.fail_create {
            return Err(SandboxError::Unavailable("fake backend down".into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let script = std::mem::take(
            &mut *self
                .exec_script
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Ok(Arc::new(FakeInstance {
            pid: 10_000 + id,
            exec_script: Mutex::new(script),
            files: Mutex::new(Vec::new()),
            destroyed: AtomicU64::new(0),
        }))
    }
}

pub struct FakeInstance {
    pid: u64,
    exec_script: Mutex<Vec<FakeExec>>,
    files: Mutex<Vec<(String, Vec<u8>)>>,
    destroyed: AtomicU64,
}

impl FakeInstance {
    /// destroy 済みか（残留無しの検証用）。
    pub fn is_destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst) > 0
    }
}

#[async_trait]
impl Instance for FakeInstance {
    fn debug_id(&self) -> String {
        format!("fake-pid-{}", self.pid)
    }

    async fn exec(
        &self,
        _req: ExecRequest,
    ) -> Result<BoxStream<'static, Result<ExecEvent, SandboxError>>, SandboxError> {
        let step = {
            let mut q = self
                .exec_script
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if q.is_empty() {
                FakeExec {
                    events: vec![ExecEvent::Exited { code: 0 }],
                    artifacts: Vec::new(),
                }
            } else {
                q.remove(0)
            }
        };
        {
            let mut files = self
                .files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (p, b) in step.artifacts {
                files.push((p, b));
            }
        }
        let events: Vec<Result<ExecEvent, SandboxError>> =
            step.events.into_iter().map(Ok).collect();
        Ok(Box::pin(stream::iter(events)))
    }

    async fn put_file(&self, path: &str, bytes: Vec<u8>) -> Result<(), SandboxError> {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((path.to_string(), bytes));
        Ok(())
    }

    async fn get_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| SandboxError::NotFound(path.to_string()))
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        let files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = files
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, b)| DirEntry {
                name: p
                    .trim_start_matches(path)
                    .trim_start_matches('/')
                    .to_string(),
                is_dir: false,
                size: b.len() as u64,
            })
            .filter(|e| !e.name.is_empty())
            .collect();
        Ok(entries)
    }

    async fn destroy(&self) -> Result<(), SandboxError> {
        self.destroyed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
