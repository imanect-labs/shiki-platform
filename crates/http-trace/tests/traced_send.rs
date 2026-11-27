//! `send_traced` が client span を切り、その span を親とする `traceparent` を付けることを確かめる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use opentelemetry::trace::{SpanKind, Status, TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use shiki_http_trace::TracedSend;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;

/// 1 リクエストだけ受け、受け取ったリクエストヘッダ（小文字化した生テキスト）を返す HTTP サーバ。
async fn one_shot_server(status: u16) -> (String, Arc<Mutex<Option<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(None));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        *seen2.lock().unwrap() = Some(String::from_utf8_lossy(&buf).to_lowercase());
        let resp = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        sock.write_all(resp.as_bytes()).await.unwrap();
    });
    (format!("http://{addr}/check?secret=x"), seen)
}

fn header(raw: &str, name: &str) -> Option<String> {
    raw.lines()
        .find_map(|l| l.strip_prefix(&format!("{name}: ")))
        .map(|v| v.trim().to_owned())
}

fn attr(span: &SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.to_string())
}

fn otel_subscriber() -> (impl tracing::Subscriber + Send + Sync, InMemorySpanExporter) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = provider.tracer("test");
    let sub =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    (sub, exporter)
}

#[tokio::test]
async fn client_span_is_child_and_traceparent_points_to_it() {
    let (sub, exporter) = otel_subscriber();
    let _g = tracing::subscriber::set_default(sub);
    let (url, seen) = one_shot_server(200).await;

    let parent = tracing::info_span!("caller");
    let parent_cx = parent.context();
    let resp = reqwest::Client::new()
        .post(&url)
        .body("{}")
        .send_traced()
        .instrument(parent.clone())
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    drop(parent);

    let raw = seen.lock().unwrap().clone().unwrap();
    let tp = header(&raw, "traceparent").expect("traceparent が付いていない");
    let parts: Vec<&str> = tp.split('-').collect();
    assert_eq!(parts.len(), 4, "{tp}");

    let spans = exporter.get_finished_spans().unwrap();
    let client = spans
        .iter()
        .find(|s| s.span_kind == SpanKind::Client)
        .expect("client span が出ていない");
    let parent_sc = parent_cx.span().span_context().clone();
    // 呼んだ側の span の子になり、同じトレースに乗る。
    assert_eq!(client.span_context.trace_id(), parent_sc.trace_id());
    assert_eq!(client.parent_span_id, parent_sc.span_id());
    // 送り先には client span 自身が親として伝わる（受け側の span がこの下にぶら下がる）。
    assert_eq!(parts[1], client.span_context.trace_id().to_string());
    assert_eq!(parts[2], client.span_context.span_id().to_string());

    assert_eq!(client.name, "POST");
    assert_eq!(attr(client, "http.request.method").as_deref(), Some("POST"));
    assert_eq!(attr(client, "server.address").as_deref(), Some("127.0.0.1"));
    assert_eq!(
        attr(client, "http.response.status_code").as_deref(),
        Some("200")
    );
    assert!(attr(client, "server.port").is_some());
    // URL（クエリの秘密を含みうる）は載せない。
    assert!(client
        .attributes
        .iter()
        .all(|kv| !kv.value.to_string().contains("secret")));
    assert_eq!(client.status, Status::Unset);
}

#[tokio::test]
async fn error_status_marks_span_error() {
    let (sub, exporter) = otel_subscriber();
    let _g = tracing::subscriber::set_default(sub);
    let (url, _seen) = one_shot_server(503).await;

    let resp = reqwest::Client::new()
        .get(&url)
        .send_traced()
        .instrument(tracing::info_span!("caller"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);

    let spans = exporter.get_finished_spans().unwrap();
    let client = spans
        .iter()
        .find(|s| s.span_kind == SpanKind::Client)
        .unwrap();
    assert_eq!(
        attr(client, "http.response.status_code").as_deref(),
        Some("503")
    );
    assert_eq!(attr(client, "error.type").as_deref(), Some("503"));
    assert!(matches!(client.status, Status::Error { .. }));
}

#[tokio::test]
async fn connect_failure_marks_span_error() {
    let (sub, exporter) = otel_subscriber();
    let _g = tracing::subscriber::set_default(sub);
    // 閉じたポートに繋いで接続失敗にする。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let res = reqwest::Client::new()
        .get(format!("http://{addr}/"))
        .send_traced()
        .await;
    assert!(res.is_err());

    let spans = exporter.get_finished_spans().unwrap();
    let client = spans
        .iter()
        .find(|s| s.span_kind == SpanKind::Client)
        .unwrap();
    assert_eq!(attr(client, "error.type").as_deref(), Some("connect"));
    assert!(matches!(client.status, Status::Error { .. }));
}

#[tokio::test]
async fn no_traceparent_without_otel_layer() {
    // OTel 無効（tracing_opentelemetry の layer 無し）なら span の文脈が無効で、何も付けない。
    let _g = tracing::subscriber::set_default(tracing_subscriber::registry());
    let (url, seen) = one_shot_server(200).await;
    reqwest::Client::new()
        .get(&url)
        .send_traced()
        .instrument(tracing::info_span!("caller"))
        .await
        .unwrap();
    let raw = seen.lock().unwrap().clone().unwrap();
    assert!(header(&raw, "traceparent").is_none(), "{raw}");
}

#[tokio::test]
async fn invalid_request_is_returned_as_error() {
    let res = reqwest::Client::new().get("not a url").send_traced().await;
    assert!(res.is_err());
}
