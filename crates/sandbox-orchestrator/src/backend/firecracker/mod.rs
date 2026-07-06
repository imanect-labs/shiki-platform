//! Firecracker microVM バックエンド。VM 級隔離（KVM 前提・NFR-1）ティア。
//!
//! create ごとに workspace.ext4 を作り、firecracker を spawn→API で kernel/drives/vsock を構成→
//! InstanceStart→vsock でゲストエージェント（PID1）に接続。exec/ファイルはエージェント経由。
//!
//! アルファは **egress 非対応**（VM 級隔離での SNI プロキシ配線＝tap＋tier 別ゲートウェイは post-alpha。
//! 外部到達が要る用途は wasm/gVisor ティアを使う）。実行時アセット取得は行わない（PIT-33）。

mod api;
mod instance;
mod vsock;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sandbox_client::{SandboxBackend, SandboxError, SandboxSpec};
use serde_json::json;

use super::egress::has_egress;
use super::native::is_executable;
use super::{Backend, Instance};
use instance::FirecrackerInstance;
use vsock::AgentConn;

/// ゲストがリッスンする vsock ポート（guest-agent と一致）。
const AGENT_PORT: u32 = 5000;
/// ゲスト CID（FC 既定の最小値）。
const GUEST_CID: u32 = 3;

/// Firecracker バックエンドの構成。
#[derive(Debug, Clone)]
pub struct FirecrackerBackend {
    fc_bin: String,
    kernel: PathBuf,
    rootfs: PathBuf,
    state_root: PathBuf,
}

impl FirecrackerBackend {
    /// 設定を検証して構築する（fc/kernel/rootfs が揃わなければエラー）。
    pub fn new(
        fc_bin: &str,
        kernel: PathBuf,
        rootfs: PathBuf,
        state_root: PathBuf,
    ) -> Result<Self, SandboxError> {
        if !is_executable(Path::new(fc_bin)) {
            return Err(SandboxError::Unavailable(format!(
                "firecracker binary not found: {fc_bin}"
            )));
        }
        if !kernel.is_file() {
            return Err(SandboxError::Unavailable(format!(
                "kernel image not found: {}",
                kernel.display()
            )));
        }
        if !rootfs.is_file() {
            return Err(SandboxError::Unavailable(format!(
                "rootfs.ext4 not found: {}",
                rootfs.display()
            )));
        }
        std::fs::create_dir_all(&state_root)
            .map_err(|e| SandboxError::Unavailable(format!("fc state dir: {e}")))?;
        Ok(FirecrackerBackend {
            fc_bin: fc_bin.to_string(),
            kernel,
            rootfs,
            state_root,
        })
    }

    /// workspace 用 ext4 を非特権生成する（サイズ＝max_fs_bytes）。
    async fn make_workspace(path: &Path, size_bytes: u64) -> Result<(), SandboxError> {
        let f = tokio::fs::File::create(path)
            .await
            .map_err(|e| SandboxError::Internal(format!("create ws image: {e}")))?;
        f.set_len(size_bytes.max(1024 * 1024))
            .await
            .map_err(|e| SandboxError::Internal(format!("size ws image: {e}")))?;
        drop(f);
        let status = tokio::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|e| SandboxError::Unavailable(format!("mkfs.ext4 spawn: {e}")))?;
        if !status.success() {
            return Err(SandboxError::Internal("mkfs.ext4 failed".into()));
        }
        Ok(())
    }

    /// API ソケットが現れるまで待つ（firecracker の起動待ち）。
    async fn wait_api_sock(sock: &Path) -> Result<(), SandboxError> {
        for _ in 0..100 {
            if sock.exists() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(SandboxError::Unavailable(
            "fc api socket never appeared".into(),
        ))
    }
}

#[async_trait]
impl Backend for FirecrackerBackend {
    async fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Instance>, SandboxError> {
        if spec.backend != SandboxBackend::Firecracker {
            return Err(SandboxError::Invalid("spec.backend != firecracker".into()));
        }
        if has_egress(&spec.egress) {
            return Err(SandboxError::Unimplemented(
                "firecracker egress is post-alpha; use the wasm/gvisor tier for outbound".into(),
            ));
        }

        let id = format!("fc-{}", uuid::Uuid::new_v4());
        let state_dir = self.state_root.join(&id);
        tokio::fs::create_dir_all(&state_dir)
            .await
            .map_err(|e| SandboxError::Internal(format!("mkdir state: {e}")))?;
        let ws_ext4 = state_dir.join("workspace.ext4");
        Self::make_workspace(&ws_ext4, spec.limits.max_fs_bytes).await?;

        let api_sock = state_dir.join("fc.sock");
        let vsock_uds = state_dir.join("vsock.sock");

        // firecracker を spawn（kill_on_drop で /dev/kvm/tap を確実に解放）。
        let fc_child = tokio::process::Command::new(&self.fc_bin)
            .arg("--api-sock")
            .arg(&api_sock)
            .arg("--id")
            .arg(&id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SandboxError::Unavailable(format!("firecracker spawn: {e}")))?;

        Self::wait_api_sock(&api_sock).await?;

        // API 構成（順序が重要）。
        let mem_mib = spec.limits.memory_mb.max(64);
        api::put(
            &api_sock,
            "/machine-config",
            &json!({"vcpu_count":1,"mem_size_mib":mem_mib}),
        )
        .await?;
        api::put(
            &api_sock,
            "/boot-source",
            &json!({
                "kernel_image_path": self.kernel.to_string_lossy(),
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off quiet init=/sbin/sandbox-init"
            }),
        )
        .await?;
        api::put(
            &api_sock,
            "/drives/rootfs",
            &json!({
                "drive_id":"rootfs","path_on_host": self.rootfs.to_string_lossy(),
                "is_root_device":true,"is_read_only":true
            }),
        )
        .await?;
        api::put(
            &api_sock,
            "/drives/workspace",
            &json!({
                "drive_id":"workspace","path_on_host": ws_ext4.to_string_lossy(),
                "is_root_device":false,"is_read_only":false
            }),
        )
        .await?;
        api::put(
            &api_sock,
            "/vsock",
            &json!({"guest_cid": GUEST_CID, "uds_path": vsock_uds.to_string_lossy()}),
        )
        .await?;
        api::put(
            &api_sock,
            "/actions",
            &json!({"action_type":"InstanceStart"}),
        )
        .await?;

        // ゲストエージェントへ接続し Ready を待つ。
        let mut conn = AgentConn::connect(&vsock_uds, AGENT_PORT, Duration::from_secs(10)).await?;
        match conn.recv().await? {
            Some(shiki_sandbox_agent_proto::Event::Ready { .. }) => {}
            other => {
                return Err(SandboxError::Unavailable(format!(
                    "agent did not report ready: {other:?}"
                )))
            }
        }

        Ok(Arc::new(FirecrackerInstance::new(
            id,
            conn,
            fc_child,
            None,
            state_dir,
            &spec.limits,
        )))
    }
}
