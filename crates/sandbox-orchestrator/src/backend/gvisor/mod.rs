//! gVisor（runsc）バックエンド。フル Linux（KVM 不要）ティア。
//!
//! create ごとに OCI バンドルを作り `runsc run`（init=sleep infinity）で常駐させる。exec は `runsc exec`、
//! ファイルは host bind の `/workspace`、破棄は kill＋delete。egress 有効時は holder の netns に入り
//! `--network=host` で回す（ゲストは netns 内 SNI プロキシ経由でのみ外へ出られる・PIT-25）。
//!
//! 実行時アセット取得は行わない（PIT-33）。runsc/rootfs はイメージ同梱・パス設定で渡す。

mod bundle;
mod instance;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sandbox_client::{SandboxBackend, SandboxError, SandboxSpec};

use super::egress::{has_egress, EgressAudit, EgressStack};
use super::native::{is_executable, workspace::Workspace};
use super::{Backend, Instance};
use instance::{runsc_base, GvisorInstance, RunscConfig};

/// gVisor バックエンドの構成。
#[derive(Debug, Clone)]
pub struct GvisorBackend {
    runsc: Arc<RunscConfig>,
    rootfs_dir: PathBuf,
    state_root: PathBuf,
    /// egress netns holder バイナリ（未設定なら egress 要求時に fail）。
    holder_bin: Option<PathBuf>,
}

impl GvisorBackend {
    /// 設定を検証してバックエンドを構築する。runsc/rootfs が無ければ `None` 相当のエラー。
    pub fn new(
        runsc_bin: &str,
        rootfs_dir: PathBuf,
        state_root: PathBuf,
        holder_bin: Option<PathBuf>,
    ) -> Result<Self, SandboxError> {
        if !is_executable(Path::new(runsc_bin)) {
            return Err(SandboxError::Unavailable(format!(
                "runsc binary not found/executable: {runsc_bin}"
            )));
        }
        if !rootfs_dir.is_dir() {
            return Err(SandboxError::Unavailable(format!(
                "gvisor rootfs dir not found: {}",
                rootfs_dir.display()
            )));
        }
        std::fs::create_dir_all(&state_root)
            .map_err(|e| SandboxError::Unavailable(format!("gvisor state dir: {e}")))?;
        // 前回クラッシュの残骸を掃除（best-effort・kill_on_drop が主・PIT: 残留ゼロ）。
        Self::sweep_orphans(&state_root);
        Ok(GvisorBackend {
            runsc: Arc::new(RunscConfig {
                bin: runsc_bin.to_string(),
                platform: "systrap".to_string(),
            }),
            rootfs_dir,
            state_root,
            holder_bin,
        })
    }

    /// 起動時の孤児掃除。残った per-sandbox ディレクトリを runsc delete して除去する。
    fn sweep_orphans(state_root: &Path) {
        let Ok(entries) = std::fs::read_dir(state_root) else {
            return;
        };
        for ent in entries.flatten() {
            let dir = ent.path();
            let root = dir.join("runsc");
            if root.is_dir() {
                if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                    let _ = std::process::Command::new("runsc")
                        .arg("--root")
                        .arg(&root)
                        .args(["--rootless", "delete", "--force", name])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// `runsc state <id>` を最大 `attempts` 回ポーリングして running を待つ。
    async fn wait_running(
        runsc: &RunscConfig,
        root_dir: &Path,
        netns_pid: Option<u32>,
        network: &str,
        id: &str,
    ) -> Result<(), SandboxError> {
        for _ in 0..50 {
            let out = runsc_base(runsc, root_dir, netns_pid, network)
                .arg("state")
                .arg(id)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .await;
            if let Ok(o) = out {
                if let Ok(text) = String::from_utf8(o.stdout) {
                    if text.contains("\"status\": \"running\"")
                        || text.contains("\"status\":\"running\"")
                    {
                        return Ok(());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(SandboxError::Unavailable(
            "gvisor container did not reach running state".into(),
        ))
    }
}

#[async_trait]
impl Backend for GvisorBackend {
    async fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Instance>, SandboxError> {
        if spec.backend != SandboxBackend::Gvisor {
            return Err(SandboxError::Invalid("spec.backend != gvisor".into()));
        }
        let id = format!("gv-{}", uuid::Uuid::new_v4());
        let state_dir = self.state_root.join(&id);
        let root_dir = state_dir.join("runsc");
        let exec_dir = state_dir.join("exec");
        for d in [&state_dir, &root_dir, &exec_dir] {
            tokio::fs::create_dir_all(d)
                .await
                .map_err(|e| SandboxError::Internal(format!("mkdir {}: {e}", d.display())))?;
        }
        let workspace = Workspace::create(&state_dir.join("workspace"))?;

        // egress 有効なら holder の netns を立ち上げ、resolv.conf を用意する。
        let (egress, resolv_conf) = if has_egress(&spec.egress) {
            let holder = self.holder_bin.as_ref().ok_or_else(|| {
                SandboxError::Unimplemented(
                    "egress requested but SANDBOX__NETNS_HOLDER_BIN unset".into(),
                )
            })?;
            let audit = EgressAudit {
                tenant_id: spec.tenant_id.clone(),
                sandbox_id: id.clone(),
            };
            let stack =
                EgressStack::start(&spec.egress, audit, holder, &state_dir.join("egress")).await?;
            let resolv = state_dir.join("resolv.conf");
            tokio::fs::write(&resolv, format!("nameserver {}\n", stack.gateway()))
                .await
                .map_err(|e| SandboxError::Internal(format!("write resolv.conf: {e}")))?;
            (Some(stack), Some(resolv))
        } else {
            (None, None)
        };

        // config.json を書き出す。
        let config = bundle::build_config(
            &spec.limits,
            &self.rootfs_dir,
            workspace.host_root(),
            &exec_dir,
            resolv_conf.as_deref(),
        );
        let config_path = state_dir.join("config.json");
        tokio::fs::write(
            &config_path,
            serde_json::to_vec_pretty(&config)
                .map_err(|e| SandboxError::Internal(format!("serialize config: {e}")))?,
        )
        .await
        .map_err(|e| SandboxError::Internal(format!("write config.json: {e}")))?;

        let netns_pid = egress.as_ref().map(EgressStack::netns_pid);
        let network = if egress.is_some() { "host" } else { "none" };

        // `runsc run` を常駐子として spawn（init=sleep infinity・kill_on_drop）。
        let run_child = runsc_base(&self.runsc, &root_dir, netns_pid, network)
            .arg("run")
            .arg("--bundle")
            .arg(&state_dir)
            .arg(&id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SandboxError::Unavailable(format!("runsc run spawn: {e}")))?;

        // running を待つ（失敗時は run_child を drop で kill）。
        Self::wait_running(&self.runsc, &root_dir, netns_pid, network, &id).await?;

        Ok(Arc::new(GvisorInstance::new(
            Arc::clone(&self.runsc),
            root_dir,
            id,
            workspace,
            exec_dir,
            egress,
            run_child,
            state_dir,
            &spec.limits,
        )))
    }
}
