use futures::StreamExt;

use super::*;
use crate::model::{StopReason, Usage};

/// プロバイダが返すもの（接続の結果と、流れるデルタ）。
type Script = fn() -> Result<Vec<Result<StreamDelta, LlmError>>, LlmError>;

/// 決まった結果を返すプロバイダ。
struct Fixed(Script);

#[async_trait::async_trait]
impl LlmProvider for Fixed {
    fn name(&self) -> &'static str {
        "fixed"
    }
    async fn stream(&self, _req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        Ok(Box::pin(futures::stream::iter((self.0)()?)))
    }
}

fn text() -> Result<StreamDelta, LlmError> {
    Ok(StreamDelta::TextDelta { text: "x".into() })
}

fn done() -> Result<StreamDelta, LlmError> {
    Ok(StreamDelta::Done {
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
    })
}

fn pending(h: &HealthRecorder) -> Bucket {
    let p = h.pending.lock().unwrap();
    let mut sum = Bucket::default();
    for b in p.values() {
        sum.ok += b.ok;
        sum.failed += b.failed;
        sum.rate_limited += b.rate_limited;
        for (s, n) in sum.ttft.iter_mut().zip(b.ttft) {
            *s += n;
        }
    }
    sum
}

async fn run(f: Script) -> Bucket {
    let h = Arc::new(HealthRecorder::new());
    let p = Monitored::wrap(Arc::new(Fixed(f)), "p", Arc::clone(&h));
    if let Ok(s) = p.stream(&GenerateRequest::new(Vec::new())).await {
        let _ = s.collect::<Vec<_>>().await;
    }
    pending(&h)
}

fn ttft_total(b: &Bucket) -> i64 {
    b.ttft.iter().sum()
}

#[tokio::test]
async fn completed_stream_counts_ok_with_ttft() {
    let b = run(|| Ok(vec![text(), done()])).await;
    assert_eq!((b.ok, b.failed, b.rate_limited), (1, 0, 0));
    assert_eq!(ttft_total(&b), 1);
}

#[tokio::test]
async fn connect_errors_are_classified() {
    let b = run(|| Err(LlmError::Unavailable("503".into()))).await;
    assert_eq!((b.ok, b.failed, b.rate_limited), (0, 1, 0));
    assert_eq!(ttft_total(&b), 0);
    let b = run(|| Err(LlmError::RateLimited("429".into()))).await;
    assert_eq!((b.ok, b.failed, b.rate_limited), (0, 0, 1));
    // 呼び出し側の不備はプロバイダの障害として数えない。
    let b = run(|| {
        Err(LlmError::BadRequest(
            "openai 400 Bad Request: bad tool".into(),
        ))
    })
    .await;
    assert_eq!(b, Bucket::default());
    // キーの失効・残高切れ・接続先の誤りは、どの呼び出しも通らないので障害として数える。
    let b = run(|| {
        Err(LlmError::BadRequest(
            "anthropic 401 Unauthorized: invalid x-api-key".into(),
        ))
    })
    .await;
    assert_eq!((b.ok, b.failed), (0, 1));
    let b = run(|| {
        Err(LlmError::BadRequest(
            "responses 404 Not Found: no such model".into(),
        ))
    })
    .await;
    assert_eq!((b.ok, b.failed), (0, 1));
    let b = run(|| Err(LlmError::Config("no key".into()))).await;
    assert_eq!(b, Bucket::default());
}

#[tokio::test]
async fn midstream_cut_counts_once_as_failed() {
    let b = run(|| {
        Ok(vec![
            text(),
            Err(LlmError::Unavailable("cut".into())),
            text(),
        ])
    })
    .await;
    assert_eq!((b.ok, b.failed, b.rate_limited), (0, 1, 0));
    assert_eq!(ttft_total(&b), 1);
}

#[tokio::test]
async fn caller_abort_counts_ok_only_after_first_delta() {
    let h = Arc::new(HealthRecorder::new());
    let p = Monitored::wrap(
        Arc::new(Fixed(|| Ok(vec![text(), text(), done()]))),
        "p",
        Arc::clone(&h),
    );
    // 最初のデルタの後に打ち切る → 正常として 1 回。
    let mut s = p.stream(&GenerateRequest::new(Vec::new())).await.unwrap();
    let _ = s.next().await;
    drop(s);
    assert_eq!(pending(&h).ok, 1);
    // 何も読まずに捨てる → 数えない。
    drop(p.stream(&GenerateRequest::new(Vec::new())).await.unwrap());
    assert_eq!(pending(&h).ok, 1);
}

#[test]
fn ttft_buckets_follow_bounds() {
    assert_eq!(ttft_bucket(Duration::from_millis(0)), 0);
    assert_eq!(ttft_bucket(Duration::from_millis(250)), 0);
    assert_eq!(ttft_bucket(Duration::from_millis(251)), 1);
    assert_eq!(
        ttft_bucket(Duration::from_secs(64)),
        TTFT_BOUNDS_MS.len() - 1
    );
    assert_eq!(ttft_bucket(Duration::from_secs(65)), TTFT_BOUNDS_MS.len());
}

#[test]
fn restore_adds_back_to_counts_taken_meanwhile() {
    let h = HealthRecorder::new();
    h.record("p", Outcome::Ok, Some(Duration::from_millis(100)));
    let taken: Vec<_> = h.pending.lock().unwrap().drain().collect();
    h.record("p", Outcome::Failed, None);
    h.restore(taken.into_iter());
    let b = pending(&h);
    assert_eq!((b.ok, b.failed), (1, 1));
    assert_eq!(b.ttft[0], 1);
}
