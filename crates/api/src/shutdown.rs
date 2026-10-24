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
    pub(crate) fn on_signal() -> Shutdown {
        let (tx, s) = Self::channel();
        tokio::spawn(async move {
            shutdown_signal().await;
            let _ = tx.send(true);
        });
        s
    }

    /// 合図が立つまで待つ（立てる側が落ちた場合も停止として扱う）。
    pub(crate) async fn wait(mut self) {
        let _ = self.rx.wait_for(|v| *v).await;
    }
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
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("ctrl_c ハンドラ設定に失敗");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM ハンドラ設定に失敗")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("シャットダウンシグナル受信");
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
}
