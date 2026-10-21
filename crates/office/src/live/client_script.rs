//! `CoolWsClient` のスクリプト実行・書き出し（#725）。`client.rs` から切り出した拡張。

use super::client::{wait_matching, CoolWsClient};
use super::error::LiveError;
use super::script::{self, ScriptResult};

/// 書き出しの本文の上限（プレビュー用 PDF を想定）。
const MAX_DOWNLOAD_BYTES: usize = 64 << 20;

impl CoolWsClient {
    /// 固定のスクリプトを kit 内で実行し、戻り値（JSON）を得る。
    ///
    /// `source` は**リポジトリ管理の固定ソース**（`office::engine` が組み立てる）。
    /// LLM の出力は `args` の JSON としてだけ渡す（PIT-68 ⑤）。
    pub async fn execute_script(
        &mut self,
        source: &str,
        args: &serde_json::Value,
    ) -> Result<ScriptResult, LiveError> {
        let call_id = self.next_call_id;
        self.next_call_id += 1;
        self.send(script::executescript_line(call_id, source, args))
            .await?;
        let timeout = self.phase_timeout(self.script_timeout)?;
        let want = call_id.to_string();
        wait_matching(&mut self.stream, timeout, "script", |msg| {
            script::parse_executescriptresult(msg)
                .filter(|(id, _)| *id == want)
                .map(|(_, result)| result)
        })
        .await
    }

    /// 現在の状態（未保存の編集を含む）を `format`（例 `pdf`）で書き出して取得する。
    pub async fn download_as(&mut self, format: &str) -> Result<Vec<u8>, LiveError> {
        let name = format!("preview.{format}");
        self.send(script::downloadas_line(&name, format)).await?;
        let timeout = self.phase_timeout(self.script_timeout)?;
        let download_id = wait_matching(
            &mut self.stream,
            timeout,
            "downloadas",
            script::parse_downloadas,
        )
        .await?;
        let url = format!(
            "{}/cool/{}/download/{}",
            self.http_origin,
            percent_encoding::utf8_percent_encode(
                &self.wopi_src,
                percent_encoding::NON_ALPHANUMERIC
            ),
            download_id
        );
        let timeout = self.phase_timeout(self.script_timeout)?;
        // 見出しの受信だけでなく本文の受信まで同じ期限で包み、上限を超える本文は読み切る前に止める
        // （遅い・大きい本文でセッション枠と AI の直列化ロックを抱え続けない）。
        tokio::time::timeout(timeout, fetch_limited(&url))
            .await
            .map_err(|_| LiveError::Timeout("download"))?
    }
}

/// 書き出しの本文を上限つきで取得する。
async fn fetch_limited(url: &str) -> Result<Vec<u8>, LiveError> {
    use futures::StreamExt;
    let fail = |e: reqwest::Error| {
        LiveError::Protocol(format!("書き出しの取得に失敗: {}", e.without_url()))
    };
    let response = reqwest::get(url).await.map_err(fail)?;
    if !response.status().is_success() {
        return Err(LiveError::Protocol(format!(
            "書き出しの取得に失敗: HTTP {}",
            response.status()
        )));
    }
    let too_large = || LiveError::Protocol("書き出しが大きすぎます".into());
    if response
        .content_length()
        .is_some_and(|n| n > MAX_DOWNLOAD_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(fail)?;
        if body.len() + chunk.len() > MAX_DOWNLOAD_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
