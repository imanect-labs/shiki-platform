//! プロバイダの一時障害の送り直し（#743・#897）。
//!
//! - **接続前**（最初のバイト前）の `Unavailable` は、待ってから同じ要求を送り直す。何も生成
//!   されていないので二重にならない。
//! - **流れ始めた後**に切れたストリームも、まだ何も確定していない（本文・ツール呼び出し・推論
//!   アイテムを 1 つも出していない）うちなら、同じ要求で送り直して続きとして流す。それまでに
//!   流れたのは思考だけなので、エージェントの履歴には何も残っていない。Gemini API の Gemma は
//!   200 で流し始めた後に思考の途中で 500 を返して切ることがあり、送り直さないと 1 回の失敗で
//!   会話全体がやり直しになる（やり直しは適用済みのツールを二重に実行しうる・#889）。

use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};

use crate::model::{GenerateRequest, StreamDelta};
use crate::provider::{DeltaStream, LlmError, LlmProvider};

/// 接続前の `Unavailable` を送り直すまでの待ち時間（試行の間ごと）。Gemini API の
/// gemma-4-31b-it は 500 を数回続けて返すことがあり、2 回では足りなかった（実測で 3〜4 回続く）。
pub(crate) const UNAVAILABLE_BACKOFF: [Duration; 4] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

/// 流れ始めた後に切れたストリームを送り直す回数。
pub(crate) const MIDSTREAM_RETRIES: usize = 2;

/// `backoff` の数だけ、接続確立前の Unavailable を待ってから送り直す。
pub(crate) async fn stream_with_retry(
    provider: &dyn LlmProvider,
    req: &GenerateRequest,
    backoff: &[Duration],
) -> Result<DeltaStream, LlmError> {
    let mut waits = backoff.iter();
    loop {
        match provider.stream(req).await {
            Err(LlmError::Unavailable(e)) => {
                let Some(wait) = waits.next() else {
                    return Err(LlmError::Unavailable(e));
                };
                tracing::warn!(error = %e, ?wait, "llm provider unavailable, retrying");
                tokio::time::sleep(*wait).await;
            }
            other => return other,
        }
    }
}

/// このデルタを出した後は送り直せない（エージェントの履歴・画面の本文に残る）か。
fn settles(delta: &StreamDelta) -> bool {
    match delta {
        StreamDelta::TextDelta { text } => !text.is_empty(),
        StreamDelta::ThinkingDelta { .. } => false,
        StreamDelta::ToolUseStart { .. }
        | StreamDelta::ToolUseInputDelta { .. }
        | StreamDelta::ToolUseStop { .. }
        | StreamDelta::ReasoningItem { .. }
        | StreamDelta::Done { .. } => true,
    }
}

struct Resume {
    provider: Arc<dyn LlmProvider>,
    req: GenerateRequest,
    current: Option<DeltaStream>,
    settled: bool,
    thought: bool,
    left: usize,
}

