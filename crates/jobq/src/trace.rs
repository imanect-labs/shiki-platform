//! ジョブの実行を enqueue 元のトレースにつなげる。
//!
//! - 投入時（[`crate::enqueue_on`]）: 現在の span の OTel context を W3C `traceparent`
//!   （`00-<trace_id>-<span_id>-<flags>`）にして `job_queue.traceparent` へ保存する
//!   （[`current_traceparent`]）。呼び出し側は何も渡さなくてよい。
//! - 実行時: ワーカーは claim 後に [`ClaimedJob::span`] で span を作り、処理をその中で走らせる。
//!   保存した context を**親**にするか**リンク**にするかはワーカーが [`JobSpanLink`] で選ぶ。
//!
//! 再試行（`fail`・`defer`）は同じ行を使い回すので、何度 claim しても同じ context につながる。
//! DLQ へ移しても列ごと引き継ぎ、`requeue_dead` で戻せば元のトレースへつながる。
//!
//! **続きのジョブ**（一括取込・バックフィルの積み直し、outbox から中継するインジェスト）は、積む時点の
//! span が元の操作ではない。[`continue_trace`] で元の context を指定して積むと、それを引き継ぐ。
//!
//! `traceparent` が無い行（列の追加前に積まれた行・span の外で積まれた行）や壊れた値でも
//! 落ちない: 親もリンクも無い span になるだけ。

use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use opentelemetry::Context;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::ClaimedJob;

/// ジョブの span を enqueue 元の span とどうつなぐか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobSpanLink {
    /// enqueue 元の span の**子**にする（同じトレースに入る）。ユーザーが結果を待っている
    /// 短いジョブ（チャット生成）向け。
    Parent,
    /// 新しいトレースを始め、enqueue 元へ **span link** を張る。長時間のバッチ・バックフィル・
    /// 保守ジョブ向け（元のリクエストのトレースが何十分も伸びないように）。
    Link,
}

impl JobSpanLink {
    /// ユーザーが待っているジョブ向け: 初回（`attempts == 1`）は親子、再試行はリンク。
    ///
    /// バックオフ後の再試行を親子にすると、元のリクエストのトレースが何十分も伸びる。
    /// `defer`（順番待ち）は attempts を消費しないので、先送り明けも初回のまま親子になる。
    #[must_use]
    pub fn parent_unless_retry(attempts: i32) -> Self {
        if attempts > 1 {
            Self::Link
        } else {
            Self::Parent
        }
    }
}

/// 現在の span の OTel context を W3C `traceparent` にする。有効な context が無ければ `None`。
///
/// `trace_id` が指定されていて現在の span の trace と違うとき（保存済みの trace_id で積み直す
/// reaper・outbox の relay など、現在の span が enqueue の「元」ではない場合）も `None`:
/// 親 span が分からないまま別のトレースへつなぐと、因果の違う span が混ざる。
#[must_use]
pub fn current_traceparent(trace_id: Option<&str>) -> Option<String> {
    let cx = tracing::Span::current().context();
    let sc = cx.span().span_context().clone();
    if !sc.is_valid() {
        return None;
    }
    let tp = format_traceparent(&sc);
    if let Some(expected) = trace_id {
        if !expected.eq_ignore_ascii_case(&sc.trace_id().to_string()) {
            return None;
        }
    }
    Some(tp)
}

tokio::task_local! {
    /// [`continue_trace`] が指定した、積むジョブへ引き継ぐ context。
    static INHERITED: Option<String>;
}

/// `fut` の中で積むジョブに、現在の span ではなく `traceparent`（元のジョブ・outbox の行が持つ
/// context）を保存させる。`None` を渡したときは何も引き継がない（現在の span も使わない）。
///
/// 続きのジョブ・中継のように「積む時点の span が元の操作ではない」ところで使う。元の操作が
/// 親子なら続きも同じトレースへ、リンクなら同じ元へのリンクになる（続きのたびに鎖にならない）。
pub async fn continue_trace<F: std::future::Future>(
    traceparent: Option<&str>,
    fut: F,
) -> F::Output {
    INHERITED.scope(traceparent.map(str::to_string), fut).await
}

/// [`crate::enqueue_on`] が保存する値: [`continue_trace`] の中ならその値、外なら現在の span。
pub(crate) fn traceparent_for_enqueue(trace_id: Option<&str>) -> Option<String> {
    match INHERITED.try_with(Clone::clone) {
        Ok(inherited) => inherited,
        Err(_) => current_traceparent(trace_id),
    }
}

