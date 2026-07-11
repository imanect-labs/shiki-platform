//! 隔離ランナー（`shiki-tabular-runner`）の起動（Task 11P.7・PIT-39）。
//!
//! 1 リクエスト = 1 サブプロセス（per-query 隔離）。CSV バイトの解釈・DuckDB 実行は
//! すべてこの非特権サブプロセス内で行い、**api プロセスには一切食わせない**。タイムアウトで
//! プロセスごと kill する（C++ パーサ暴走・メモリ食い潰し・クォータ超過をプロセス境界で封じる）。

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::TabularError;
use crate::protocol::{RunnerRequest, RunnerResponse};

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

    let mut child = Command::new(&config.binary_path)
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
