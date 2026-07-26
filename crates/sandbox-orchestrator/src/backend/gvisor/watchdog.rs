//! gVisor メモリ watchdog（#346・design-caveats PIT-24）。
//!
//! `--ignore-cgroups`（rootless）ではメモリ上限のハード強制ができないため、
//! OCI spec の `resources.memory.limit`（ゲスト側ソフト上限・bundle.rs）に加えて orchestrator 側から
//! `runsc events --stats` を周期監視し、`SandboxLimits.memory_mb` 超過で kill する
//! **二重防御**を敷く。ソフト強制であることに変わりはない（cgroups が使える環境では
//! cgroup 上限が最終防衛・fork-policy.md）。
//!
//! 監視タスクはインスタンス破棄（[`super::instance::GvisorInstance::destroy`]）で abort する。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use super::instance::{runsc_base, RunscConfig};

/// `runsc events --stats` の出力からメモリ使用量（bytes）を取り出す。
///
/// 形式は OCI ランタイムの stats イベント（`{"type":"stats","data":{"memory":{"usage":{"usage":N}}}}`）。
/// パース不能は None（監視は継続・kill はしない＝誤検知で正常サンドボックスを殺さない）。
fn parse_memory_usage(stdout: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    v.get("data")?
        .get("memory")?
        .get("usage")?
        .get("usage")?
        .as_u64()
}

/// メモリ watchdog タスクを起動する（`interval` ごとに stats を確認・超過で kill）。
///
/// 返り値の [`tokio::task::JoinHandle`] は destroy 時に abort すること。
pub(super) fn spawn(
    runsc: Arc<RunscConfig>,
    root_dir: PathBuf,
    id: String,
    tenant_id: String,
    memory_limit_bytes: u64,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            // events は netns 非依存（ホスト側 runsc の状態問い合わせ）。hang 対策の短いタイムアウト。
            let fut = runsc_base(&runsc, &root_dir, None, "none")
                .arg("events")
                .arg("--stats")
                .arg(&id)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .output();
            let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(3), fut).await else {
                continue;
            };
            if !out.status.success() {
                // コンテナが既に終了している（destroy 済み等）。監視終了。
                return;
            }
            let Some(usage) = String::from_utf8(out.stdout)
                .ok()
                .as_deref()
                .and_then(parse_memory_usage)
            else {
                continue;
            };
            if usage <= memory_limit_bytes {
                continue;
            }
            // 超過: kill（インスタンスの exec は以後失敗する・fail-closed）。delete/状態掃除は
            // destroy / TTL sweeper が担う。監査ログ（sandbox_audit target）に理由を残す。
            tracing::warn!(
                target: "sandbox_audit",
                tenant = %tenant_id,
                sandbox = %id,
                usage_bytes = usage,
                limit_bytes = memory_limit_bytes,
                reason = "memory_limit",
                "gVisor サンドボックスがメモリ上限を超過したため kill します"
            );
            let _ = runsc_base(&runsc, &root_dir, None, "none")
                .arg("kill")
                .arg(&id)
                .arg("KILL")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status()
                .await;
            return;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::parse_memory_usage;

    #[test]
    fn parses_oci_stats_event_shape() {
        let out = r#"{"type":"stats","id":"gv-x","data":{"memory":{"usage":{"usage":123456,"max":200000}}}}"#;
        assert_eq!(parse_memory_usage(out), Some(123_456));
        // 形式外・壊れた JSON は None（誤検知で kill しない）。
        assert_eq!(parse_memory_usage("not json"), None);
        assert_eq!(parse_memory_usage(r#"{"type":"stats","data":{}}"#), None);
    }
}
