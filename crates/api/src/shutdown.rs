//! プロセスの停止（SIGTERM / Ctrl-C）を全リスナへ伝え、期限内に終了させる。
//!
//! 以前はメインリスナだけが graceful shutdown を持ち、SSE（データの差分配信・チャット）のような
//! 長寿命の接続が残ると `axum::serve` が返らず、プロセスはゲートウェイ（第 2・第 3 リスナ）の
//! ポートを握ったまま生き続けた（再起動した新プロセスが bind に失敗する）。ここでは:
//!
//! 1. シグナルを受けたら [`Shutdown`] を立て、全リスナ（メイン・ゲートウェイ・B1 配信・Office 中継）が
//!    同時に受付を止める（処理中の要求は続ける）。
//! 2. 処理中の要求を待つのは猶予 [`SHUTDOWN_GRACE`] まで（[`serve_until_shutdown`]）。超えたら残った
//!    接続を捨てて戻る。
//! 3. ランタイムの停止は [`RUNTIME_SHUTDOWN_TIMEOUT`] で打ち切る（常駐ワーカー・残った接続のタスクを
//!    捨てる）。プロセスは「猶予 + 打ち切り」の有限時間で必ず終了する。
//! 4. SSE（`text/event-stream`）の応答は合図で本文を終える（[`end_streams_on_shutdown`]）。SSE は
//!    自分からは終わらないので、猶予を待つと停止が毎回 10 秒かかっていた（3 回目の検証）。クライアントは
//!    `Last-Event-ID` から再接続する（at-least-once・新しいプロセスへ繋がる）。

use std::future::Future;
use std::time::Duration;

use tokio::sync::watch;

/// 停止の合図から、処理中の要求を待つ猶予。
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// ランタイムの停止（常駐タスクの破棄）を待つ上限。
pub(crate) const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// 停止の合図（複製して各リスナに渡す）。
#[derive(Clone)]
pub(crate) struct Shutdown {
    rx: watch::Receiver<bool>,
}

impl Shutdown {
    /// 合図を立てる側と、受ける側の組。
    pub(crate) fn channel() -> (watch::Sender<bool>, Shutdown) {
        let (tx, rx) = watch::channel(false);
        (tx, Shutdown { rx })
    }

    /// SIGTERM / Ctrl-C で立つ合図を作る（受信タスクを spawn する）。
    ///
    /// SIGTERM のハンドラはこの呼び出しの中で登録する（spawn したタスクが走る前に届いた SIGTERM で
    /// 既定動作の即時終了にならない）。
    pub(crate) fn on_signal() -> Shutdown {
        let (tx, s) = Self::channel();
        let signal = shutdown_signal();
        tokio::spawn(async move {
            signal.await;
            let _ = tx.send(true);
        });
        s
    }

    /// 合図が立つまで待つ（立てる側が落ちた場合も停止として扱う）。
    pub(crate) async fn wait(mut self) {
        let _ = self.rx.wait_for(|v| *v).await;
    }
}

/// ルータの SSE（`text/event-stream`）の応答を、停止の合図で終わらせる（猶予を待たずに閉じる）。
///
/// 本文のストリームを合図までで切るだけなので、送りかけのイベントは途中で切れない（チャンク単位で終わる）。
/// SSE 以外の応答（処理中の通常の要求）はそのまま猶予まで待つ。
pub(crate) fn end_streams_on_shutdown(router: axum::Router, shutdown: &Shutdown) -> axum::Router {
    let shutdown = shutdown.clone();
    router.layer(axum::middleware::map_response(
        move |resp: axum::response::Response| {
            let shutdown = shutdown.clone();
            async move { end_event_stream(resp, shutdown) }
        },
    ))
}

