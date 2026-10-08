//! ネイティブティア（gVisor/Firecracker）共通の下回り。
//!
//! - [`workspace`]: ホスト側 `/workspace` ディレクトリのファイル操作（パストラバーサルガード）。
//! - [`stream`]: 子プロセス stdout/stderr を `ExecEvent` ストリームへ（出力上限・タイムアウト）。
//! - [`nsenter_command`]: egress netns へゲストランタイムを入れる `nsenter -U -n` コマンド生成。

pub mod stream;
pub mod workspace;

use std::path::Path;

use sandbox_client::SandboxError;
use tokio::process::Command;

/// `nsenter -t <pid> -U -n --preserve-credentials -- <program>` を組み立てる。
///
/// 0-cap プロセスでも、まず userns に入ることで CAP_SYS_ADMIN を得て netns へ join できる
/// （netns だけの join は EPERM になる・実測確認済み）。
#[must_use]
pub fn nsenter_command(netns_pid: u32, program: &str) -> Command {
    let mut cmd = Command::new("nsenter");
    cmd.arg("-t")
        .arg(netns_pid.to_string())
        .arg("-U")
        .arg("-n")
        .arg("--preserve-credentials")
        .arg("--")
        .arg(program);
    cmd
}

/// netns 内で `ip` を実行するコマンド（インターフェース準備用）。
#[must_use]
pub fn nsenter_ip(netns_pid: u32, args: &[&str]) -> Command {
    let mut cmd = nsenter_command(netns_pid, "ip");
    cmd.args(args);
    cmd
}

/// 存在すれば実行可能な絶対パスを返す（設定バイナリの存在確認）。
#[must_use]
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// シェル行をゲストの POSIX シェル（`/bin/sh -c`）で解釈させる argv を組み立てる（#504）。
///
/// ネイティブティアの rootfs は `python:3.12-slim` 由来で `/bin/sh`（dash）を持つ（Firecracker の
/// `rootfs.ext4` も同じツリーから生成・`scripts/build-sandbox-rootfs.sh`）。パイプ・`&&`・リダイレクトが
/// そのまま使える。wasm ティアは brush の PTY 問題で shlex 分割のまま（`backend/wasm/instance.rs`）。
///
/// 能力は増えない: `shell` からは既に `python3` の `subprocess` で任意のパイプを組める。境界は
/// 従来どおり隔離・egress 遮断・使い捨て・承認ゲート（`shell` は `requires_confirmation`）。
pub fn shell_argv(cmd: &str) -> Result<Vec<String>, SandboxError> {
    if cmd.trim().is_empty() {
        return Err(SandboxError::Invalid("empty shell command".into()));
    }
    Ok(vec!["/bin/sh".into(), "-c".into(), cmd.to_string()])
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn nsenter_command_enters_userns_and_netns() {
        let cmd = nsenter_command(4242, "runsc");
        assert_eq!(cmd.as_std().get_program().to_string_lossy(), "nsenter");
        let args = args_of(&cmd);
        assert_eq!(
            args,
            vec![
                "-t",
                "4242",
                "-U",
                "-n",
                "--preserve-credentials",
                "--",
                "runsc"
            ]
        );
    }

    #[test]
    fn nsenter_ip_appends_args() {
        let cmd = nsenter_ip(7, &["link", "set", "lo", "up"]);
        let args = args_of(&cmd);
        assert_eq!(args.first().map(String::as_str), Some("-t"));
        assert!(args.ends_with(&[
            "ip".to_string(),
            "link".to_string(),
            "set".to_string(),
            "lo".to_string(),
            "up".to_string()
        ]));
    }

    #[test]
    fn is_executable_detects_file() {
        assert!(is_executable(Path::new("/bin/sh")));
        assert!(!is_executable(Path::new("/nonexistent/xyz")));
        assert!(!is_executable(Path::new("/")));
    }

    /// シェル行は分割せず `/bin/sh -c` へ 1 引数で渡す（演算子・クォートの解釈はゲストのシェル・#504）。
    #[test]
    fn shell_argv_hands_the_whole_line_to_sh() {
        let line = "cut -d, -f2 a.csv | sort > 'out file.txt' && echo done";
        assert_eq!(
            shell_argv(line).unwrap(),
            vec!["/bin/sh".to_string(), "-c".to_string(), line.to_string()]
        );
    }

    #[test]
    fn shell_argv_rejects_blank_lines() {
        for blank in ["", "   ", "\n\t"] {
            assert!(
                matches!(shell_argv(blank), Err(SandboxError::Invalid(_))),
                "{blank:?} は拒否する"
            );
        }
    }
}
