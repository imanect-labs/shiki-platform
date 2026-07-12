//! 隔離ランナー（`shiki-tabular-runner`）の起動（Task 11P.7・PIT-39）。
//!
//! 1 リクエスト = 1 サブプロセス（per-query 隔離）。CSV バイトの解釈・DuckDB 実行は
//! すべてこの非特権サブプロセス内で行い、**api プロセスには一切食わせない**。タイムアウトで
//! プロセスごと kill する（C++ パーサ暴走・メモリ食い潰し・クォータ超過をプロセス境界で封じる）。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::TabularError;
use crate::protocol::{RunnerRequest, RunnerResponse};

/// 実行ファイルを絶対パスへ解決する（`env_clear()` 前に呼ぶ）。
///
/// ランナー起動は `env_clear()` で環境を落とす（資格情報を渡さない・INV）が、これは
/// **PATH も消す**ため、`binary_path` が bare 名（例 既定の `shiki-tabular-runner`）だと
/// 実行ファイル解決に失敗する（compose/本番デプロイの既定で ENOENT）。親プロセスの PATH で
/// 先に絶対パスへ解決しておくことで、env をクリアしても bare 名が解決できる。スラッシュ入り
/// （相対/絶対）はそのまま返す。
fn resolve_program(binary_path: &str) -> PathBuf {
    resolve_in_path(binary_path, std::env::var_os("PATH").as_deref())
}

/// PATH 値を引数で受ける純粋版（global env を触らずテスト可能にする）。
fn resolve_in_path(binary_path: &str, path_var: Option<&std::ffi::OsStr>) -> PathBuf {
    if binary_path.contains('/') {
        return PathBuf::from(binary_path);
    }
    if let Some(paths) = path_var {
        for dir in std::env::split_paths(paths) {
            let candidate = dir.join(binary_path);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    // 見つからなければ bare 名のまま（従来どおり明示的な起動失敗にする）。
    PathBuf::from(binary_path)
}

/// ランナー起動の設定（バイナリパス・時間クォータ）。
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// `shiki-tabular-runner` の実行パス。
    pub binary_path: String,
    /// 実行の時間上限（超過でプロセス kill・クォータ）。
    pub timeout: Duration,
}

impl RunnerConfig {
    pub fn new(binary_path: impl Into<String>, timeout: Duration) -> Self {
        RunnerConfig {
            binary_path: binary_path.into(),
            timeout,
        }
    }
}

/// リクエストを隔離プロセスで実行し、応答を返す（タイムアウトは QuotaExceeded）。
pub async fn run_isolated(
    config: &RunnerConfig,
    request: &RunnerRequest,
) -> Result<RunnerResponse, TabularError> {
    let payload = serde_json::to_vec(request)
        .map_err(|e| TabularError::Internal(format!("request 直列化に失敗: {e}")))?;

    // bare 名は PATH で絶対パスへ解決してから env_clear する（env_clear が PATH を消すため）。
    let program = resolve_program(&config.binary_path);
    let mut child = Command::new(&program)
        // 資格情報・環境を渡さない（ランナーは authz を持たない・INV）。
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TabularError::Runner(format!("ランナー起動に失敗: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .await
            .map_err(|e| TabularError::Runner(format!("stdin 書込に失敗: {e}")))?;
        // EOF を送る（drop で閉じる）。
        drop(stdin);
    }

    let output = match tokio::time::timeout(config.timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(TabularError::Runner(format!("ランナー実行に失敗: {e}"))),
        Err(_) => {
            // タイムアウト: kill_on_drop で child は落ちる。クォータ超過として返す。
            return Err(TabularError::QuotaExceeded(
                "実行時間の上限を超えました".into(),
            ));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 異常終了（SIGKILL 等＝メモリ超過の可能性）はクォータ超過に寄せる。
        if output.status.code().is_none() {
            return Err(TabularError::QuotaExceeded(format!(
                "ランナーが強制終了しました（メモリ超過の可能性）: {stderr}"
            )));
        }
        return Err(TabularError::Runner(format!(
            "ランナーが失敗しました: {stderr}"
        )));
    }

    let response: RunnerResponse = serde_json::from_slice(&output.stdout)
        .map_err(|e| TabularError::Runner(format!("応答のデコードに失敗: {e}")))?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::resolve_in_path;
    use std::path::PathBuf;

    #[test]
    fn slashed_path_is_returned_verbatim() {
        // 相対/絶対（スラッシュ入り）はそのまま（PATH 解決しない）。
        let p = std::env::join_paths(["/nonexistent"]).unwrap();
        assert_eq!(
            resolve_in_path(
                "crates/tabular/runner/target/release/shiki-tabular-runner",
                Some(p.as_os_str())
            ),
            PathBuf::from("crates/tabular/runner/target/release/shiki-tabular-runner")
        );
        assert_eq!(
            resolve_in_path("/usr/local/bin/shiki-tabular-runner", Some(p.as_os_str())),
            PathBuf::from("/usr/local/bin/shiki-tabular-runner")
        );
    }

    #[test]
    fn bare_name_resolves_via_path_before_env_clear() {
        // env_clear() が PATH を消す前に、bare 名を PATH 上の実ファイルへ絶対解決すること。
        // 一時ディレクトリに実行ファイルを置き、PATH 値（引数）から解決を検証する（global env 不変）。
        let dir = std::env::temp_dir().join(format!("shiki-runner-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("fake-runner-xyz");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let path = std::env::join_paths([dir.as_os_str(), "/usr/bin".as_ref()]).unwrap();
        let resolved = resolve_in_path("fake-runner-xyz", Some(path.as_os_str()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(resolved, bin, "bare 名は PATH 上の絶対パスへ解決される");
    }

    #[test]
    fn missing_bare_name_falls_back_to_bare() {
        // PATH に無い bare 名は従来どおり bare のまま（起動時に明示エラー）。
        let path = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        assert_eq!(
            resolve_in_path("definitely-not-on-path-zzz-12345", Some(path.as_os_str())),
            PathBuf::from("definitely-not-on-path-zzz-12345")
        );
    }
}
