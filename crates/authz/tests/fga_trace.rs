//! OpenFGA の check が呼んだ側の子の client span になり、OpenFGA へその span を親とする
//! `traceparent` が届くことを確かめる（shiki-http-trace）。OpenFGA は OTel 対応なので、
//! これで OpenFGA 内部の span も同じトレースにぶら下がる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use authz::fga_http::FgaHttp;
use opentelemetry::trace::{SpanKind, TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;

/// 1 リクエストだけ受けて `{"allowed":true}` を返し、受けたヘッダ（小文字化）を記録する。
async fn fake_openfga() -> (String, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        *seen2.lock().unwrap() = String::from_utf8_lossy(&buf).to_lowercase();
        let body = r#"{"allowed":true}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn check_is_child_span_and_propagates_traceparent() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let sub = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _g = tracing::subscriber::set_default(sub);

    let (base, seen) = fake_openfga().await;
    let fga = FgaHttp::new(reqwest::Client::new(), base);
    let caller = tracing::info_span!("authz.check");
    let caller_sc = caller.context().span().span_context().clone();
    let allowed = fga
        .check(
            "store",
            "model",
            "user:a",
            "viewer",
            "doc:x",
            "MINIMIZE_LATENCY",
        )
        .instrument(caller.clone())
        .await
        .unwrap();
    assert!(allowed);
    drop(caller);

    let spans = exporter.get_finished_spans().unwrap();
    let client = spans
        .iter()
        .find(|s| s.span_kind == SpanKind::Client)
        .expect("client span が出ていない");
    assert_eq!(client.span_context.trace_id(), caller_sc.trace_id());
    assert_eq!(client.parent_span_id, caller_sc.span_id());

    let raw = seen.lock().unwrap().clone();
    let expected = format!(
        "traceparent: 00-{}-{}-01",
        client.span_context.trace_id(),
        client.span_context.span_id()
    );
    assert!(raw.contains(&expected), "{raw}");
}