fn end_event_stream(
    resp: axum::response::Response,
    shutdown: Shutdown,
) -> axum::response::Response {
    use futures::StreamExt;
    let is_sse = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    if !is_sse {
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let stream = body.into_data_stream().take_until(shutdown.wait());
    axum::response::Response::from_parts(parts, axum::body::Body::from_stream(stream))
}

/// リスナを走らせ、停止の合図の後は猶予 `grace` までだけ処理中の要求を待つ。
///
/// `server` は合図で受付を止める graceful shutdown つきの `axum::serve`。長寿命の接続（SSE）が残って
/// 戻らないときも、猶予を過ぎたら戻る。
pub(crate) async fn serve_until_shutdown<F, E>(
    server: F,
    shutdown: Shutdown,
    grace: Duration,
) -> Result<(), E>
where
    F: Future<Output = Result<(), E>>,
{
    tokio::pin!(server);
    tokio::select! {
        r = &mut server => r,
        () = async {
            shutdown.wait().await;
            tokio::time::sleep(grace).await;
        } => {
            tracing::warn!(
                grace_secs = grace.as_secs(),
                "停止の猶予を過ぎても終わらない接続を打ち切ります（SSE などの長寿命の接続）"
            );
            Ok(())
        }
    }
}

// シグナルハンドラの登録失敗はプロセス起動直後の致命的な環境不整合であり、
// 継続不能なため `expect` で即時 abort する（本番運用でも復帰手段が無い）。
#[allow(clippy::expect_used)]
fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM ハンドラ設定に失敗");
    async move {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("ctrl_c ハンドラ設定に失敗");
        };
        #[cfg(unix)]
        let terminate = async move {
            term.recv().await;
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            () = ctrl_c => {},
            () = terminate => {},
        }
        tracing::info!("シャットダウンシグナル受信");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 終わらない接続が残っていても、合図の後は猶予で戻る。
    #[tokio::test]
    async fn returns_after_grace_even_if_connections_never_finish() {
        let (tx, s) = Shutdown::channel();
        let never = std::future::pending::<Result<(), std::io::Error>>();
        let grace = Duration::from_millis(300);
        let h = tokio::spawn(serve_until_shutdown(never, s, grace));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!h.is_finished(), "合図の前は止まらない");
        let sent = std::time::Instant::now();
        tx.send(true).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!h.is_finished(), "猶予の間は処理中の要求を待つ");
        tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(sent.elapsed() >= grace);
    }

    /// 接続が先に終われば猶予を待たずに戻る。複製した受け手も全員合図を受ける。
    #[tokio::test]
    async fn returns_immediately_when_server_finishes_and_all_listeners_see_the_signal() {
        let (tx, s) = Shutdown::channel();
        let others: Vec<_> = (0..3).map(|_| tokio::spawn(s.clone().wait())).collect();
        let server = {
            let s = s.clone();
            async move {
                s.wait().await;
                Ok::<(), std::io::Error>(())
            }
        };
        let h = tokio::spawn(serve_until_shutdown(server, s, Duration::from_hours(1)));
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        for o in others {
            tokio::time::timeout(Duration::from_secs(5), o)
                .await
                .unwrap()
                .unwrap();
        }
    }

    /// 起動から停止までを main と同じ形で通す（B-OPS-1）: SIGTERM で両リスナ（メイン・
    /// ゲートウェイ）が同時に受付を止め、終わらない SSE 接続が両方に残っていても、メインは猶予で戻り、
    /// ランタイムの停止も打ち切りで終わる。ポートは解放される（再起動した新プロセスが bind できる）。
    #[cfg(unix)]
    #[test]
    fn sigterm_stops_every_listener_and_the_runtime_finishes_within_the_grace() {
        use std::convert::Infallible;
        use std::net::{SocketAddr, TcpListener as StdListener, TcpStream as StdStream};
        use std::time::Instant;

        use axum::response::sse::{Event, Sse};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        fn sse_router() -> axum::Router {
            axum::Router::new().route(
                "/sse",
                axum::routing::get(|| async {
                    Sse::new(futures::stream::pending::<Result<Event, Infallible>>())
                }),
            )
        }
        /// SSE を開いて応答ヘッダまで読み、接続を開いたまま返す。
        async fn open_sse(addr: SocketAddr) -> tokio::net::TcpStream {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(b"GET /sse HTTP/1.1\r\nhost: t\r\n\r\n")
                .await
                .unwrap();
            let mut buf = vec![0u8; 1024];
            let mut got = Vec::new();
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).await.unwrap();
                assert!(n > 0, "SSE の応答ヘッダの前に切れた");
                got.extend_from_slice(&buf[..n]);
            }
            assert!(
                got.starts_with(b"HTTP/1.1 200"),
                "{}",
                String::from_utf8_lossy(&got)
            );
            s
        }

        let grace = Duration::from_millis(1500);
        let gateway_addr = {
            let l = StdListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let (main_addr, signalled_at) = rt.block_on(async {
            let shutdown = Shutdown::on_signal();
            crate::wiring_gateway::spawn_gateway_listener(
                Some(sse_router()),
                Some(gateway_addr.to_string()),
                &shutdown,
            )
            .await
            .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let main_addr = listener.local_addr().unwrap();
            let server =
                axum::serve(listener, sse_router()).with_graceful_shutdown(shutdown.clone().wait());
            let main = tokio::spawn(serve_until_shutdown(
                std::future::IntoFuture::into_future(server),
                shutdown,
                grace,
            ));
            // 両方のリスナに終わらない SSE を残す（常駐ワーカー相当の終わらないタスクも置く）。
            let _main_sse = open_sse(main_addr).await;
            let _gateway_sse = open_sse(gateway_addr).await;
            tokio::spawn(async {
                loop {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            });

            // 起点は kill の前に取る（シグナルは kill が返る前に届いて猶予の計時が始まりうる）。
            let signalled_at = Instant::now();
            let status = std::process::Command::new("kill")
                .args(["-TERM", &std::process::id().to_string()])
                .status()
                .unwrap();
            assert!(status.success());

            // 合図の直後に両リスナとも受付を止める（SSE が残っていても）。
            let deadline = Instant::now() + Duration::from_secs(1);
            for addr in [main_addr, gateway_addr] {
                loop {
                    if tokio::net::TcpStream::connect(addr).await.is_err() {
                        break;
                    }
                    assert!(Instant::now() < deadline, "{addr} が受付を止めない");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            assert!(!main.is_finished(), "猶予の間は処理中の要求を待つ");
            tokio::time::timeout(grace + Duration::from_secs(3), main)
                .await
                .expect("猶予を過ぎてもメインが戻らない")
                .unwrap()
                .unwrap();
            (main_addr, signalled_at)
        });
        let returned = signalled_at.elapsed();
        assert!(returned >= grace, "猶予の前に戻った: {returned:?}");
        rt.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        let finished = signalled_at.elapsed();
        assert!(
            finished < grace + RUNTIME_SHUTDOWN_TIMEOUT,
            "猶予 + 打ち切りの時間内に終わらない: {finished:?}"
        );
        // ポートが解放され、新しいプロセスが同じポートで bind できる。
        for addr in [main_addr, gateway_addr] {
            assert!(StdStream::connect(addr).is_err(), "{addr} がまだ受け付ける");
            StdListener::bind(addr).unwrap();
        }
    }

    /// SSE は停止の合図で本文が終わり、猶予を待たずにサーバが戻る（3 回目の検証: 毎回 10 秒かかっていた）。
    /// SSE 以外の応答には触れない。
    #[tokio::test]
    async fn event_streams_end_on_the_signal_without_waiting_for_the_grace() {
        use std::convert::Infallible;
        use std::time::Instant;

        use axum::response::sse::{Event, Sse};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (tx, shutdown) = Shutdown::channel();
        let router = axum::Router::new()
            .route(
                "/sse",
                axum::routing::get(|| async {
                    Sse::new(futures::stream::pending::<Result<Event, Infallible>>())
                }),
            )
            .route("/plain", axum::routing::get(|| async { "ok" }));
        let router = end_streams_on_shutdown(router, &shutdown);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = axum::serve(listener, router).with_graceful_shutdown(shutdown.clone().wait());
        let grace = Duration::from_secs(30);
        let main = tokio::spawn(serve_until_shutdown(
            std::future::IntoFuture::into_future(server),
            shutdown,
            grace,
        ));
        let plain = reqwest::get(format!("http://{addr}/plain")).await.unwrap();
        assert_eq!(plain.text().await.unwrap(), "ok", "SSE 以外はそのまま");
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /sse HTTP/1.1\r\nhost: t\r\n\r\n")
            .await
            .unwrap();
        let mut buf = vec![0u8; 1024];
        let mut got = Vec::new();
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut buf).await.unwrap();
            assert!(n > 0);
            got.extend_from_slice(&buf[..n]);
        }
        assert!(got.starts_with(b"HTTP/1.1 200"));
        let sent = Instant::now();
        tx.send(true).unwrap();
        // 本文が終わる（チャンクの終端 `0\r\n\r\n`）か接続が閉じる。
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let n = s.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                got.extend_from_slice(&buf[..n]);
                if got.windows(5).any(|w| w == b"0\r\n\r\n") {
                    return;
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "SSE の本文が合図で終わらない");
        tokio::time::timeout(Duration::from_secs(5), main)
            .await
            .expect("猶予を待たずに戻る")
            .unwrap()
            .unwrap();
        assert!(sent.elapsed() < Duration::from_secs(5));
    }
}
