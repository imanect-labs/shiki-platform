//! jobq のトレース連携の結合テスト（実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` 未設定ならスキップ（`SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! 検証: enqueue 元の span の context が `traceparent` として保存され、claim → 再試行（fail・
//! defer）→ DLQ → requeue_dead を通しても同じ context が返り、ワーカーの span が enqueue 元の
//! トレースにつながる。`traceparent` の無い既存行でも claim・span 作成が落ちない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::time::Duration;

use jobq::{FailOutcome, JobSpanLink, NewJob};
use opentelemetry::trace::{SpanId, TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn unique_queue(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// OTel の layer を付けた subscriber をこのスレッドの既定にする（current_thread ランタイム前提）。
fn otel() -> (
    InMemorySpanExporter,
    SdkTracerProvider,
    tracing::subscriber::DefaultGuard,
) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("it")));
    let guard = tracing::subscriber::set_default(subscriber);
    (exporter, provider, guard)
}

async fn enqueue(pool: &PgPool, queue: &str, trace_id: Option<&str>, max_attempts: i32) -> i64 {
    let mut conn = pool.acquire().await.unwrap();
    jobq::enqueue_on(
        &mut conn,
        NewJob {
            queue,
            tenant_id: "a-corp",
            payload: &serde_json::json!({"k": "v"}),
            trace_id,
            max_attempts,
        },
    )
    .await
    .unwrap()
}

async fn claim_one(pool: &PgPool, queue: &str) -> jobq::ClaimedJob {
    let mut conn = pool.acquire().await.unwrap();
    let mut jobs = jobq::claim(&mut conn, queue, Duration::from_secs(300), 1)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "可視なジョブが 1 件あること");
    jobs.pop().unwrap()
}

/// 可視期限を今に戻す（バックオフ・先送りを待たずに再 claim するため）。
async fn make_visible(pool: &PgPool, id: i64) {
    sqlx::query("update job_queue set visible_at = now() where id = $1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn traceparent_survives_retry_defer_dead_and_requeue() {
    let Some(pool) = setup().await else { return };
    let (exporter, provider, _guard) = otel();
    let queue = unique_queue("trace");

    // API のリクエスト span の中で積む（trace_id は監査用に同じ値を渡す＝チャットの発話と同じ形）。
    let request = tracing::info_span!("http_request");
    let request_cx = request.context().span().span_context().clone();
    let trace_hex = request_cx.trace_id().to_string();
    let id = {
        let _g = request.enter();
        enqueue(&pool, &queue, Some(&trace_hex), 2).await
    };
    drop(request);

    let expected = format!("00-{}-{}-01", request_cx.trace_id(), request_cx.span_id());
    let job = claim_one(&pool, &queue).await;
    assert_eq!(job.traceparent.as_deref(), Some(expected.as_str()));
    assert_eq!(job.trace_id.as_deref(), Some(trace_hex.as_str()));

    // 先送り → 再 claim でも同じ context。
    let mut conn = pool.acquire().await.unwrap();
    jobq::defer(&mut conn, id, Duration::from_secs(60))
        .await
        .unwrap();
    make_visible(&pool, id).await;
    let job = claim_one(&pool, &queue).await;
    assert_eq!(job.traceparent.as_deref(), Some(expected.as_str()));

    // 失敗（再試行）→ 再 claim でも同じ context。
    let outcome = jobq::fail(&mut conn, id, "boom", Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(outcome, FailOutcome::Retry { attempts: 1 });
    make_visible(&pool, id).await;
    let job = claim_one(&pool, &queue).await;
    assert_eq!(job.attempts, 2);
    assert_eq!(job.traceparent.as_deref(), Some(expected.as_str()));

    // ワーカーの span（親子）は enqueue 元のトレースに入る。
    job.span(JobSpanLink::Parent).in_scope(|| {});

    // 上限で DLQ へ → requeue_dead → 同じ context。
    let outcome = jobq::fail(&mut conn, id, "boom", Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(outcome, FailOutcome::Dead);
    let dead_tp: Option<String> =
        sqlx::query_scalar("select traceparent from job_queue_dead where id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(dead_tp.as_deref(), Some(expected.as_str()));
    assert!(jobq::requeue_dead(&mut conn, id).await.unwrap());
    let job = claim_one(&pool, &queue).await;
    assert_eq!(job.traceparent.as_deref(), Some(expected.as_str()));
    let mut conn = pool.acquire().await.unwrap();
    jobq::ack(&mut conn, job.id).await.unwrap();

    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let worker = spans
        .iter()
        .find(|s| s.name.starts_with("job "))
        .expect("ジョブの span が出ること");
    assert_eq!(worker.span_context.trace_id(), request_cx.trace_id());
    assert_eq!(worker.parent_span_id, request_cx.span_id());
}

/// span の外で積んだ行・現在の span と違う trace_id で積み直した行は traceparent を持たない。
#[tokio::test]
async fn no_traceparent_outside_span_or_on_foreign_trace_id() {
    let Some(pool) = setup().await else { return };
    let (_exporter, _provider, _guard) = otel();
    let queue = unique_queue("trace-none");

    enqueue(&pool, &queue, None, 3).await;
    let job = claim_one(&pool, &queue).await;
    assert!(job.traceparent.is_none());

    // reaper のように、保存済みの（別トレースの）trace_id で積み直す。
    let sweeper = tracing::info_span!("sweeper");
    {
        let _g = sweeper.enter();
        enqueue(&pool, &queue, Some("0af7651916cd43dd8448eb211c80319c"), 3).await;
    }
    let job = claim_one(&pool, &queue).await;
    assert!(job.traceparent.is_none(), "親 span が分からない積み直し");
    assert_eq!(
        job.trace_id.as_deref(),
        Some("0af7651916cd43dd8448eb211c80319c")
    );
}

/// 列の追加前に積まれた行（traceparent が null）でもワーカーが落ちない。
#[tokio::test]
async fn legacy_rows_without_traceparent_still_work() {
    let Some(pool) = setup().await else { return };
    let (exporter, provider, _guard) = otel();
    let queue = unique_queue("trace-legacy");
    // 旧版の insert（traceparent 列を知らない）。
    sqlx::query(
        "insert into job_queue (queue, tenant_id, payload, max_attempts, trace_id) \
         values ($1, 'a-corp', '{}'::jsonb, 3, 'legacy-trace')",
    )
    .bind(&queue)
    .execute(&pool)
    .await
    .unwrap();
    let job = claim_one(&pool, &queue).await;
    assert!(job.traceparent.is_none());
    job.span(JobSpanLink::Parent).in_scope(|| {});
    job.span(JobSpanLink::Link).in_scope(|| {});
    let mut conn = pool.acquire().await.unwrap();
    jobq::ack(&mut conn, job.id).await.unwrap();

    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2);
    for s in &spans {
        assert_eq!(s.parent_span_id, SpanId::INVALID);
        assert!(s.links.is_empty());
    }
}
