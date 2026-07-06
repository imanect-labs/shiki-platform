//! 子プロセス（runsc exec 等）の stdout/stderr を `ExecEvent` ストリームへ写像する。
//!
//! 出力累積上限（超過で kill＋`LimitExceeded{Output}`）と壁時計タイムアウト（`LimitExceeded{WallClock}`）を
//! orchestrator 側の二重防御として強制する。純粋にホスト側 I/O なので `/bin/echo` 等で単体テストできる。

use std::time::Duration;

use futures::stream::BoxStream;
use sandbox_client::{ExecEvent, LimitKind, SandboxError};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// stdout=0 / stderr=1 のタグ付きチャンク。
type Chunk = (u8, Vec<u8>);

/// 子プロセスを exec ストリームへ変換する（stdout/stderr は piped で spawn 済みであること）。
pub fn stream_child(
    mut child: Child,
    max_output: usize,
    timeout: Duration,
) -> BoxStream<'static, Result<ExecEvent, SandboxError>> {
    let (tx, rx) = mpsc::channel::<Result<ExecEvent, SandboxError>>(64);
    let (itx, mut irx) = mpsc::channel::<Chunk>(64);

    if let Some(out) = child.stdout.take() {
        tokio::spawn(pump(out, 0, itx.clone()));
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(pump(err, 1, itx.clone()));
    }
    drop(itx); // 両 reader が終われば irx が閉じる。

    tokio::spawn(async move {
        let mut total = 0usize;
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                biased;
                () = &mut deadline => {
                    let _ = child.start_kill();
                    let _ = tx.send(Ok(ExecEvent::LimitExceeded {
                        kind: LimitKind::WallClock,
                        detail: "wall clock limit exceeded".into(),
                    })).await;
                    break;
                }
                msg = irx.recv() => match msg {
                    Some((ch, bytes)) => {
                        total = total.saturating_add(bytes.len());
                        if total > max_output {
                            let _ = child.start_kill();
                            let _ = tx.send(Ok(ExecEvent::LimitExceeded {
                                kind: LimitKind::Output,
                                detail: "output limit exceeded".into(),
                            })).await;
                            break;
                        }
                        let ev = if ch == 0 {
                            ExecEvent::Stdout(bytes)
                        } else {
                            ExecEvent::Stderr(bytes)
                        };
                        if tx.send(Ok(ev)).await.is_err() {
                            let _ = child.start_kill();
                            let _ = child.wait().await;
                            return;
                        }
                    }
                    None => break, // 両 reader が完了。
                }
            }
        }
        // 終了コードを回収（kill 済みでも wait で reap）。
        let code = match child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(_) => -1,
        };
        let _ = tx.send(Ok(ExecEvent::Exited { code })).await;
    });

    Box::pin(ReceiverStream::new(rx))
}

/// 1 本の読み取り側を汲み出す（EOF/エラーで終了）。
async fn pump<R: AsyncRead + Unpin>(mut reader: R, ch: u8, tx: mpsc::Sender<Chunk>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send((ch, buf[..n].to_vec())).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::process::Stdio;
    use tokio::process::Command;

    async fn collect(mut s: BoxStream<'static, Result<ExecEvent, SandboxError>>) -> Vec<ExecEvent> {
        let mut out = Vec::new();
        while let Some(Ok(ev)) = s.next().await {
            out.push(ev);
        }
        out
    }

    #[tokio::test]
    async fn echo_stdout_and_exit() {
        let child = Command::new("/bin/echo")
            .arg("hello")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let evs = collect(stream_child(child, 1 << 20, Duration::from_secs(5))).await;
        let stdout: Vec<u8> = evs
            .iter()
            .filter_map(|e| match e {
                ExecEvent::Stdout(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(stdout, b"hello\n");
        assert!(matches!(evs.last(), Some(ExecEvent::Exited { code: 0 })));
    }

    #[tokio::test]
    async fn output_limit_kills() {
        // 大量出力を yes で生成し、小さな上限で打ち切る。
        let child = Command::new("/usr/bin/yes")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let evs = collect(stream_child(child, 1024, Duration::from_secs(10))).await;
        assert!(evs.iter().any(|e| matches!(
            e,
            ExecEvent::LimitExceeded {
                kind: LimitKind::Output,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn wall_clock_timeout_kills() {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let evs = collect(stream_child(child, 1 << 20, Duration::from_millis(300))).await;
        assert!(evs.iter().any(|e| matches!(
            e,
            ExecEvent::LimitExceeded {
                kind: LimitKind::WallClock,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn nonzero_exit_code() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let evs = collect(stream_child(child, 1 << 20, Duration::from_secs(5))).await;
        assert!(matches!(evs.last(), Some(ExecEvent::Exited { code: 7 })));
    }
}
