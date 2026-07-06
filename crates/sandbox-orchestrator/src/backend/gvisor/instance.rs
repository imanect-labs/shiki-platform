//! gVisor インスタンス: 1 コンテナへの exec / ファイル操作 / 破棄。
//!
//! `runsc run`（バックグラウンド・init=sleep infinity）で常駐させ、コマンドごとに `runsc exec` する。
//! ファイルは host bind の `/workspace` を直接操作する。egress 時は `nsenter -U -n` で holder の netns に
//! 入って `runsc --network=host` を回す（ゲストは netns 内プロキシ経由でのみ外へ出られる）。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use sandbox_client::{DirEntry, ExecEvent, ExecRequest, SandboxError};
use tokio::process::{Child, Command};

use crate::backend::egress::EgressStack;
use crate::backend::native::workspace::Workspace;
use crate::backend::native::{nsenter_command, stream::stream_child};
use crate::backend::Instance;

/// runsc 実行の共有設定（バイナリ・プラットフォーム）。
#[derive(Debug, Clone)]
pub(super) struct RunscConfig {
    pub bin: String,
    pub platform: String,
}

/// runsc グローバルフラグを載せた `Command` を組む（サブコマンドは呼び出し側が付ける）。
///
/// `netns_pid` が Some なら `nsenter -U -n` で holder の netns に入って実行する（egress 経路）。
pub(super) fn runsc_base(
    runsc: &RunscConfig,
    root_dir: &std::path::Path,
    netns_pid: Option<u32>,
    network: &str,
) -> Command {
    let mut cmd = match netns_pid {
        Some(pid) => nsenter_command(pid, &runsc.bin),
        None => Command::new(&runsc.bin),
    };
    cmd.arg("--root")
        .arg(root_dir)
        .arg("--rootless")
        .arg(format!("--network={network}"))
        .arg(format!("--platform={}", runsc.platform))
        .arg("--overlay2=root:memory")
        .arg("--ignore-cgroups");
    cmd
}

/// 生成済み gVisor コンテナ 1 個。
pub(super) struct GvisorInstance {
    runsc: Arc<RunscConfig>,
    /// per-sandbox の runsc 状態ディレクトリ（`--root`）。
    root_dir: PathBuf,
    id: String,
    workspace: Workspace,
    /// 注入スクリプト（`/__exec`・RO bind）のホスト側ディレクトリ。
    exec_dir: PathBuf,
    /// egress 有効時のみ Some（holder の netns へ入る）。drop で netns/プロキシを畳む。
    egress: Option<EgressStack>,
    /// `runsc run` の常駐子プロセス（kill_on_drop）。
    _run_child: Child,
    state_dir: PathBuf,
    exec_timeout: Duration,
    wall_clock: Duration,
    max_output: usize,
    seq: AtomicU64,
}

impl GvisorInstance {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        runsc: Arc<RunscConfig>,
        root_dir: PathBuf,
        id: String,
        workspace: Workspace,
        exec_dir: PathBuf,
        egress: Option<EgressStack>,
        run_child: Child,
        state_dir: PathBuf,
        limits: &sandbox_client::SandboxLimits,
    ) -> Self {
        GvisorInstance {
            runsc,
            root_dir,
            id,
            workspace,
            exec_dir,
            egress,
            _run_child: run_child,
            state_dir,
            exec_timeout: Duration::from_millis(limits.exec_timeout_ms.max(1)),
            wall_clock: Duration::from_millis(limits.wall_clock_ms.max(1)),
            max_output: usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
            seq: AtomicU64::new(0),
        }
    }

    fn netns_pid(&self) -> Option<u32> {
        self.egress.as_ref().map(EgressStack::netns_pid)
    }

    /// runsc グローバルフラグを載せた `Command`（サブコマンドは呼び出し側が付ける）。
    fn base(&self, network: &str) -> Command {
        runsc_base(&self.runsc, &self.root_dir, self.netns_pid(), network)
    }

    fn network_mode(&self) -> &'static str {
        if self.egress.is_some() {
            "host"
        } else {
            "none"
        }
    }

    /// in-guest の壁時計上限（`timeout(1)` 用秒）。最低 1 秒。
    fn timeout_secs(&self) -> u64 {
        self.exec_timeout.as_secs().max(1)
    }
}

#[async_trait]
impl Instance for GvisorInstance {
    fn debug_id(&self) -> String {
        format!("gvisor:{}", self.id)
    }

    async fn exec(
        &self,
        req: ExecRequest,
    ) -> Result<BoxStream<'static, Result<ExecEvent, SandboxError>>, SandboxError> {
        let net = self.network_mode();
        let mut cmd = self.base(net);
        cmd.arg("exec").arg("--cwd").arg("/workspace");

        match &req {
            ExecRequest::Python { code, .. } => {
                let seq = self.seq.fetch_add(1, Ordering::SeqCst);
                let script = self.exec_dir.join(format!("main-{seq}.py"));
                tokio::fs::write(&script, code.as_bytes())
                    .await
                    .map_err(|e| SandboxError::Internal(format!("write exec script: {e}")))?;
                let guest_path = format!("/__exec/main-{seq}.py");
                cmd.arg(&self.id)
                    .arg("timeout")
                    .arg("-k")
                    .arg("2")
                    .arg(self.timeout_secs().to_string())
                    .arg("python3")
                    .arg(guest_path);
            }
            ExecRequest::Shell { cmd: shell, .. } => {
                let parts = shlex::split(shell)
                    .ok_or_else(|| SandboxError::Invalid("unparseable shell command".into()))?;
                if parts.is_empty() {
                    return Err(SandboxError::Invalid("empty shell command".into()));
                }
                cmd.arg(&self.id)
                    .arg("timeout")
                    .arg("-k")
                    .arg("2")
                    .arg(self.timeout_secs().to_string());
                for p in parts {
                    cmd.arg(p);
                }
            }
        }

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|e| SandboxError::Unavailable(format!("runsc exec spawn: {e}")))?;
        // 壁時計は in-guest timeout(1) が一次。orchestrator 側 stream にも二重で上限を敷く。
        Ok(stream_child(child, self.max_output, self.wall_clock))
    }

    async fn put_file(&self, path: &str, bytes: Vec<u8>) -> Result<(), SandboxError> {
        self.workspace.put(path, bytes).await
    }

    async fn get_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        self.workspace.get(path).await
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        self.workspace.list(path).await
    }

    async fn destroy(&self) -> Result<(), SandboxError> {
        // kill→delete（冪等・失敗は無視）。
        let _ = self
            .base(self.network_mode())
            .arg("kill")
            .arg(&self.id)
            .arg("KILL")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        let _ = self
            .base(self.network_mode())
            .arg("delete")
            .arg("--force")
            .arg(&self.id)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        // egress スタック（netns/プロキシ）と run_child は self の Drop で畳まれる
        // （EgressStack::Drop→タスク abort＋Netns::Drop で holder kill・run_child は kill_on_drop）。
        // destroy 中は netns を生かしておく必要があるため、ここでは触らない。
        // 状態ディレクトリを掃除（tmpfs 想定・残ってもリークにはならない）。
        let _ = tokio::fs::remove_dir_all(&self.state_dir).await;
        Ok(())
    }
}
