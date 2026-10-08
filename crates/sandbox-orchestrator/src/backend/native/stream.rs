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

/// 子が終了した後、まだ届いていない出力を待つ猶予（#504）。
///
/// パイプの EOF だけを終端にすると、子が残したバックグラウンドプロセス（`sh -c "srv & echo ok"`）が
/// パイプを握ったまま生き続け、子はとうに終わっているのに壁時計上限まで待って `LimitExceeded` になる。
/// 子が終わったらこの猶予だけ汲み出して打ち切る。残ったプロセスはここでは止めない。
/// 出力先の読み手を失うので、次の書込で EPIPE/SIGPIPE を受けるか、
/// サンドボックス破棄で消える。`shell` / `code_interpreter` は呼び出しごとに破棄するので後続の exec
/// からは見えない。
const DRAIN_GRACE: Duration = Duration::from_millis(250);
/// 子の終了を確かめる間隔。
const EXIT_POLL: Duration = Duration::from_millis(50);

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
        // 子の終了時刻と終了コード（終わったら DRAIN_GRACE だけ汲み出して打ち切る）。
        let mut exited: Option<(tokio::time::Instant, i32)> = None;
        let mut poll = tokio::time::interval(EXIT_POLL);
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
                },
                _ = poll.tick() => match exited {
                    None => {
                        if let Ok(Some(status)) = child.try_wait() {
                            exited = Some((tokio::time::Instant::now(), status.code().unwrap_or(-1)));
                        }
                    }
                    // 子は終わったがパイプが閉じない＝子孫が握っている。猶予を過ぎたら打ち切る。
                    Some((at, _)) if at.elapsed() >= DRAIN_GRACE => break,
                    Some(_) => {}
                },
            }
        }
        // 終了コードを回収（kill 済みでも wait で reap）。
        let code = match exited {
            Some((_, code)) => code,
            None => match child.wait().await {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            },
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

    /// 子が残したバックグラウンドプロセスがパイプを握っていても、子の終了で打ち切る（#504）。
    /// 壁時計上限まで待たず、`LimitExceeded` にもならず、子の終了コードを返す。
    #[tokio::test]
    async fn background_job_does_not_hold_the_stream_open() {
        let child = Command::new("/bin/sh")
            .args(["-c", "sleep 30 & echo started; exit 3"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let evs = collect(stream_child(child, 1 << 20, Duration::from_secs(10))).await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "子の終了で打ち切ること: {:?}",
            started.elapsed()
        );
        let stdout: Vec<u8> = evs
            .iter()
            .filter_map(|e| match e {
                ExecEvent::Stdout(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(String::from_utf8_lossy(&stdout), "started\n");
        assert!(!evs
            .iter()
            .any(|e| matches!(e, ExecEvent::LimitExceeded { .. })));
        assert!(matches!(evs.last(), Some(ExecEvent::Exited { code: 3 })));
    }
}
