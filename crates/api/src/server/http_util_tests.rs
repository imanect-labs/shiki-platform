//! リクエスト span（[`make_request_span`]・[`record_response`]）の検証。
//!
//! 実際の build と同じ順（`observe` を内側、TraceLayer を外側）でレイヤを掛けた小さな
//! ルータに投げ、メモリ上の exporter に出た span の親子・属性を確かめる。

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::get,
    Router,
};
use http_body_util::BodyExt;
use opentelemetry::{
    trace::{SpanId, Status, TraceId, TracerProvider as _},
    Value,
};
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    trace::{InMemorySpanExporter, SdkTracerProvider, SpanData},
};
use tower::ServiceExt;
use tower_http::trace::TraceLayer;
use tracing_subscriber::layer::SubscriberExt;

use super::{make_request_span, record_response, span_name};
use crate::{extract::TraceIdExt, telemetry};

const REMOTE_TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const REMOTE_SPAN_ID: &str = "00f067aa0ba902b7";

fn router() -> Router {
    Router::new()
        .route(
            "/items/{id}",
            get(|t: TraceIdExt| async move { t.0.unwrap_or_default() }),
        )
        .route(
            "/boom",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "x") }),
        )
        .layer(middleware::from_fn(telemetry::observe))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_request_span)
                .on_response(record_response),
        )
}

/// 1 リクエストを OTel 有効の subscriber 下で処理し、(応答 status, 本文, 出た span) を返す。
async fn call(req: Request<Body>) -> (StatusCode, String, Vec<SpanData>) {
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let resp = router().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    (status, String::from_utf8_lossy(&body).into_owned(), spans)
}

fn request_span(spans: &[SpanData]) -> &SpanData {
    spans
        .iter()
        .find(|s| s.attributes.iter().any(|kv| kv.key.as_str() == "url.path"))
        .expect("http_request span が出ていること")
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

#[tokio::test]
async fn continues_incoming_traceparent_and_hands_trace_id_to_handlers() {
    let req = Request::get("/items/42?access_token=secret-token")
        .header(
            "traceparent",
            format!("00-{REMOTE_TRACE_ID}-{REMOTE_SPAN_ID}-01"),
        )
        .body(Body::empty())
        .unwrap();
    let (status, body, spans) = call(req).await;
    assert_eq!(status, StatusCode::OK);
    // 監査ログへ渡る trace_id（extract::TraceId）が上流のトレースと同じになる。
    assert_eq!(body, REMOTE_TRACE_ID);

    let span = request_span(&spans);
    assert_eq!(
        span.span_context.trace_id(),
        TraceId::from_hex(REMOTE_TRACE_ID).unwrap()
    );
    assert_eq!(
        span.parent_span_id,
        SpanId::from_hex(REMOTE_SPAN_ID).unwrap()
    );
    assert_eq!(span.name, "GET /items/{id}");
    assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Server);
    assert_eq!(
        attr(span, "http.route").map(Value::as_str).as_deref(),
        Some("/items/{id}")
    );
    assert_eq!(
        attr(span, "http.request.method")
            .map(Value::as_str)
            .as_deref(),
        Some("GET")
    );
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(&Value::I64(200))
    );
    assert_eq!(span.status, Status::Unset);
    // クエリ（資格情報が来うる）は属性のどこにも載せない。
    assert!(attr(span, "url.query").is_none());
    assert!(
        !span
            .attributes
            .iter()
            .any(|kv| kv.value.as_str().contains("secret-token")),
        "クエリの値が span 属性に漏れていないこと"
    );
}

#[tokio::test]
async fn starts_new_trace_without_traceparent() {
    let req = Request::get("/items/1").body(Body::empty()).unwrap();
    let (_, body, spans) = call(req).await;
    let span = request_span(&spans);
    assert_ne!(span.span_context.trace_id(), TraceId::INVALID);
    assert_eq!(span.parent_span_id, SpanId::INVALID);
    // 新しく振った trace_id が監査ログ用 extension にも入る。
    assert_eq!(body, span.span_context.trace_id().to_string());
}

#[tokio::test]
async fn invalid_traceparent_is_ignored() {
    let req = Request::get("/items/1")
        .header("traceparent", "garbage")
        .body(Body::empty())
        .unwrap();
    let (status, body, spans) = call(req).await;
    assert_eq!(status, StatusCode::OK);
    let span = request_span(&spans);
    assert_eq!(span.parent_span_id, SpanId::INVALID);
    assert_eq!(body, span.span_context.trace_id().to_string());
}

#[tokio::test]
async fn unsampled_traceparent_does_not_drop_the_span() {
    // 外部が flags=00（非サンプリング）を送っても、trace_id は継続しつつ span は記録する。
    let req = Request::get("/boom")
        .header(
            "traceparent",
            format!("00-{REMOTE_TRACE_ID}-{REMOTE_SPAN_ID}-00"),
        )
        .body(Body::empty())
        .unwrap();
    let (status, _, spans) = call(req).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let span = request_span(&spans);
    assert_eq!(
        span.span_context.trace_id(),
        TraceId::from_hex(REMOTE_TRACE_ID).unwrap()
    );
    assert_eq!(
        span.parent_span_id,
        SpanId::from_hex(REMOTE_SPAN_ID).unwrap()
    );
    assert!(span.span_context.is_sampled());
    assert!(matches!(span.status, Status::Error { .. }));
}

#[tokio::test]
async fn keeps_legacy_method_and_path_fields() {
    let req = Request::post("/items/7").body(Body::empty()).unwrap();
    let (_, _, spans) = call(req).await;
    let span = request_span(&spans);
    assert_eq!(
        attr(span, "method").map(Value::as_str).as_deref(),
        Some("POST")
    );
    assert_eq!(
        attr(span, "path").map(Value::as_str).as_deref(),
        Some("/items/7")
    );
}

#[tokio::test]
async fn server_error_marks_span_as_error() {
    let req = Request::get("/boom").body(Body::empty()).unwrap();
    let (status, _, spans) = call(req).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let span = request_span(&spans);
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(&Value::I64(500))
    );
    assert!(matches!(span.status, Status::Error { .. }));
}

#[tokio::test]
async fn unmatched_route_uses_method_only_name() {
    let req = Request::get("/nope/123").body(Body::empty()).unwrap();
    let (status, _, spans) = call(req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let span = request_span(&spans);
    assert_eq!(span.name, "GET");
    assert!(attr(span, "http.route").is_none());
    // 4xx は span の status を ERROR にしない（semconv）。
    assert_eq!(span.status, Status::Unset);
}

#[test]
fn span_name_follows_semconv() {
    assert_eq!(span_name("POST", Some("/files/{id}")), "POST /files/{id}");
    assert_eq!(span_name("GET", None), "GET");
}
