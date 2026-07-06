//! ネイティブティア（gVisor/Firecracker）共通の下回り。
//!
//! - [`workspace`]: ホスト側 `/workspace` ディレクトリのファイル操作（パストラバーサルガード）。
//! - [`stream`]: 子プロセス stdout/stderr を `ExecEvent` ストリームへ（出力上限・タイムアウト）。
//! - [`nsenter_command`]: egress netns へゲストランタイムを入れる `nsenter -U -n` コマンド生成。

pub mod stream;
pub mod workspace;

use std::path::Path;

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
}
