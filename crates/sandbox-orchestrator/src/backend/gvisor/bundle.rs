//! OCI ランタイムバンドルの `config.json` 生成（runsc 用・純関数）。
//!
//! init は `sleep infinity`（アンカー）。root は共有 RO rootfs＋`--overlay2=root:memory`（per-sandbox
//! コピー無し）。`/workspace` は host bind（成果物をホスト側で直接回収）、`/__exec` は RO bind
//! （注入スクリプトをゲストが改竄できない）。limits は rlimits に写す（cgroups 無し環境の一次防御）。

use std::path::Path;

use sandbox_client::SandboxLimits;
use serde_json::{json, Value};

/// バンドルの `config.json` を組み立てる。
///
/// - `rootfs`: 共有 RO rootfs のホストパス。
/// - `workspace_host` / `exec_host`: それぞれ `/workspace`(rw) と `/__exec`(ro) にバインドするホストdir。
/// - `resolv_conf`: egress 時に `/etc/resolv.conf` へバインドするホストファイル（None なら bind 無し）。
#[must_use]
pub(super) fn build_config(
    limits: &SandboxLimits,
    rootfs: &Path,
    workspace_host: &Path,
    exec_host: &Path,
    resolv_conf: Option<&Path>,
) -> Value {
    let mut mounts = vec![
        json!({"destination":"/proc","type":"proc","source":"proc"}),
        json!({"destination":"/tmp","type":"tmpfs","source":"tmpfs","options":["nosuid","nodev","noexec"]}),
        json!({"destination":"/dev/shm","type":"tmpfs","source":"tmpfs","options":["nosuid","nodev"]}),
        json!({
            "destination":"/workspace","type":"bind",
            "source": workspace_host.to_string_lossy(),
            "options":["rbind","rw"]
        }),
        json!({
            "destination":"/__exec","type":"bind",
            "source": exec_host.to_string_lossy(),
            "options":["rbind","ro"]
        }),
    ];
    if let Some(rc) = resolv_conf {
        mounts.push(json!({
            "destination":"/etc/resolv.conf","type":"bind",
            "source": rc.to_string_lossy(),
            "options":["rbind","ro"]
        }));
    }

    json!({
        "ociVersion": "1.0.0",
        "process": {
            "terminal": false,
            "user": {"uid": 0, "gid": 0},
            "args": ["sleep", "infinity"],
            "env": [
                "PATH=/usr/local/bin:/usr/bin:/bin",
                "HOME=/root",
                "LANG=C.UTF-8"
            ],
            "cwd": "/workspace",
            "capabilities": {
                "bounding": ["CAP_NET_BIND_SERVICE"],
                "effective": [],
                "inheritable": [],
                "permitted": [],
                "ambient": []
            },
            "rlimits": rlimits(limits),
            "noNewPrivileges": true
        },
        "root": {"path": rootfs.to_string_lossy(), "readonly": true},
        "hostname": "sandbox",
        "mounts": mounts,
        "linux": {
            // メモリ上限（#346）: sentry がゲストへ見せる/管理するメモリのソフト上限。
            // `--ignore-cgroups`（rootless）ではホスト側ハード強制にならないため、
            // orchestrator の watchdog（超過 kill）と併せた二重防御にする（PIT-24）。
            "resources": {"memory": {"limit": limits.memory_mb.saturating_mul(1024 * 1024)}},
            "namespaces": [
                {"type": "pid"},
                {"type": "mount"},
                {"type": "ipc"},
                {"type": "uts"}
            ]
        }
    })
}

/// limits を OCI rlimits に写す（プロセス数・fd 数・ファイルサイズ）。
fn rlimits(limits: &SandboxLimits) -> Value {
    json!([
        {"type":"RLIMIT_NOFILE","hard": limits.max_open_fds, "soft": limits.max_open_fds},
        {"type":"RLIMIT_NPROC","hard": limits.max_processes, "soft": limits.max_processes},
        {"type":"RLIMIT_FSIZE","hard": limits.max_fs_bytes, "soft": limits.max_fs_bytes}
    ])
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cfg(resolv: Option<&Path>) -> Value {
        build_config(
            &SandboxLimits::constrained(),
            Path::new("/opt/rootfs"),
            Path::new("/run/sbx/ws"),
            Path::new("/run/sbx/exec"),
            resolv,
        )
    }

    #[test]
    fn init_is_sleep_and_root_readonly() {
        let c = cfg(None);
        assert_eq!(c["process"]["args"], json!(["sleep", "infinity"]));
        assert_eq!(c["root"]["readonly"], json!(true));
        assert_eq!(c["root"]["path"], json!("/opt/rootfs"));
        assert_eq!(c["process"]["cwd"], json!("/workspace"));
    }

    #[test]
    fn workspace_rw_exec_ro() {
        let c = cfg(None);
        let mounts = c["mounts"].as_array().unwrap();
        let ws = mounts
            .iter()
            .find(|m| m["destination"] == json!("/workspace"))
            .unwrap();
        assert_eq!(ws["source"], json!("/run/sbx/ws"));
        assert!(ws["options"].as_array().unwrap().contains(&json!("rw")));
        let ex = mounts
            .iter()
            .find(|m| m["destination"] == json!("/__exec"))
            .unwrap();
        assert!(ex["options"].as_array().unwrap().contains(&json!("ro")));
    }

    #[test]
    fn resolv_conf_only_when_egress() {
        assert!(cfg(None)["mounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["destination"] != json!("/etc/resolv.conf")));
        let c = cfg(Some(Path::new("/run/sbx/resolv.conf")));
        assert!(c["mounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["destination"] == json!("/etc/resolv.conf")));
    }

    #[test]
    fn memory_limit_in_linux_resources() {
        // メモリ上限は OCI resources.memory.limit（bytes）へ写る（#346）。
        let c = cfg(None);
        assert_eq!(
            c["linux"]["resources"]["memory"]["limit"],
            json!(SandboxLimits::constrained().memory_mb * 1024 * 1024)
        );
    }

    #[test]
    fn rlimits_from_limits() {
        let c = cfg(None);
        let rl = c["process"]["rlimits"].as_array().unwrap();
        let nofile = rl
            .iter()
            .find(|r| r["type"] == json!("RLIMIT_NOFILE"))
            .unwrap();
        assert_eq!(
            nofile["hard"],
            json!(SandboxLimits::constrained().max_open_fds)
        );
    }
}
