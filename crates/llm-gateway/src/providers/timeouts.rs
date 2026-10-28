//! LLM 呼び出しの 3 種のタイムアウト（全アダプタ共通・#554）。
//!
//! reqwest の `RequestBuilder::timeout` は接続から本文の読み終わりまでの全体に 1 本で掛かる。
//! それだと「応答が返らない」「SSE の途中で止まった」を長い生成と区別できないので、
//! 次の 3 つに分けて測る（値は [`LlmTimeouts`]）:
//!
//! - **最初の応答**: 送信から応答ヘッダが返るまで（[`RequestClock::send`]）。
//! - **無通信**: 本文のチャンクとチャンクの間隔（[`RequestClock::next_chunk`]）。
//! - **全体**: 1 回の生成全体の締め切り。上の 2 つの待ちは、どれも締め切りまでの残りで頭打ちにする。
//!
//! どれも [`LlmError::Unavailable`] にして既存の再試行・フォールバックに乗せ、どのタイムアウトで
//! 切れたかをメッセージに入れる。

use std::future::Future;
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use futures::{Stream, StreamExt};
use tokio::time::Instant;

use crate::config::LlmTimeouts;
use crate::provider::{DeltaStream, LlmError};

/// どのタイムアウトで切れたか（メッセージ用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    FirstResponse,
    Idle,
    Total,
}

/// チャンク待ちのあいだ、受け手が閉じたかを見る間隔。
const ABANDON_POLL: Duration = Duration::from_millis(200);

/// 桁あふれ時の締め切り（実質無制限・約 30 年後）。
fn far_future() -> Instant {
    Instant::now() + Duration::from_hours(24 * 365 * 30)
}

/// 1 回の生成の時計（送信時に作り、ストリームを読み終えるまで持つ）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct RequestClock {
    timeouts: LlmTimeouts,
    deadline: Instant,
    label: &'static str,
}

impl RequestClock {
    /// 今から測り始める。`label` はエラーメッセージの接頭辞（アダプタ名）。
    pub(crate) fn start(timeouts: LlmTimeouts, label: &'static str) -> Self {
        RequestClock {
            timeouts,
            // 極端に大きな値（「無制限」のつもりの u64::MAX 等）で桁あふれさせない。
            deadline: Instant::now()
                .checked_add(timeouts.total)
                .unwrap_or_else(far_future),
            label,
        }
    }

    /// `limit` を全体の締め切りまでの残りで頭打ちにする（残りの方が短ければ全体として数える）。
    fn bounded(&self, limit: Duration, kind: Kind) -> (Duration, Kind) {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining <= limit {
            (remaining, Kind::Total)
        } else {
            (limit, kind)
        }
    }

    /// 全体の締め切りまでの残り（429 の待ちが締め切りを越えないかの判定に使う）。
    pub(crate) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// 全体の締め切りを過ぎたか。
    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn timed_out(&self, kind: Kind) -> LlmError {
        let label = self.label;
        LlmError::Unavailable(match kind {
            Kind::FirstResponse => format!(
                "{label}: no response within {:?} (first-response timeout)",
                self.timeouts.first_response
            ),
            Kind::Idle => format!(
                "{label}: stream stalled for {:?} (idle timeout)",
                self.timeouts.idle
            ),
            Kind::Total => format!(
                "{label}: generation exceeded {:?} (total timeout)",
                self.timeouts.total
            ),
        })
    }

