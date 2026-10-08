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

/// シェル行を包むラッパ（ゲストの `/bin/sh` が `$1` にシェル行を受けて実行する・#504）。
///
/// シェル行は `setsid` で**専用のプロセスグループ**に入れて走らせ、終わったらそのグループを丸ごと
/// SIGKILL してから終了コードを返す。`cmd &` のジョブを残したまま返ると、`shell` ツールが
/// /workspace を書き戻している最中にもジョブが書き続け、途中の版を永続化しうるため。
/// `timeout(1)` の TERM はラッパにだけ届く（グループが別）ので、trap で同じグループを kill する。
/// `&` で起動した子は（ジョブ制御なしの sh では）グループリーダではないので、`setsid` は fork せずに
/// その pid のまま新セッション＝新グループを作る（`$!` がそのままグループ ID になる）。
/// dash の組み込み `kill` は `--` を受け付けない（`Illegal number`）ので `kill -KILL -<pgid>` と書く。
const SHELL_WRAPPER: &str = "setsid /bin/sh -c \"$1\" & p=$!
trap 'kill -KILL -$p 2>/dev/null; exit 143' TERM
wait $p; rc=$?
kill -KILL -$p 2>/dev/null
exit $rc";

/// シェル行をゲストの POSIX シェル（`/bin/sh -c`）で解釈させる argv を組み立てる（#504）。
///
/// ネイティブティアの rootfs は `python:3.12-slim` 由来で `/bin/sh`（dash）と `setsid`（util-linux）を
/// 持つ（Firecracker の `rootfs.ext4` も同じツリーから生成・`scripts/build-sandbox-rootfs.sh`）。
/// パイプ・`&&`・リダイレクトがそのまま使える。シェル行が残したジョブは返る前に止める
/// （[`SHELL_WRAPPER`]）。wasm ティアは brush の PTY 問題で shlex 分割のまま（`backend/wasm/instance.rs`）。
///
/// 能力は増えない: `shell` からは既に `python3` の `subprocess` で任意のパイプを組める。境界は
/// 従来どおり隔離・egress 遮断・使い捨て・承認ゲート（`shell` は `requires_confirmation`）。
pub fn shell_argv(cmd: &str) -> Result<Vec<String>, SandboxError> {
    if cmd.trim().is_empty() {
        return Err(SandboxError::Invalid("empty shell command".into()));
    }
    Ok(vec![
        "/bin/sh".into(),
        "-c".into(),
        SHELL_WRAPPER.into(),
        // `$0`（エラーメッセージ上の名前）。シェル行は `$1` で渡す（ラッパ文字列へ連結しない）。
        "sh".into(),
        cmd.to_string(),
    ])
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

    /// シェル行は分割せず 1 引数で渡す（演算子・クォートの解釈はゲストのシェル・#504）。
    #[test]
    fn shell_argv_hands_the_whole_line_to_sh() {
        let line = "cut -d, -f2 a.csv | sort > 'out file.txt' && echo done";
        let argv = shell_argv(line).unwrap();
        assert_eq!(argv[..2], ["/bin/sh".to_string(), "-c".to_string()]);
        // シェル行はラッパ文字列へ連結せず、位置引数 `$1` としてそのまま渡す。
        assert_eq!(argv.last(), Some(&line.to_string()));
        assert!(!argv[2].contains(line));
    }

    /// ラッパ越しに実行する（ホストの `/bin/sh` と `setsid` で、ゲストと同じ argv を走らせる）。
    fn run_line(line: &str) -> (std::process::Output, std::time::Duration) {
        let argv = shell_argv(line).unwrap();
        let started = std::time::Instant::now();
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .unwrap();
        (out, started.elapsed())
    }

    /// シェル行の解釈・クォート・終了コードはラッパを挟んでも変わらない。
    #[test]
    fn wrapper_keeps_shell_semantics_and_exit_code() {
        let (out, _) = run_line("printf '%s\\n' \"a b\" | tr a-z A-Z; exit 5");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "A B\n");
        assert_eq!(out.status.code(), Some(5));
        let (out, _) = run_line("false && echo never || echo fallback");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "fallback\n");
        assert_eq!(out.status.code(), Some(0));
    }

    /// シェル行が残したジョブは、返る前にグループごと止める（後から /workspace を書き換えない）。
    #[test]
    fn wrapper_kills_jobs_left_by_the_line() {
        let marker = std::env::temp_dir().join(format!("wrapper-bg-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let (out, elapsed) = run_line(&format!(
            "(sleep 1; touch {}) & echo started",
            marker.display()
        ));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "started\n");
        assert_eq!(out.status.code(), Some(0));
        // ジョブがパイプを握ったままなら output() は 1 秒待つ。
        assert!(
            elapsed < std::time::Duration::from_millis(900),
            "{elapsed:?}"
        );
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(!marker.exists(), "ジョブが生き残って書き込んだ");
    }

    /// `timeout(1)` の TERM はラッパにだけ届く。trap がシェル行のグループごと止めて 143 で返る。
    #[test]
    fn wrapper_forwards_term_to_the_line_group() {
        let argv = shell_argv("sleep 30 & sleep 30").unwrap();
        let mut child = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let started = std::time::Instant::now();
        let status = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        // stdout の EOF まで読む＝グループの全員が終わっている（sleep が生きていれば 30 秒待つ）。
        let mut rest = String::new();
        std::io::Read::read_to_string(child.stdout.as_mut().unwrap(), &mut rest).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(143));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
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
