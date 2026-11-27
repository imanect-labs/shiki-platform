//! ingestion-worker の呼び出し（/embed）が呼んだ側の子の client span になり、worker へ
//! その span を親とする `traceparent` が届くことを確かめる（shiki-http-trace）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use authz::{AuthContext, Principal};
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use opentelemetry::trace::{SpanKind, TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use rag::embedding::EmbeddingProvider;
use rag::{EmbedInput, HttpEmbeddingProvider};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;

fn ctx() -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("a-corp".into()),
        },
        "acme".into(),
        authz::TenantId::parse("a-corp").unwrap(),
    )
}

#[tokio::test]
async fn embed_call_is_child_span_and_propagates_traceparent() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let sub = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _g = tracing::subscriber::set_default(sub);

    let seen: Arc<Mutex<Option<String>>> = Arc::default();
    let seen2 = seen.clone();
    let router = Router::new().route(
        "/embed",
        post(move |headers: HeaderMap| async move {
            *seen2.lock().unwrap() = headers
                .get("traceparent")
                .map(|v| v.to_str().unwrap().to_owned());
            Json(serde_json::json!({
                "vectors": [[0.1, 0.2]],
                "model_version": "m",
                "dimension": 2
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let embedder =
        HttpEmbeddingProvider::new(reqwest::Client::new(), &format!("http://{addr}"), "m");
    let caller = tracing::info_span!("rag.search");
    let caller_sc = caller.context().span().span_context().clone();
    embedder
        .embed(&ctx(), EmbedInput::Query, &["q".into()])
        .instrument(caller.clone())
        .await
        .unwrap();
    drop(caller);

    let spans = exporter.get_finished_spans().unwrap();
    let client = spans
        .iter()
        .find(|s| s.span_kind == SpanKind::Client)
        .expect("client span が出ていない");
    assert_eq!(client.span_context.trace_id(), caller_sc.trace_id());
    assert_eq!(client.parent_span_id, caller_sc.span_id());

    let tp = seen
        .lock()
        .unwrap()
        .clone()
        .expect("worker に traceparent が届いていない");
    assert_eq!(
        tp,
        format!(
            "00-{}-{}-01",
            client.span_context.trace_id(),
            client.span_context.span_id()
        )
    );
}