/// まだ何も確定していないうちに `Unavailable` で切れたストリームを、同じ要求で送り直して
/// 続ける（最大 `retries` 回）。確定した後の失敗や、送り直しも尽きた失敗はそのまま流す。
pub(crate) fn resume_unsettled(
    provider: Arc<dyn LlmProvider>,
    req: GenerateRequest,
    first: DeltaStream,
    retries: usize,
) -> DeltaStream {
    let state = Resume {
        provider,
        req,
        current: Some(first),
        settled: false,
        thought: false,
        left: retries,
    };
    stream::unfold(state, |mut st| async move {
        loop {
            let item = st.current.as_mut()?.next().await;
            match item {
                None => return None,
                Some(Err(LlmError::Unavailable(e))) if !st.settled && st.left > 0 => {
                    st.left -= 1;
                    st.current = None;
                    tracing::warn!(error = %e, "LLM の応答が途中で切れた（まだ何も確定していないので送り直す）");
                    match stream_with_retry(st.provider.as_ref(), &st.req, &UNAVAILABLE_BACKOFF).await
                    {
                        Ok(s) => {
                            st.current = Some(s);
                            // 前の試行の思考と、送り直した試行の思考を区切る。
                            if st.thought {
                                let sep = StreamDelta::ThinkingDelta { text: "\n\n".into() };
                                return Some((Ok(sep), st));
                            }
                        }
                        Err(e) => return Some((Err(e), st)),
                    }
                }
                Some(item) => {
                    if let Ok(d) = &item {
                        st.settled |= settles(d);
                        st.thought |= matches!(d, StreamDelta::ThinkingDelta { .. });
                    }
                    return Some((item, st));
                }
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{StopReason, Usage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// 最初の `fail` 回は `err` を返し、その後は空のストリームを返す。
    struct Flaky {
        fail: usize,
        err: fn() -> LlmError,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for Flaky {
        fn name(&self) -> &'static str {
            "flaky"
        }
        async fn stream(&self, _req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail {
                return Err((self.err)());
            }
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn unavailable_before_first_byte_is_retried_as_many_times_as_backoff_allows() {
        let backoff = [Duration::from_millis(1); 2];
        let req = GenerateRequest::new(Vec::new());
        let unavailable = || LlmError::Unavailable("500".into());
        let calls = |p: &Flaky| p.calls.load(Ordering::SeqCst);

        let p = Flaky {
            fail: 2,
            err: unavailable,
            calls: 0.into(),
        };
        assert!(stream_with_retry(&p, &req, &backoff).await.is_ok());
        assert_eq!(calls(&p), 3);

        let p = Flaky {
            fail: 3,
            err: unavailable,
            calls: 0.into(),
        };
        assert!(matches!(
            stream_with_retry(&p, &req, &backoff).await,
            Err(LlmError::Unavailable(_))
        ));
        assert_eq!(calls(&p), 3);

        // 直せば通るたぐいの誤りは送り直さない。
        let p = Flaky {
            fail: 1,
            err: || LlmError::BadRequest("400".into()),
            calls: 0.into(),
        };
        assert!(matches!(
            stream_with_retry(&p, &req, &backoff).await,
            Err(LlmError::BadRequest(_))
        ));
        assert_eq!(calls(&p), 1);
    }

    type Script = Vec<Result<StreamDelta, LlmError>>;

    /// 呼ばれるたびに、用意した台本を順に流す（台本が尽きたら空）。
    struct Scripted {
        scripts: Mutex<Vec<Script>>,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        async fn stream(&self, _req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut scripts = self.scripts.lock().unwrap();
            let script = if scripts.is_empty() {
                Vec::new()
            } else {
                scripts.remove(0)
            };
            Ok(futures::stream::iter(script).boxed())
        }
    }

    fn think(t: &str) -> Result<StreamDelta, LlmError> {
        Ok(StreamDelta::ThinkingDelta { text: t.into() })
    }
    fn text(t: &str) -> Result<StreamDelta, LlmError> {
        Ok(StreamDelta::TextDelta { text: t.into() })
    }
    fn cut() -> Result<StreamDelta, LlmError> {
        Err(LlmError::Unavailable("stream ended with: 500".into()))
    }
    fn done() -> Result<StreamDelta, LlmError> {
        Ok(StreamDelta::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        })
    }

    /// 1 回目の台本を最初のストリームとして渡し、残りを送り直しで流す。
    async fn run(scripts: Vec<Script>, retries: usize) -> (Vec<String>, usize) {
        let mut scripts = scripts;
        let first: DeltaStream = futures::stream::iter(scripts.remove(0)).boxed();
        let provider = Arc::new(Scripted {
            scripts: Mutex::new(scripts),
            calls: 0.into(),
        });
        let out = resume_unsettled(
            provider.clone(),
            GenerateRequest::new(Vec::new()),
            first,
            retries,
        )
        .map(|d| match d {
            Ok(StreamDelta::ThinkingDelta { text }) => format!("think:{text}"),
            Ok(StreamDelta::TextDelta { text }) => format!("text:{text}"),
            Ok(StreamDelta::Done { .. }) => "done".into(),
            Ok(other) => format!("{other:?}"),
            Err(e) => format!("err:{e}"),
        })
        .collect::<Vec<_>>()
        .await;
        (out, provider.calls.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_stream_cut_while_only_thinking_is_sent_again() {
        let (out, calls) = run(
            vec![
                vec![think("考え中"), cut()],
                vec![think("考え直し"), text("答え"), done()],
            ],
            2,
        )
        .await;
        assert_eq!(calls, 1);
        assert_eq!(
            out,
            [
                "think:考え中",
                "think:\n\n",
                "think:考え直し",
                "text:答え",
                "done"
            ]
        );
    }

    #[tokio::test]
    async fn a_stream_cut_after_the_answer_started_is_not_sent_again() {
        let (out, calls) = run(vec![vec![text("答えの途中"), cut()], vec![done()]], 2).await;
        assert_eq!(calls, 0, "本文を出した後は送り直さない（二重になる）");
        assert_eq!(out[0], "text:答えの途中");
        assert!(out[1].starts_with("err:"), "{out:?}");
    }

    #[tokio::test]
    async fn retries_are_bounded() {
        let (out, calls) = run(vec![vec![cut()], vec![cut()], vec![cut()], vec![done()]], 2).await;
        assert_eq!(calls, 2);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].starts_with("err:"));
    }
}
