//! Firecracker vsock（ホスト側）。FC は uds マルチプレクサを提供し、`CONNECT <port>\n` で
//! ゲストのリッスンポートへ橋渡しする。以後はエージェントプロトコル（u32-LE 長さ前置＋JSON）で会話する。

use std::path::Path;
use std::time::Duration;

use sandbox_client::SandboxError;
use shiki_sandbox_agent_proto::{Event, Request, MAX_FRAME_BYTES};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// ゲストエージェントへの接続（1 接続・逐次）。
pub(super) struct AgentConn {
    stream: UnixStream,
}

impl AgentConn {
    /// FC の vsock uds へ接続し `CONNECT <port>` ハンドシェイクを行う。
    ///
    /// ゲスト起動直後は uds がまだ無い/接続拒否のため、短くリトライする。
    pub(super) async fn connect(
        uds: &Path,
        port: u32,
        timeout: Duration,
    ) -> Result<Self, SandboxError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match Self::try_connect(uds, port).await {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(SandboxError::Unavailable(format!(
                            "agent vsock connect: {e}"
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    async fn try_connect(uds: &Path, port: u32) -> Result<Self, SandboxError> {
        let mut stream = UnixStream::connect(uds)
            .await
            .map_err(|e| SandboxError::Unavailable(format!("uds connect: {e}")))?;
        stream
            .write_all(format!("CONNECT {port}\n").as_bytes())
            .await
            .map_err(|e| SandboxError::Unavailable(format!("connect write: {e}")))?;
        // 応答 "OK <hostport>\n" を 1 行読む。
        let line = read_line(&mut stream).await?;
        if !line.starts_with("OK ") {
            return Err(SandboxError::Unavailable(format!(
                "vsock handshake rejected: {line:?}"
            )));
        }
        Ok(AgentConn { stream })
    }

    /// 要求フレームを送る。
    pub(super) async fn send(&mut self, req: &Request) -> Result<(), SandboxError> {
        let body =
            serde_json::to_vec(req).map_err(|e| SandboxError::Internal(format!("enc: {e}")))?;
        self.stream
            .write_all(&(body.len() as u32).to_le_bytes())
            .await
            .map_err(|e| map_io(&e))?;
        self.stream.write_all(&body).await.map_err(|e| map_io(&e))?;
        self.stream.flush().await.map_err(|e| map_io(&e))?;
        Ok(())
    }

    /// イベントフレームを 1 つ読む（EOF は `None`）。
    pub(super) async fn recv(&mut self) -> Result<Option<Event>, SandboxError> {
        let mut len_buf = [0u8; 4];
        match self.stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(map_io(&e)),
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(SandboxError::Invalid(format!(
                "agent frame too large: {len}"
            )));
        }
        let mut body = vec![0u8; len];
        self.stream
            .read_exact(&mut body)
            .await
            .map_err(|e| map_io(&e))?;
        let ev = serde_json::from_slice(&body)
            .map_err(|e| SandboxError::Internal(format!("dec: {e}")))?;
        Ok(Some(ev))
    }
}

fn map_io(e: &std::io::Error) -> SandboxError {
    SandboxError::Unavailable(format!("agent io: {e}"))
}

/// `\n` 終端の 1 行を読む（ハンドシェイク応答用・短い前提）。
async fn read_line(stream: &mut UnixStream) -> Result<String, SandboxError> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    for _ in 0..256 {
        let n = stream.read(&mut byte).await.map_err(|e| map_io(&e))?;
        if n == 0 {
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        out.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use shiki_sandbox_agent_proto::write_frame;
    use tokio::net::UnixListener;

    fn tmp_sock() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "fcvsock-{}-{}.sock",
            std::process::id(),
            C.fetch_add(1, Ordering::SeqCst)
        ))
    }

    /// CONNECT ハンドシェイク→Ready→exec 要求を待って Stdout/Exited を返すモックエージェント。
    #[tokio::test]
    async fn handshake_and_exec_roundtrip() {
        let sock = tmp_sock();
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            // "CONNECT 5000\n" を読む。
            let mut line = Vec::new();
            let mut b = [0u8; 1];
            loop {
                let n = s.read(&mut b).await.unwrap();
                if n == 0 || b[0] == b'\n' {
                    break;
                }
                line.push(b[0]);
            }
            assert_eq!(String::from_utf8_lossy(&line), "CONNECT 5000");
            s.write_all(b"OK 12345\n").await.unwrap();
            // Ready フレーム（同期 codec で組んで生バイト送出）。
            let mut frame = Vec::new();
            write_frame(
                &mut frame,
                &Event::Ready {
                    version: "t".into(),
                },
            )
            .unwrap();
            s.write_all(&frame).await.unwrap();
            // 1 要求読む（len+body）。
            let mut lb = [0u8; 4];
            s.read_exact(&mut lb).await.unwrap();
            let len = u32::from_le_bytes(lb) as usize;
            let mut body = vec![0u8; len];
            s.read_exact(&mut body).await.unwrap();
            let _req: Request = serde_json::from_slice(&body).unwrap();
            // Stdout+Exited を返す。
            let mut out = Vec::new();
            write_frame(&mut out, &Event::Stdout { b64: "aGk=".into() }).unwrap();
            write_frame(&mut out, &Event::Exited { code: 0 }).unwrap();
            s.write_all(&out).await.unwrap();
        });

        let mut conn = AgentConn::connect(&sock, 5000, Duration::from_secs(2))
            .await
            .expect("connect");
        assert!(matches!(
            conn.recv().await.unwrap(),
            Some(Event::Ready { .. })
        ));
        conn.send(&Request::Exec {
            argv: vec!["echo".into()],
            timeout_ms: 1000,
        })
        .await
        .unwrap();
        assert!(matches!(
            conn.recv().await.unwrap(),
            Some(Event::Stdout { .. })
        ));
        assert!(matches!(
            conn.recv().await.unwrap(),
            Some(Event::Exited { code: 0 })
        ));
    }

    #[tokio::test]
    async fn rejects_bad_handshake() {
        let sock = tmp_sock();
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut b = [0u8; 32];
            let _ = s.read(&mut b).await;
            let _ = s.write_all(b"NOPE\n").await;
        });
        assert!(AgentConn::connect(&sock, 5000, Duration::from_millis(500))
            .await
            .is_err());
    }
}
