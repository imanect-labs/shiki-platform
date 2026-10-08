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

/// 子が終了した後、まだ届いていない出力を待つ猶予（#504）。
///
/// パイプの切断だけを終端にすると、子が残したバックグラウンドプロセス（`sh -c "srv & echo ok"`）が
/// パイプを握ったまま生き続け、子はとうに終わっているのに timeout まで待つ（その間 vsock 接続も
/// 塞がる）。子が終わったらこの猶予の後にグループごと kill して読み手を終わらせる。
const DRAIN_GRACE: Duration = Duration::from_millis(250);

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
    // グループへ SIGKILL を送った時刻（timeout か、子の終了後の打ち切り）。
    let mut killed_at: Option<Instant> = None;
    // 最後に出力を受け取った時刻。打ち切りは「kill 後に出力が DRAIN_GRACE 途絶えたら」で判定する
    // （kill からの経過時間で切ると、転送待ちで溜まっている出力を捨ててしまう）。
    let mut last_data = Instant::now();
    // 出力が途絶えなくても、timeout＋猶予を過ぎたら必ず抜ける上限。
    let hard_stop = deadline.checked_add(DRAIN_GRACE).unwrap_or(deadline);
    // 子（グループリーダ）が終了した時刻。終了後 DRAIN_GRACE でグループの残りを kill する。
    let mut exited_at: Option<Instant> = None;
    // 出力パイプが全て閉じたか。閉じても**子の終了とはみなさない**（`exec >/dev/null` で出力を閉じて
    // 走り続ける子がある）。閉じた後は子の終了か timeout を待ち、残りを止めてから抜ける。
    let mut pipes_closed = false;
    loop {
        // deadline は**毎周**判定する。子が 100ms 未満間隔で出力し続けると `recv_timeout` が常に
        // `Ok` を返し Timeout ブランチに入らないため、ここで判定しないとタイムアウトが発火しない。
        if killed_at.is_none() && Instant::now() >= deadline {
            kill_leftovers(pid);
            killed_at = Some(Instant::now());
        }
        if exited_at.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
            exited_at = Some(Instant::now());
        }
        if pipes_closed {
            // 出力は全て受け取った。子が終わったか timeout で止めたなら、出力をリダイレクトして
            // 残ったジョブも止めてから抜ける（止めずに抜けると、返った後に /workspace を書き換える）。
            if exited_at.is_some() || killed_at.is_some() {
                if killed_at.is_none() {
                    kill_leftovers(pid);
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if killed_at.is_none() && exited_at.is_some_and(|at| at.elapsed() >= DRAIN_GRACE) {
            kill_leftovers(pid);
            killed_at = Some(Instant::now());
        }
        // kill 後もパイプが閉じない＝止められなかった子孫が握っている（VM 外のテストでは別グループへ
        // 抜けた子孫にグループ kill が届かない）。出力が猶予の間途絶えたら（溜まった分は送り切ってから）
        // パイプを待たずに打ち切る（読み手スレッドは VM 破棄まで残る）。待ち続けると vsock 接続
        // ごと塞がり、後続の exec / destroy も止まる。書き続ける子孫でも timeout＋猶予で抜ける。
        if let Some(at) = killed_at {
            if at.max(last_data).elapsed() >= DRAIN_GRACE || Instant::now() >= hard_stop {
                break;
            }
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok((ch, bytes)) => {
                last_data = Instant::now();
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
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => pipes_closed = true,
        }
    }

    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
    let _ = write_frame(conn, &Event::Exited { code });
}

/// exec が残したプロセスを止める（timeout・子の終了後の打ち切り・#504）。
///
/// VM の PID 1（本エージェント）として動いているなら、自分以外の**全プロセス**へ SIGKILL を送る
/// （`kill(-1)` は呼び出し元と PID 1 を除く）。別グループへ抜けた子孫（`setsid`・自己デーモン化）や、
/// timeout で SIGKILL されたシェルラッパが止め損ねたシェル行のグループまで確実に止めるため。
/// 要求は逐次処理なので、止めてよいのはこの exec のプロセスだけ。PID 1 でない（ホスト上のテスト）
/// なら、子のプロセスグループだけを止める（`kill(-1)` はホストの無関係なプロセスを止める）。
fn kill_leftovers(pid: i32) {
    if nix::unistd::getpid() == Pid::from_raw(1) {
        let _ = kill(Pid::from_raw(-1), Signal::SIGKILL);
    } else {
        // リーダは reap 済みでも、グループの残り（バックグラウンドジョブ）には届く。
        let _ = kill(Pid::from_raw(-pid), Signal::SIGKILL);
    }
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
    fn continuous_output_still_times_out() {
        // 出力が絶えず届く（recv_timeout が常に Ok を返す）状況でも、deadline を**毎周**判定するため
        // kill されて終了する。修正前は Timeout ブランチでしか deadline を見ず永久にハングしていた。
        // フラッドで backlog を作らないよう軽くペースを入れる。
        let mut buf = Vec::new();
        let start = std::time::Instant::now();
        run(
            &mut buf,
            &argv(&["/bin/sh", "-c", "while true; do echo x; sleep 0.02; done"]),
            400,
            ".",
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "deadline を超えてハングしてはならない: {:?}",
            start.elapsed()
        );
        assert!(matches!(decode(buf).last(), Some(Event::Exited { .. })));
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

    /// 子が残したバックグラウンドジョブがパイプを握っていても、子の終了で打ち切る（#504）。
    /// timeout まで待たず、子の終了コードを返し、残ったジョブはグループごと kill される。
    #[test]
    fn background_job_is_cut_off_when_the_child_exits() {
        let marker = std::env::temp_dir().join(format!("bg-job-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let line = format!(
            "(sleep 2; touch {}) & echo started; exit 4",
            marker.display()
        );
        let started = Instant::now();
        let mut buf = Vec::new();
        run(&mut buf, &argv(&["/bin/sh", "-c", &line]), 10_000, ".");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "子の終了で打ち切ること: {:?}",
            started.elapsed()
        );
        let evs = decode(buf);
        assert!(evs.iter().any(
            |e| matches!(e, Event::Stdout { b64 } if B64.decode(b64).unwrap() == b"started\n")
        ));
        assert!(matches!(evs.last(), Some(Event::Exited { code: 4 })));
        // 残ったジョブは kill 済み（生きていれば 2 秒後に marker を作る）。
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "バックグラウンドジョブが生き残っている");
    }

    /// 書き出し先が遅い（vsock 相当）書き手。1 回の write ごとに待つ。
    struct SlowConn(Vec<u8>);
    impl Write for SlowConn {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(3));
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 子が大量に書いてすぐ終わり、転送が猶予より長くかかっても、溜まった出力を捨てない。
    #[test]
    fn queued_output_is_not_dropped_when_forwarding_is_slow() {
        const BYTES: usize = 2_000_000;
        let mut conn = SlowConn(Vec::new());
        let line = format!("head -c {BYTES} /dev/zero");
        run(&mut conn, &argv(&["/bin/sh", "-c", &line]), 30_000, ".");
        let evs = decode(conn.0);
        let got: usize = evs
            .iter()
            .filter_map(|e| match e {
                Event::Stdout { b64 } => Some(B64.decode(b64).unwrap().len()),
                _ => None,
            })
            .sum();
        assert_eq!(got, BYTES, "転送途中の出力が捨てられた");
        assert!(matches!(evs.last(), Some(Event::Exited { code: 0 })));
    }

    /// 出力を閉じて走り続ける子でも、timeout で止めて返る（パイプの切断を終了とみなさない）。
    #[test]
    fn child_that_closes_its_output_still_times_out() {
        let started = Instant::now();
        let mut buf = Vec::new();
        run(
            &mut buf,
            &argv(&["/bin/sh", "-c", "exec >/dev/null 2>&1; sleep 30"]),
            1_000,
            ".",
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(decode(buf).last(), Some(Event::Exited { .. })));
    }

    /// 出力をリダイレクトしたジョブを残して子が終わっても、パイプの切断で抜ける前に残りを止める。
    #[test]
    fn redirected_job_is_stopped_when_the_child_exits() {
        let marker = std::env::temp_dir().join(format!("redirected-job-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let line = format!(
            "(sleep 1; touch {}) >/dev/null 2>&1 & exit 0",
            marker.display()
        );
        let mut buf = Vec::new();
        run(&mut buf, &argv(&["/bin/sh", "-c", &line]), 10_000, ".");
        assert!(matches!(
            decode(buf).last(),
            Some(Event::Exited { code: 0 })
        ));
        std::thread::sleep(Duration::from_secs(2));
        assert!(!marker.exists(), "出力を閉じたジョブが生き残って書き込んだ");
    }

    /// 別グループへ抜けた子孫が書き続けても（出力が途絶えなくても）、timeout＋猶予で抜ける。
    #[test]
    fn detached_writer_is_cut_off_at_the_timeout() {
        let started = Instant::now();
        let mut buf = Vec::new();
        run(
            &mut buf,
            &argv(&[
                "/bin/sh",
                "-c",
                // 子孫は自分で 4 秒後に終わる（テストがホストにプロセスを残さない）。
                "setsid sh -c 'for i in $(seq 80); do echo x; sleep 0.05; done' & echo started",
            ]),
            1_000,
            ".",
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(decode(buf).last(), Some(Event::Exited { .. })));
    }

    /// 別グループへ抜けた子孫（`setsid`）がパイプを握っていても、kill の猶予を過ぎたら打ち切る。
    /// グループ kill が届かないので、パイプの切断を待ち続けると timeout を過ぎても返らない。
    #[test]
    fn detached_descendant_holding_the_pipe_does_not_hang() {
        let started = Instant::now();
        let mut buf = Vec::new();
        run(
            &mut buf,
            &argv(&["/bin/sh", "-c", "setsid sleep 30 & echo started"]),
            10_000,
            ".",
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "打ち切ること: {:?}",
            started.elapsed()
        );
        assert!(matches!(
            decode(buf).last(),
            Some(Event::Exited { code: 0 })
        ));
    }
}