/// [`SpanContext`] を `traceparent` の文字列にする。
fn format_traceparent(sc: &SpanContext) -> String {
    format!(
        "00-{}-{}-{:02x}",
        sc.trace_id(),
        sc.span_id(),
        sc.trace_flags().to_u8()
    )
}

/// `traceparent` を [`SpanContext`]（remote）に戻す。形式が違う・ID が全 0 なら `None`。
#[must_use]
pub fn parse_traceparent(value: &str) -> Option<SpanContext> {
    let mut parts = value.trim().split('-');
    let (version, trace, span, flags) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || version != "00" {
        return None;
    }
    if trace.len() != 32 || span.len() != 16 || flags.len() != 2 {
        return None;
    }
    let trace_id = TraceId::from_hex(trace).ok()?;
    let span_id = SpanId::from_hex(span).ok()?;
    let flags = u8::from_str_radix(flags, 16).ok()?;
    let sc = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::new(flags) & TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    sc.is_valid().then_some(sc)
}

impl ClaimedJob {
    /// このジョブを処理する span を作る。処理は `.instrument(span)` か `span.in_scope` で包む。
    ///
    /// 属性: キュー名・ジョブ id・試行回数・テナント・待ち時間（enqueue から claim まで。どちらも
    /// DB の時計で、span を作るまでの処理時間は含めない。再試行では最初の enqueue からの経過）。
    ///
    /// span は**呼び出し時点の現在の span を親に取らない**（`parent: None`）。ワーカーのループが
    /// span の中で動いていても、親子モードなら enqueue 元の子、リンクモードなら独立したトレースになる。`traceparent` が無く `trace_id` だけある行は、
    /// 突合用に `job.enqueue_trace_id` を載せる。
    #[must_use]
    pub fn span(&self, link: JobSpanLink) -> tracing::Span {
        let wait_ms = (self.claimed_at - self.enqueued_at)
            .num_milliseconds()
            .max(0);
        let span = tracing::info_span!(
            parent: None,
            "job",
            otel.name = %format_args!("job {}", self.queue),
            job.queue = %self.queue,
            job.id = self.id,
            job.attempts = self.attempts,
            job.max_attempts = self.max_attempts,
            job.wait_ms = wait_ms,
            job.link = ?link,
            job.enqueue_trace_id = tracing::field::Empty,
            tenant_id = %self.tenant_id,
        );
        match self.traceparent.as_deref().and_then(parse_traceparent) {
            Some(sc) => match link {
                JobSpanLink::Parent => {
                    span.set_parent(Context::new().with_remote_span_context(sc));
                }
                JobSpanLink::Link => span.add_link(sc),
            },
            None => {
                if let Some(trace_id) = self.trace_id.as_deref() {
                    span.record("job.enqueue_trace_id", trace_id);
                }
            }
        }
        span
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use chrono::Utc;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing_subscriber::layer::SubscriberExt;

    const TP: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

    fn job(traceparent: Option<&str>, trace_id: Option<&str>) -> ClaimedJob {
        ClaimedJob {
            id: 7,
            queue: "chat_generation".into(),
            tenant_id: "a-corp".into(),
            payload: serde_json::json!({}),
            attempts: 2,
            max_attempts: 3,
            trace_id: trace_id.map(str::to_string),
            traceparent: traceparent.map(str::to_string),
            enqueued_at: claimed_at() - chrono::Duration::seconds(5),
            claimed_at: claimed_at(),
        }
    }

    /// 固定の claim 時刻（待ち時間を厳密に比べるため）。
    fn claimed_at() -> chrono::DateTime<Utc> {
        chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    /// テスト用の exporter を付けた subscriber の下で `f` を走らせ、終わった span を返す。
    fn capture(f: impl FnOnce()) -> Vec<opentelemetry_sdk::trace::SpanData> {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        tracing::subscriber::with_default(subscriber, f);
        provider.force_flush().unwrap();
        exporter.get_finished_spans().unwrap()
    }

    fn attr(span: &opentelemetry_sdk::trace::SpanData, key: &str) -> Option<String> {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    }

    #[test]
    fn retries_are_linked_not_parented() {
        assert_eq!(JobSpanLink::parent_unless_retry(0), JobSpanLink::Parent);
        assert_eq!(JobSpanLink::parent_unless_retry(1), JobSpanLink::Parent);
        assert_eq!(JobSpanLink::parent_unless_retry(2), JobSpanLink::Link);
        assert_eq!(JobSpanLink::parent_unless_retry(3), JobSpanLink::Link);
    }

    #[test]
    fn parse_roundtrips_and_rejects_garbage() {
        let sc = parse_traceparent(TP).unwrap();
        assert!(sc.is_remote());
        assert!(sc.is_sampled());
        assert_eq!(format_traceparent(&sc), TP);
        for bad in [
            "",
            "trace-1",
            "01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            "00-00000000000000000000000000000000-b7ad6b7169203331-01",
            "00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-zz",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-x",
        ] {
            assert!(parse_traceparent(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn parent_mode_joins_the_enqueue_trace() {
        let spans = capture(|| {
            let _g = job(Some(TP), None).span(JobSpanLink::Parent).entered();
        });
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.span_context.trace_id().to_string(), &TP[3..35]);
        assert_eq!(s.parent_span_id.to_string(), &TP[36..52]);
        assert!(s.links.is_empty());
        assert_eq!(s.name, "job chat_generation");
        assert_eq!(attr(s, "job.queue").as_deref(), Some("chat_generation"));
        assert_eq!(attr(s, "job.attempts").as_deref(), Some("2"));
        assert_eq!(attr(s, "tenant_id").as_deref(), Some("a-corp"));
        // 待ち時間は claim 時刻で確定する（span を作った時刻ではない）。
        assert_eq!(attr(s, "job.wait_ms").as_deref(), Some("5000"));
    }

    #[test]
    fn link_mode_starts_a_new_trace_with_a_link() {
        let spans = capture(|| {
            let _g = job(Some(TP), None).span(JobSpanLink::Link).entered();
        });
        let s = &spans[0];
        assert_ne!(s.span_context.trace_id().to_string(), &TP[3..35]);
        assert_eq!(s.parent_span_id, SpanId::INVALID);
        let links: Vec<_> = s.links.iter().collect();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].span_context.trace_id().to_string(), &TP[3..35]);
        assert_eq!(links[0].span_context.span_id().to_string(), &TP[36..52]);
    }

    /// ワーカーのループが span の中で動いていても、その span を暗黙の親に取らない。
    #[test]
    fn job_span_ignores_the_ambient_worker_span() {
        let spans = capture(|| {
            tracing::info_span!("worker_loop").in_scope(|| {
                job(Some(TP), None).span(JobSpanLink::Link).in_scope(|| {});
                job(Some(TP), None)
                    .span(JobSpanLink::Parent)
                    .in_scope(|| {});
                job(None, None).span(JobSpanLink::Parent).in_scope(|| {});
            });
        });
        let outer = spans.iter().find(|s| s.name == "worker_loop").unwrap();
        let jobs: Vec<_> = spans.iter().filter(|s| s.name != "worker_loop").collect();
        assert_eq!(jobs.len(), 3);
        for s in &jobs {
            assert_ne!(s.span_context.trace_id(), outer.span_context.trace_id());
            assert_ne!(s.parent_span_id, outer.span_context.span_id());
        }
        // リンク: 独立したトレースのルート。親子: enqueue 元の子。traceparent 無し: ルート。
        assert_eq!(jobs[0].parent_span_id, SpanId::INVALID);
        assert_eq!(jobs[0].links.len(), 1);
        assert_eq!(jobs[1].parent_span_id.to_string(), &TP[36..52]);
        assert_eq!(jobs[2].parent_span_id, SpanId::INVALID);
    }

    #[test]
    fn missing_or_broken_traceparent_still_makes_a_root_span() {
        let spans = capture(|| {
            job(None, Some("legacy-trace"))
                .span(JobSpanLink::Parent)
                .in_scope(|| {});
            job(Some("garbage"), None)
                .span(JobSpanLink::Link)
                .in_scope(|| {});
        });
        assert_eq!(spans.len(), 2);
        for s in &spans {
            assert_eq!(s.parent_span_id, SpanId::INVALID);
            assert!(s.links.is_empty());
        }
        assert_eq!(
            attr(&spans[0], "job.enqueue_trace_id").as_deref(),
            Some("legacy-trace")
        );
    }

    #[test]
    fn current_traceparent_reads_the_active_span() {
        let mut seen = None;
        let mut mismatched = Some(String::new());
        let mut outside = Some(String::new());
        capture(|| {
            outside = current_traceparent(None);
            let span = tracing::info_span!("http_request");
            let _g = span.enter();
            seen = current_traceparent(None);
            let tid = span.context().span().span_context().trace_id().to_string();
            assert_eq!(current_traceparent(Some(&tid)), seen);
            mismatched = current_traceparent(Some("0af7651916cd43dd8448eb211c80319c"));
        });
        assert!(outside.is_none());
        let seen = seen.unwrap();
        assert!(parse_traceparent(&seen).is_some(), "{seen}");
        assert!(mismatched.is_none());
    }
}
