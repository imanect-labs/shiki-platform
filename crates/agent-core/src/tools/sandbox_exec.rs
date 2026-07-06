//! サンドボックス実行の共通ヘルパ（code_interpreter / web_fetch で共用）。

use futures::StreamExt;
use sandbox_client::ExecEvent;

use crate::tool::ToolError;

/// stdout/stderr の会話返却上限（サンドボックス側の 1MiB 上限とは別の、モデル向け整形上限）。
pub(super) const MODEL_OUTPUT_CAP: usize = 16 * 1024;

/// exec ストリームを stdout/stderr/exit に畳み込む。
pub(super) async fn collect_output(
    mut stream: futures::stream::BoxStream<
        'static,
        Result<ExecEvent, sandbox_client::SandboxError>,
    >,
) -> Result<(String, String, Option<i32>, Option<String>), ToolError> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit = None;
    let mut limit = None;
    while let Some(ev) = stream.next().await {
        match ev.map_err(|e| ToolError::Unavailable(format!("sandbox exec: {e}")))? {
            ExecEvent::Stdout(b) => stdout.extend_from_slice(&b),
            ExecEvent::Stderr(b) => stderr.extend_from_slice(&b),
            ExecEvent::Exited { code } => exit = Some(code),
            ExecEvent::LimitExceeded { kind, detail } => {
                limit = Some(format!("リソース超過（{kind:?}）: {detail}"));
            }
        }
    }
    Ok((
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
        exit,
        limit,
    ))
}

/// モデル向け整形上限で打ち切る（UTF-8 の char 境界を守る）。
pub(super) fn truncate(s: &str) -> String {
    if s.len() <= MODEL_OUTPUT_CAP {
        return s.to_string();
    }
    // バイト境界がマルチバイト文字の途中に来るとスライスがパニックするため、
    // 直近の char 境界まで戻す（Python 出力は日本語等を含みうる）。
    let mut end = MODEL_OUTPUT_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…（出力を{end}バイトで打ち切り）", &s[..end])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_utf8_boundary() {
        // マルチバイト（日本語）だけの長い文字列。バイト境界が文字途中でもパニックしない。
        let s = "あ".repeat(MODEL_OUTPUT_CAP); // 3 bytes/char
        let out = truncate(&s);
        assert!(out.contains("バイトで打ち切り"));
        // 打ち切り位置は char 境界（3 の倍数）まで戻っている。
        assert!(out.starts_with('あ'));
    }
}
