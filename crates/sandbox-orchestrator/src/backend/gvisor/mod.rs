//! gVisor（runsc）バックエンド。フル Linux（KVM 不要）ティア。
//!
//! create ごとに OCI バンドルを作り `runsc run`（init=sleep infinity）で常駐させる。exec は `runsc exec`、
//! ファイルは host bind の `/workspace`、破棄は kill＋delete。egress 有効時は holder の netns に入り
//! `--network=host` で回す（ゲストは netns 内 SNI プロキシ経由でのみ外へ出られる・PIT-25）。
//!
//! 実行時アセット取得は行わない（PIT-33）。runsc/rootfs はイメージ同梱・パス設定で渡す。

mod bundle;
mod instance;
mod watchdog;

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
    /// メモリ watchdog の監視間隔（None で無効・#346。`--total-memory` と併せた二重防御）。
    watchdog_interval: Option<Duration>,
}

impl GvisorBackend {
    /// 設定を検証してバックエンドを構築する。runsc/rootfs が無ければ `None` 相当のエラー。
    pub fn new(
        runsc_bin: &str,
        rootfs_dir: PathBuf,
        state_root: PathBuf,
        holder_bin: Option<PathBuf>,
        watchdog_interval: Option<Duration>,
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
        Self::sweep_orphans(runsc_bin, &state_root);
        Ok(GvisorBackend {
            runsc: Arc::new(RunscConfig {
                bin: runsc_bin.to_string(),
                platform: "systrap".to_string(),
            }),
            rootfs_dir,
            state_root,
            holder_bin,
            watchdog_interval,
        })
    }

    /// 起動時の孤児掃除。**自分が作った `gv-*` ディレクトリのみ**を対象に、設定済み `runsc_bin` で
    /// delete してから除去する（`state_root` が広いディレクトリを誤指定しても無関係データを消さない）。
    fn sweep_orphans(runsc_bin: &str, state_root: &Path) {
        let Ok(entries) = std::fs::read_dir(state_root) else {
            return;
        };
        for ent in entries.flatten() {
            let dir = ent.path();
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // 自分の命名規則（create の `gv-<uuid>`）以外は触らない。
            if !name.starts_with("gv-") {
                continue;
            }
            let root = dir.join("runsc");
            if root.is_dir() {
                let _ = std::process::Command::new(runsc_bin)
                    .arg("--root")
                    .arg(&root)
                    .args(["--rootless", "delete", "--force", name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
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
            // 各 `runsc state` にもタイムアウトを敷く（hang した runsc で create 全体が詰まらないように）。
            // kill_on_drop: タイムアウトで future を drop したら hang した子も確実に kill/reap する。
            let fut = runsc_base(runsc, root_dir, netns_pid, network)
                .arg("state")
                .arg(id)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .output();
            if let Ok(Ok(o)) = tokio::time::timeout(Duration::from_secs(3), fut).await {
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
        // メモリ上限は OCI spec の `linux.resources.memory.limit`（bundle.rs・ゲスト可視の
        // ソフト上限。現行 runsc に旧 `--total-memory` フラグは無い）＋ orchestrator 側
        // watchdog（超過 kill）の二重防御（#346・cgroups 無し環境ではソフト強制・PIT-24）。
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

        // メモリ watchdog（超過 kill・destroy 時に abort・#346）。
        let watchdog = match (self.watchdog_interval, spec.limits.memory_mb) {
            (Some(interval), mb) if mb > 0 => Some(watchdog::spawn(
                Arc::clone(&self.runsc),
                root_dir.clone(),
                id.clone(),
                spec.tenant_id.clone(),
                mb.saturating_mul(1024 * 1024),
                interval,
            )),
            _ => None,
        };

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
            watchdog,
        )))
    }
}
