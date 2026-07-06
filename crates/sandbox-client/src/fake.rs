//! `FakeSandbox` — インメモリのサンドボックス実装（agent-core / chat の結合テスト用）。
//!
//! 実 sidecar・V8 を使わず、Python/Shell の実行結果と仮想FSをスクリプト化する。決定的で高速。
//! カバレッジの主力（実 Pyodide は gated IT のみ）。

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};

use crate::error::SandboxError;
use crate::spec::{DirEntry, ExecEvent, ExecRequest, Sandbox, SandboxHandle, SandboxSpec};

/// 1 回の exec に対する擬似応答。
#[derive(Debug, Clone, Default)]
pub struct FakeExecResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    /// exec 後に `/workspace` へ現れる成果物（path→bytes）。
    pub artifacts: Vec<(String, Vec<u8>)>,
}

impl FakeExecResult {
    pub fn stdout(s: impl Into<String>) -> Self {
        FakeExecResult {
            stdout: s.into().into_bytes(),
            ..Default::default()
        }
    }
}

#[derive(Default)]
struct FakeState {
    created: usize,
    destroyed: Vec<String>,
    files: HashMap<String, Vec<u8>>, // "<sandbox_id>:<path>" → bytes
}

/// スクリプト化されたインメモリ Sandbox。`with_exec` で exec 応答を差し込む。
pub struct FakeSandbox {
    exec_results: Mutex<Vec<FakeExecResult>>,
    state: Mutex<FakeState>,
    fail_create: bool,
}

impl Default for FakeSandbox {
    fn default() -> Self {
        FakeSandbox {
            exec_results: Mutex::new(Vec::new()),
            state: Mutex::new(FakeState::default()),
            fail_create: false,
        }
    }
}

impl FakeSandbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// exec 応答をキューに積む（呼ばれた順に消費される）。
    #[must_use]
    pub fn with_exec(self, result: FakeExecResult) -> Self {
        self.exec_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(result);
        self
    }

    /// create が必ず失敗するモード（エラー経路テスト用）。
    #[must_use]
    pub fn failing_create(mut self) -> Self {
        self.fail_create = true;
        self
    }

    /// destroy されたサンドボックス ID 一覧（Drop ガードの検証用）。
    pub fn destroyed(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .destroyed
            .clone()
    }
}

fn key(id: &str, path: &str) -> String {
    format!("{id}:{path}")
}

#[async_trait]
impl Sandbox for FakeSandbox {
    async fn create(&self, _spec: SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        if self.fail_create {
            return Err(SandboxError::Unavailable("fake create failure".into()));
        }
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.created += 1;
        let id = format!("fake-{}", st.created);
        Ok(SandboxHandle { id })
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        _req: ExecRequest,
    ) -> Result<BoxStream<'static, Result<ExecEvent, SandboxError>>, SandboxError> {
        let result = {
            let mut q = self
                .exec_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if q.is_empty() {
                FakeExecResult::default()
            } else {
                q.remove(0)
            }
        };
        // 成果物を仮想FSへ反映（list_dir/get_file で見える）。
        {
            let mut st = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (path, bytes) in &result.artifacts {
                st.files.insert(key(&handle.id, path), bytes.clone());
            }
        }
        let mut events = Vec::new();
        if !result.stdout.is_empty() {
            events.push(Ok(ExecEvent::Stdout(result.stdout)));
        }
        if !result.stderr.is_empty() {
            events.push(Ok(ExecEvent::Stderr(result.stderr)));
        }
        events.push(Ok(ExecEvent::Exited {
            code: result.exit_code,
        }));
        Ok(Box::pin(stream::iter(events)))
    }

    async fn put_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SandboxError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .files
            .insert(key(&handle.id, path), bytes);
        Ok(())
    }

    async fn get_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, SandboxError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .files
            .get(&key(&handle.id, path))
            .cloned()
            .ok_or_else(|| SandboxError::NotFound(path.to_string()))
    }

    async fn list_dir(
        &self,
        handle: &SandboxHandle,
        path: &str,
    ) -> Result<Vec<DirEntry>, SandboxError> {
        let prefix = key(&handle.id, path);
        let st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = st
            .files
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(&prefix).map(|rest| DirEntry {
                    name: rest.trim_start_matches('/').to_string(),
                    is_dir: false,
                    size: v.len() as u64,
                })
            })
            .filter(|e| !e.name.is_empty())
            .collect();
        Ok(entries)
    }

    async fn destroy(&self, handle: &SandboxHandle) -> Result<(), SandboxError> {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.destroyed.push(handle.id.clone());
        Ok(())
    }
}
