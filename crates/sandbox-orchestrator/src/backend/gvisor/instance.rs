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
    /// egress 有効時のみ Some（holder の netns へ入る）。`destroy()` で明示的に take/drop して
    /// netns/プロキシを即時解放する（Arc の drop 待ちにしない）。std Mutex は await を跨がず短時間だけ持つ。
    egress: std::sync::Mutex<Option<EgressStack>>,
    /// `runsc run` の常駐子プロセス（kill_on_drop）。`destroy()` で take/kill する。
    run_child: std::sync::Mutex<Option<Child>>,
    state_dir: PathBuf,
    exec_timeout: Duration,
    wall_clock: Duration,
    max_output: usize,
    seq: AtomicU64,
    /// メモリ watchdog タスク（#346・destroy で abort）。
    watchdog: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
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
        watchdog: Option<tokio::task::JoinHandle<()>>,
    ) -> Self {
        GvisorInstance {
            runsc,
            root_dir,
            id,
            workspace,
            exec_dir,
            egress: std::sync::Mutex::new(egress),
            run_child: std::sync::Mutex::new(Some(run_child)),
            state_dir,
            exec_timeout: Duration::from_millis(limits.exec_timeout_ms.max(1)),
            wall_clock: Duration::from_millis(limits.wall_clock_ms.max(1)),
            max_output: usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
            seq: AtomicU64::new(0),
            watchdog: std::sync::Mutex::new(watchdog),
        }
    }

    fn netns_pid(&self) -> Option<u32> {
        self.egress
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(EgressStack::netns_pid))
    }

    /// runsc グローバルフラグを載せた `Command`（サブコマンドは呼び出し側が付ける）。
    fn base(&self, network: &str) -> Command {
        runsc_base(&self.runsc, &self.root_dir, self.netns_pid(), network)
    }

    fn network_mode(&self) -> &'static str {
        let has = self.egress.lock().is_ok_and(|g| g.is_some());
        if has {
            "host"
        } else {
            "none"
        }
    }

    /// この exec の in-guest 上限秒（`timeout(1)` 用）。
    ///
    /// リクエストの `timeout_ms` を尊重するが、**exec 上限と壁時計の両方**で頭打ちにする
    /// （リクエストが exec 上限を超えて実行時間を引き延ばせないように）。ミリ秒は切り上げ・最低 1 秒。
    fn timeout_secs(&self, req_timeout_ms: Option<u64>) -> u64 {
        let wall_ms = u64::try_from(self.wall_clock.as_millis()).unwrap_or(u64::MAX);
        let exec_ms = u64::try_from(self.exec_timeout.as_millis()).unwrap_or(u64::MAX);
        let cap = exec_ms.min(wall_ms);
        let ms = req_timeout_ms.unwrap_or(cap).min(cap).max(1);
        ms.div_ceil(1000).max(1)
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
            ExecRequest::Python { code, timeout_ms } => {
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
                    .arg(self.timeout_secs(*timeout_ms).to_string())
                    .arg("python3")
                    .arg(guest_path);
            }
            ExecRequest::Shell {
                cmd: shell,
                timeout_ms,
            } => {
                let parts = shlex::split(shell)
                    .ok_or_else(|| SandboxError::Invalid("unparseable shell command".into()))?;
                if parts.is_empty() {
                    return Err(SandboxError::Invalid("empty shell command".into()));
                }
                cmd.arg(&self.id)
                    .arg("timeout")
                    .arg("-k")
                    .arg("2")
                    .arg(self.timeout_secs(*timeout_ms).to_string());
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
        // watchdog を先に止める（kill 済みコンテナへの空監視・二重 kill を避ける）。
        if let Ok(mut w) = self.watchdog.lock() {
            if let Some(handle) = w.take() {
                handle.abort();
            }
        }
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
        // runsc を落とし終えたので、egress（netns/プロキシ）と常駐 run_child を **即時**解放する
        // （Arc の drop 待ちにしない）。egress は kill/delete が nsenter で参照するため、この順序で。
        if let Ok(mut g) = self.egress.lock() {
            drop(g.take()); // EgressStack::Drop → タスク abort ＋ Netns::Drop で holder kill。
        }
        if let Ok(mut c) = self.run_child.lock() {
            if let Some(mut child) = c.take() {
                let _ = child.start_kill(); // kill_on_drop でも落ちるが即時性のため明示 kill。
            }
        }
        // 状態ディレクトリを掃除（tmpfs 想定・残ってもリークにはならない）。
        let _ = tokio::fs::remove_dir_all(&self.state_dir).await;
        Ok(())
    }
}
