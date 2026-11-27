//! リクエスト root span（`OtelAxumLayer` ＋ `telemetry::observe`）の振る舞いを、実ルータに
//! 近い層の重ね方で確かめる。OTel の span はメモリ上のエクスポータで受け取る。

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
use tracing_subscriber::layer::SubscriberExt;

use super::http_util::{request_span_layer, strip_incoming_trace_context};
use crate::{extract::TraceIdExt, telemetry};

const PARENT_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT_SPAN: &str = "00f067aa0ba902b7";

fn traceparent() -> String {
    format!("00-{PARENT_TRACE}-{PARENT_SPAN}-01")
}

/// テスト用の OTel パイプライン（thread-local の subscriber）。drop まで有効。
struct Otel {
    exporter: InMemorySpanExporter,
    provider: SdkTracerProvider,
    _guard: tracing::subscriber::DefaultGuard,
}

impl Otel {
    fn new() -> Self {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let guard = tracing::subscriber::set_default(subscriber);
        Self {
            exporter,
            provider,
            _guard: guard,
        }
    }

    fn spans(&self) -> Vec<SpanData> {
        self.provider.force_flush().expect("flush");
        self.exporter.get_finished_spans().expect("spans")
    }
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

async fn trace_id_body(TraceIdExt(id): TraceIdExt) -> String {
    id.unwrap_or_default()
}

fn app(trust_incoming: bool) -> Router {
    let router = Router::new()
        .route("/files/{id}", get(trace_id_body))
        .route("/healthz", get(trace_id_body))
        .route("/boom", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route(
            "/authed",
            get(|| async {
                telemetry::record_auth_on_span("tenant-a", "user-1");
                "ok"
            }),
        )
        .layer(middleware::from_fn(telemetry::observe))
        .layer(request_span_layer());
    if trust_incoming {
        router
    } else {
        router.layer(middleware::map_request(strip_incoming_trace_context))
    }
}

async fn call(app: Router, uri: &str, traceparent: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::builder().uri(uri);
    if let Some(tp) = traceparent {
        req = req.header("traceparent", tp);
    }
    let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn incoming_traceparent_is_continued_and_matches_audit_trace_id() {
    let otel = Otel::new();
    let (status, body) = call(app(true), "/files/123", Some(&traceparent())).await;
    assert_eq!(status, StatusCode::OK);
    // 監査ログが使う TraceId extension ＝ 受信した親の trace_id。
    assert_eq!(body, PARENT_TRACE);

    let spans = otel.spans();
    assert_eq!(spans.len(), 1, "{spans:?}");
    let span = &spans[0];
    assert_eq!(
        span.span_context.trace_id(),
        TraceId::from_hex(PARENT_TRACE).unwrap()
    );
    assert_eq!(span.parent_span_id, SpanId::from_hex(PARENT_SPAN).unwrap());
    assert_eq!(span.name, "GET /files/{id}");
    // http.route はテンプレート（生 path ではない）。
    assert_eq!(attr(span, "http.route"), Some(&Value::from("/files/{id}")));
    assert_eq!(attr(span, "http.request.method"), Some(&Value::from("GET")));
    // ライブラリは u16 を記録し、tracing-opentelemetry 0.31 は u64 を文字列で載せる（"200"）。
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(&Value::from("200"))
    );
    assert_eq!(attr(span, "trace_id"), Some(&Value::from(PARENT_TRACE)));
    assert_eq!(span.status, Status::Unset);
}

#[tokio::test]
async fn request_without_traceparent_starts_new_trace() {
    let otel = Otel::new();
    let (_, body) = call(app(true), "/files/456", None).await;
    assert_eq!(body.len(), 32, "trace_id が載る: {body}");
    assert_ne!(body, PARENT_TRACE);
    let spans = otel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].span_context.trace_id().to_string(), body);
    assert_eq!(spans[0].parent_span_id, SpanId::INVALID);
}

#[tokio::test]
async fn server_error_marks_span_status_error() {
    let otel = Otel::new();
    let (status, _) = call(app(true), "/boom", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let spans = otel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "http.response.status_code"),
        Some(&Value::from("500"))
    );
    assert!(
        matches!(spans[0].status, Status::Error { .. }),
        "{:?}",
        spans[0].status
    );
}

#[tokio::test]
async fn health_check_is_not_traced() {
    let otel = Otel::new();
    let (status, body) = call(app(true), "/healthz", Some(&traceparent())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "", "span が無いので trace_id も載らない");
    assert!(otel.spans().is_empty());
}

#[tokio::test]
async fn untrusted_mode_discards_incoming_traceparent() {
    let otel = Otel::new();
    let (_, body) = call(app(false), "/files/1", Some(&traceparent())).await;
    assert_eq!(body.len(), 32);
    assert_ne!(body, PARENT_TRACE);
    let spans = otel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].parent_span_id, SpanId::INVALID);
}

#[tokio::test]
async fn auth_attributes_are_recorded_on_root_span() {
    let otel = Otel::new();
    let (status, _) = call(app(true), "/authed", None).await;
    assert_eq!(status, StatusCode::OK);
    let spans = otel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(attr(&spans[0], "tenant_id"), Some(&Value::from("tenant-a")));
    assert_eq!(attr(&spans[0], "enduser.id"), Some(&Value::from("user-1")));
}
