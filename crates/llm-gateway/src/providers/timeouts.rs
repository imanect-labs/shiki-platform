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

use std::time::Duration;

use futures::{Stream, StreamExt};
use tokio::time::Instant;

use crate::config::LlmTimeouts;
use crate::provider::LlmError;

/// どのタイムアウトで切れたか（メッセージ用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    FirstResponse,
    Idle,
    Total,
}

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
        let (limit, kind) = self.bounded(self.timeouts.first_response, Kind::FirstResponse);
        match tokio::time::timeout(limit, builder.send()).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Err(LlmError::Unavailable(format!(
                "{} request failed: {e}",
                self.label
            ))),
            Err(_) => Err(self.timed_out(kind)),
        }
    }

    /// エラー応答の本文を読む（無通信・全体の残りで頭打ち。読めなければ空）。
    pub(crate) async fn error_text(&self, resp: reqwest::Response) -> String {
        let (limit, _) = self.bounded(self.timeouts.idle, Kind::Idle);
        match tokio::time::timeout(limit, resp.text()).await {
            Ok(Ok(text)) => text,
            Ok(Err(_)) | Err(_) => String::new(),
        }
    }

    /// 本文の次のチャンクを待つ（無通信のタイムアウト）。終端は `None`。
    pub(crate) async fn next_chunk<S, B>(&self, body: &mut S) -> Option<Result<B, LlmError>>
    where
        S: Stream<Item = reqwest::Result<B>> + Unpin,
    {
        let (limit, kind) = self.bounded(self.timeouts.idle, Kind::Idle);
        match tokio::time::timeout(limit, body.next()).await {
            Ok(None) => None,
            Ok(Some(Ok(chunk))) => Some(Ok(chunk)),
            Ok(Some(Err(e))) => Some(Err(LlmError::Unavailable(format!("stream: {e}")))),
            Err(_) => Some(Err(self.timed_out(kind))),
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
        let mut never = stream::pending::<reqwest::Result<&[u8]>>();
        let err = c.next_chunk(&mut never).await.unwrap_err_msg();
        assert!(err.contains("total timeout"), "{err}");
    }

    #[tokio::test]
    async fn idle_timeout_names_itself() {
        let c = clock(1_000, 100, 10_000);
        let mut never = stream::pending::<reqwest::Result<&[u8]>>();
        let err = c.next_chunk(&mut never).await.unwrap_err_msg();
        assert!(err.contains("idle timeout"), "{err}");
    }

    #[tokio::test]
    async fn huge_total_does_not_overflow() {
        let c = RequestClock::start(
            LlmTimeouts {
                first_response: Duration::from_millis(1_000),
                idle: Duration::from_millis(50),
                total: Duration::MAX,
            },
            "test",
        );
        let mut never = stream::pending::<reqwest::Result<&[u8]>>();
        let err = c.next_chunk(&mut never).await.unwrap_err_msg();
        assert!(err.contains("idle timeout"), "{err}");
    }

    #[tokio::test]
    async fn chunks_and_end_pass_through() {
        let c = clock(1_000, 100, 10_000);
        let mut body = stream::iter(vec![Ok(&b"a"[..])]);
        assert_eq!(
            c.next_chunk(&mut body).await.map(|r| r.ok()),
            Some(Some(&b"a"[..]))
        );
        assert!(c.next_chunk(&mut body).await.is_none());
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
