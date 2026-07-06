//! Firecracker API（unix ソケット上の最小 HTTP/1.1 PUT クライアント）。
//!
//! FC は成功時 `204 No Content` を返す。hyper 等を持ち込まず、PUT 1 発ごとに接続する薄い実装。

use std::path::Path;

use sandbox_client::SandboxError;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// `PUT <path>` に JSON body を送り、2xx を確認する。
pub(super) async fn put(sock: &Path, path: &str, body: &Value) -> Result<(), SandboxError> {
    let body_bytes =
        serde_json::to_vec(body).map_err(|e| SandboxError::Internal(format!("fc json: {e}")))?;
    let mut stream = UnixStream::connect(sock)
        .await
        .map_err(|e| SandboxError::Unavailable(format!("fc api connect: {e}")))?;

    let header = format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body_bytes.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|e| SandboxError::Unavailable(format!("fc api write: {e}")))?;
    stream
        .write_all(&body_bytes)
        .await
        .map_err(|e| SandboxError::Unavailable(format!("fc api write body: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| SandboxError::Unavailable(format!("fc api flush: {e}")))?;

    // レスポンスの先頭（ステータス行）だけ読めれば十分。
    let mut buf = [0u8; 1024];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| SandboxError::Unavailable(format!("fc api read: {e}")))?;
    check_status(&buf[..n], path)
}

/// ステータス行を解釈し 2xx 以外をエラーにする。
fn check_status(resp: &[u8], path: &str) -> Result<(), SandboxError> {
    let text = String::from_utf8_lossy(resp);
    let status_line = text.lines().next().unwrap_or_default();
    // "HTTP/1.1 204 No Content"
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(c) if (200..300).contains(&c) => Ok(()),
        Some(c) => Err(SandboxError::Unavailable(format!(
            "fc api {path} -> HTTP {c}: {}",
            text.lines().last().unwrap_or_default()
        ))),
        None => Err(SandboxError::Unavailable(format!(
            "fc api {path}: unparseable response"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixListener;

    /// 指定ステータスを返す in-proc FC API モックを起動し、ソケットパスを返す。
    fn mock_api(dir: &Path, status: &'static str) -> std::path::PathBuf {
        let sock = dir.join("fc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
                    .await;
            }
        });
        sock
    }

    fn tmpdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "fcapi-{}-{}",
            std::process::id(),
            C.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn put_ok_on_204() {
        let d = tmpdir();
        let sock = mock_api(&d, "204 No Content");
        put(
            &sock,
            "/machine-config",
            &serde_json::json!({"vcpu_count":1}),
        )
        .await
        .expect("204 ok");
    }

    #[tokio::test]
    async fn put_err_on_400() {
        let d = tmpdir();
        let sock = mock_api(&d, "400 Bad Request");
        assert!(put(&sock, "/boot-source", &serde_json::json!({}))
            .await
            .is_err());
    }

    #[test]
    fn status_parsing() {
        assert!(check_status(b"HTTP/1.1 204 No Content\r\n\r\n", "/x").is_ok());
        assert!(check_status(b"HTTP/1.1 200 OK\r\n\r\n", "/x").is_ok());
        assert!(check_status(b"HTTP/1.1 400 Bad Request\r\n\r\nboom", "/x").is_err());
        assert!(check_status(b"garbage", "/x").is_err());
    }
}