    /// 送信して応答ヘッダを待つ（最初の応答のタイムアウト）。
    pub(crate) async fn send(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, LlmError> {
        if self.expired() {
            return Err(self.timed_out(Kind::Total));
        }
        self.within(
            self.timeouts.first_response,
            Kind::FirstResponse,
            builder.send(),
        )
        .await?
        .map_err(|e| LlmError::Unavailable(format!("{} request failed: {e}", self.label)))
    }

    /// `fut` を `limit`（全体の残りで頭打ち）以内に待つ。
    ///
    /// `tokio::time::timeout` は中の future を先に poll するので、期限ちょうどに両方が
    /// ready になると期限後の結果も `Ok` で返る。待ち終えた時点でも期限を判定し直し、
    /// 過ぎていれば結果を捨ててタイムアウトにする（期限後に届いた完了を正常扱いしない）。
    async fn within<F: Future>(
        &self,
        limit: Duration,
        kind: Kind,
        fut: F,
    ) -> Result<F::Output, LlmError> {
        let (limit, kind) = self.bounded(limit, kind);
        let wait_deadline = Instant::now() + limit;
        match tokio::time::timeout(limit, fut).await {
            Ok(_) if self.expired() => Err(self.timed_out(Kind::Total)),
            Ok(_) if Instant::now() > wait_deadline => Err(self.timed_out(kind)),
            Ok(out) => Ok(out),
            Err(_) => Err(self.timed_out(kind)),
        }
    }

    /// エラー応答の本文を読む（無通信・全体の残りで頭打ち）。読み取りに失敗したら空。
    ///
    /// 本文が止まったときはタイムアウトのエラーを返す。HTTP ステータスで分類すると、
    /// 4xx の `BadRequest` などになり、タイムアウトだったことが失われるため。
    pub(crate) async fn error_text(&self, resp: reqwest::Response) -> Result<String, LlmError> {
        Ok(self
            .within(self.timeouts.idle, Kind::Idle, resp.text())
            .await?
            .unwrap_or_default())
    }

    /// 中立形のストリームに無通信・全体の上限を掛ける（HTTP を介さないスタブ用）。
    ///
    /// 差分と差分の間が無通信の上限を超えるか、全体の締め切りを過ぎたら、実プロバイダと同じ
    /// タイムアウトのエラーを 1 つ流して終える。
    pub(crate) fn guard(self, inner: DeltaStream) -> DeltaStream {
        futures::stream::unfold(Some(inner), move |state| async move {
            let mut inner = state?;
            if self.expired() {
                return Some((Err(self.timed_out(Kind::Total)), None));
            }
            match self
                .within(self.timeouts.idle, Kind::Idle, inner.next())
                .await
            {
                Ok(Some(item)) => Some((item, Some(inner))),
                Ok(None) => None,
                Err(e) => Some((Err(e), None)),
            }
        })
        .boxed()
    }

    /// 本文の次のチャンクを待つ（無通信のタイムアウト）。本文の終端と、受け手（`tx` の
    /// 受信側）が閉じた場合は `None`。
    ///
    /// 受け手が `DeltaStream` を捨てたら、待っている最中でも読むのをやめる。プロバイダが
    /// 黙っていても、無通信・全体の上限まで接続と生成を握り続けない。ただし利用側
    /// （チャット・エージェント）は停止フラグを次の差分を受け取ってから見るので、黙っている
    /// 間の停止はここまで届かない（無通信の上限で切れる）。
    pub(crate) async fn next_chunk<S, B, T>(
        &self,
        body: &mut S,
        tx: &UnboundedSender<T>,
    ) -> Option<Result<B, LlmError>>
    where
        S: Stream<Item = reqwest::Result<B>> + Unpin,
    {
        if tx.is_closed() {
            return None;
        }
        // 締め切り後は、既に届いているチャンク（完了イベントを含みうる）も受け取らない。
        // `tokio::time::timeout` は 0 秒でも中の future を先に poll するので、ここで先に判定する。
        if self.expired() {
            return Some(Err(self.timed_out(Kind::Total)));
        }
        // futures の mpsc は「閉じた」を通知しないので、待ちのあいだ短い間隔で見る。
        let abandoned = async {
            loop {
                tokio::time::sleep(ABANDON_POLL).await;
                if tx.is_closed() {
                    return;
                }
            }
        };
        tokio::select! {
            read = self.within(self.timeouts.idle, Kind::Idle, body.next()) => match read {
                Ok(None) => None,
                Ok(Some(Ok(chunk))) => Some(Ok(chunk)),
                Ok(Some(Err(e))) => Some(Err(LlmError::Unavailable(format!("stream: {e}")))),
                Err(e) => Some(Err(e)),
            },
            () = abandoned => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    fn clock(first: u64, idle: u64, total: u64) -> RequestClock {
        RequestClock::start(
            LlmTimeouts {
                first_response: Duration::from_millis(first),
                idle: Duration::from_millis(idle),
                total: Duration::from_millis(total),
            },
            "test",
        )
    }

    #[tokio::test]
    async fn idle_wait_is_capped_by_the_remaining_total() {
        let c = clock(1_000, 1_000, 300);
        let (tx, _rx) = open_tx();
        let mut never = stream::pending::<reqwest::Result<&[u8]>>();
        let err = c.next_chunk(&mut never, &tx).await.unwrap_err_msg();
        assert!(err.contains("total timeout"), "{err}");
    }

    #[tokio::test]
    async fn idle_timeout_names_itself() {
        let c = clock(1_000, 100, 10_000);
        let (tx, _rx) = open_tx();
        let mut never = stream::pending::<reqwest::Result<&[u8]>>();
        let err = c.next_chunk(&mut never, &tx).await.unwrap_err_msg();
        assert!(err.contains("idle timeout"), "{err}");
    }

    #[tokio::test]
    async fn huge_total_does_not_overflow() {
        let (tx, _rx) = open_tx();
        let c = RequestClock::start(
            LlmTimeouts {
                first_response: Duration::from_millis(1_000),
                idle: Duration::from_millis(50),
                total: Duration::MAX,
            },
            "test",
        );
        let mut never = stream::pending::<reqwest::Result<&[u8]>>();
        let err = c.next_chunk(&mut never, &tx).await.unwrap_err_msg();
        assert!(err.contains("idle timeout"), "{err}");
    }

    #[tokio::test]
    async fn ready_chunks_after_the_deadline_are_refused() {
        let c = clock(1_000, 1_000, 0);
        let (tx, _rx) = open_tx();
        // 既に届いているチャンクでも、締め切り後は全体のタイムアウトにする。
        let mut body = stream::iter(vec![Ok(&b"done"[..])]);
        let err = c.next_chunk(&mut body, &tx).await.unwrap_err_msg();
        assert!(err.contains("total timeout"), "{err}");
    }

    /// 50ms ごとに差分を出し続けるストリーム。
    fn paced() -> DeltaStream {
        stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Some((
                Ok(crate::model::StreamDelta::TextDelta { text: "x".into() }),
                (),
            ))
        })
        .boxed()
    }

    async fn first_error(mut s: DeltaStream) -> (usize, String) {
        let mut ok = 0;
        while let Some(item) = s.next().await {
            match item {
                Ok(_) => ok += 1,
                Err(LlmError::Unavailable(m)) => {
                    assert!(s.next().await.is_none(), "stream must end after the error");
                    return (ok, m);
                }
                Err(e) => panic!("expected Unavailable, got {e:?}"),
            }
        }
        panic!("stream ended without an error");
    }

    #[tokio::test]
    async fn guard_cuts_a_stream_that_keeps_flowing_past_the_total() {
        let (ok, msg) = first_error(clock(1_000, 1_000, 300).guard(paced())).await;
        assert!(ok >= 1, "{ok}");
        assert!(msg.contains("total timeout"), "{msg}");
    }

    #[tokio::test]
    async fn guard_cuts_a_silent_stream_at_the_idle_limit() {
        let silent = stream::pending().boxed();
        let (ok, msg) = first_error(clock(1_000, 100, 10_000).guard(silent)).await;
        assert_eq!(ok, 0);
        assert!(msg.contains("idle timeout"), "{msg}");
    }

    #[tokio::test]
    async fn chunks_and_end_pass_through() {
        let c = clock(1_000, 100, 10_000);
        let (tx, _rx) = open_tx();
        let mut body = stream::iter(vec![Ok(&b"a"[..])]);
        assert_eq!(
            c.next_chunk(&mut body, &tx).await.map(|r| r.ok()),
            Some(Some(&b"a"[..]))
        );
        assert!(c.next_chunk(&mut body, &tx).await.is_none());
    }

    /// 受け手が開いたままの組（受信側を持っている間は閉じない）。
    fn open_tx() -> (
        UnboundedSender<()>,
        futures::channel::mpsc::UnboundedReceiver<()>,
    ) {
        futures::channel::mpsc::unbounded()
    }

    #[tokio::test]
    async fn closed_receiver_stops_waiting_for_a_silent_body() {
        let c = clock(1_000, 60_000, 120_000);
        let (tx, rx) = futures::channel::mpsc::unbounded::<()>();
        let mut never = stream::pending::<reqwest::Result<&[u8]>>();
        let started = std::time::Instant::now();
        let closer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(rx);
        });
        // 無通信 60 秒を待たず、受け手が閉じた時点（＋見る間隔）で終わる。
        assert!(c.next_chunk(&mut never, &tx).await.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
        closer.await.unwrap();
        // 閉じた後は待たずに終わる。
        assert!(c.next_chunk(&mut never, &tx).await.is_none());
    }

    trait ErrMsg {
        fn unwrap_err_msg(self) -> String;
    }

    impl<B: std::fmt::Debug> ErrMsg for Option<Result<B, LlmError>> {
        fn unwrap_err_msg(self) -> String {
            match self {
                Some(Err(LlmError::Unavailable(m))) => m,
                other => panic!("expected Unavailable, got {other:?}"),
            }
        }
    }
}
