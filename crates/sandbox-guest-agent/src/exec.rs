//! ゲスト内プロセス実行: argv を自 pgroup で起動し、stdout/stderr を base64 フレームで流し、
//! timeout でグループごと kill する。終端に `Exited{code}` を書く。

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::Engine;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use shiki_sandbox_agent_proto::{write_frame, Event};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// stdout=0 / stderr=1 のタグ付きチャンク。
type Chunk = (u8, Vec<u8>);

/// argv を実行し、結果イベントを `conn` に逐次書く（作業ディレクトリは `cwd`）。
pub(crate) fn run<W: Write>(conn: &mut W, argv: &[String], timeout_ms: u64, cwd: &str) {
    let Some((program, args)) = argv.split_first() else {
        let _ = write_frame(
            conn,
            &Event::Err {
                msg: "empty argv".into(),
            },
        );
        return;
    };

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0); // 自分を pgroup リーダに（timeout でグループ kill）。

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = write_frame(
                conn,
                &Event::Err {
                    msg: format!("spawn {program}: {e}"),
                },
            );
            return;
        }
    };
    let pid = child.id() as i32; // process_group(0) により pgid == pid。

    let (tx, rx) = mpsc::channel::<Chunk>();
    if let Some(out) = child.stdout.take() {
        spawn_reader(out, 0, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        spawn_reader(err, 1, tx.clone());
    }
    drop(tx); // 両 reader 終了で rx が切れる。

    let deadline = Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap_or_else(Instant::now);
    let mut killed = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok((ch, bytes)) => {
                let b64 = B64.encode(&bytes);
                let ev = if ch == 0 {
                    Event::Stdout { b64 }
                } else {
                    Event::Stderr { b64 }
                };
                if write_frame(conn, &ev).is_err() {
                    let _ = kill(Pid::from_raw(-pid), Signal::SIGKILL);
                    let _ = child.wait();
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !killed && Instant::now() >= deadline {
                    let _ = kill(Pid::from_raw(-pid), Signal::SIGKILL);
                    killed = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
    let _ = write_frame(conn, &Event::Exited { code });
}

/// 1 本のパイプを汲み出して mpsc へ送る（EOF/エラーで終了）。
fn spawn_reader<R: Read + Send + 'static>(mut reader: R, ch: u8, tx: mpsc::Sender<Chunk>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send((ch, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use shiki_sandbox_agent_proto::read_frame;
    use std::io::Cursor;

    /// フレーム列を Event に復号する。
    fn decode(buf: Vec<u8>) -> Vec<Event> {
        let mut cur = Cursor::new(buf);
        let mut evs = Vec::new();
        while let Ok(Some(ev)) = read_frame::<_, Event>(&mut cur) {
            evs.push(ev);
        }
        evs
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn echo_streams_stdout_and_exit() {
        let mut buf = Vec::new();
        run(&mut buf, &argv(&["/bin/echo", "hi"]), 5000, ".");
        let evs = decode(buf);
        let stdout: Vec<u8> = evs
            .iter()
            .filter_map(|e| match e {
                Event::Stdout { b64 } => Some(B64.decode(b64).unwrap()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(stdout, b"hi\n");
        assert!(matches!(evs.last(), Some(Event::Exited { code: 0 })));
    }

    #[test]
    fn timeout_kills_process() {
        let mut buf = Vec::new();
        run(&mut buf, &argv(&["/bin/sleep", "30"]), 200, ".");
        let evs = decode(buf);
        // kill されて終了イベントが返る（code は 0 以外・シグナル終了で -1）。
        assert!(matches!(evs.last(), Some(Event::Exited { .. })));
    }

    #[test]
    fn empty_argv_errors() {
        let mut buf = Vec::new();
        run(&mut buf, &[], 1000, ".");
        assert!(matches!(decode(buf).first(), Some(Event::Err { .. })));
    }

    #[test]
    fn nonzero_exit_reported() {
        let mut buf = Vec::new();
        run(&mut buf, &argv(&["/bin/sh", "-c", "exit 3"]), 5000, ".");
        assert!(matches!(
            decode(buf).last(),
            Some(Event::Exited { code: 3 })
        ));
    }
}
