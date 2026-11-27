//! LLM 呼び出しの計装（GenAI semconv の span）の結合テスト（実 Postgres・stub プロバイダ）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - `LlmGateway::stream` の span が呼び出し元の span の子として出る（＝同じ trace_id に乗る）。
//! - 振り先（実モデル ID・プロバイダ）と使用トークンが刻まれ、ストリームの終わりまで閉じない。
//! - 拒否（許可外のモデル等）は span に失敗として残る。本文は載らない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

#[path = "../src/telemetry/capture.rs"]
mod capture;

use authz::{AuthContext, Principal};
use futures::StreamExt;
use llm_gateway::{
    GatewayConfig, GenerateRequest, LlmError, LlmGateway, Message, ModelCatalog, ModelEntry, Role,
};
use sqlx::postgres::PgPoolOptions;

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

async fn gateway() -> Option<LlmGateway> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("DB 接続");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let config = GatewayConfig {
        catalog: ModelCatalog {
            default_model: "fast".into(),
            models: vec![ModelEntry {
                id: "fast".into(),
                real_id: Some("stub-real-1".into()),
                ..Default::default()
            }],
        },
        ..Default::default()
    };
    Some(LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway"))
}

#[tokio::test]
async fn stream_span_is_child_of_caller_and_closes_after_stream() {
    let Some(gw) = gateway().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let cap = capture::Captured::default();
    let _g = cap.guard();

    let parent = tracing::info_span!("chat.turn");
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "社外秘の質問")]);
    req.model = Some("fast".into());
    let mut stream = {
        use tracing::Instrument;
        gw.stream(&ctx(&tenant), req)
            .instrument(parent.clone())
            .await
            .unwrap()
    };
    drop(parent);
    let mut items = 0;
    while let Some(d) = stream.next().await {
        d.unwrap();
        items += 1;
        assert!(
            !cap.span("gen_ai.chat").closed,
            "ストリームの途中で閉じない"
        );
    }
    assert!(items > 0);
    // 終端に達した時点で閉じている（drop を待たない）。
    let s = cap.span("gen_ai.chat");
    assert!(s.closed);
    drop(stream);
    assert_eq!(s.parent.as_deref(), Some("chat.turn"));
    assert_eq!(s.field("shiki.llm.model"), "fast");
    assert_eq!(s.field("gen_ai.request.model"), "stub-real-1");
    assert_eq!(s.field("gen_ai.provider.name"), "stub");
    assert!(s.fields.contains_key("gen_ai.usage.input_tokens"));
    assert!(s.fields.contains_key("gen_ai.usage.output_tokens"));
    assert!(s.fields.contains_key("gen_ai.server.time_to_first_token"));
    assert!(!s.fields.contains_key("otel.status_code"));
    assert!(
        !cap.all_values().iter().any(|v| v.contains("社外秘")),
        "プロンプト本文は span に載せない"
    );
}

#[tokio::test]
async fn rejected_call_is_recorded_as_error() {
    let Some(gw) = gateway().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let cap = capture::Captured::default();
    let _g = cap.guard();
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.model = Some("missing".into());
    assert!(matches!(
        gw.stream(&ctx(&tenant), req).await,
        Err(LlmError::Config(_))
    ));
    let s = cap.span("gen_ai.chat");
    assert!(s.closed);
    assert_eq!(s.field("otel.status_code"), "ERROR");
    assert_eq!(s.field("error.type"), "config");
    // 未検証のモデル名は span 名・属性に載せない。
    assert_eq!(s.field("otel.name"), "chat");
    assert!(!cap.all_values().iter().any(|v| v.contains("missing")));
}

/// Langfuse の trace id と OTel の trace_id が揃う: Langfuse に渡す trace_id（run の trace_id）を
/// 親の文脈に持たせて `stream` を呼ぶと、LLM の span（OTel にエクスポートされるもの）の
/// trace_id がそれと一致し、親 span の子になる。
#[tokio::test]
async fn llm_span_keeps_the_callers_otel_trace_id() {
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState, TracerProvider as _,
    };
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing::Instrument;
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    use tracing_subscriber::layer::SubscriberExt;

    let Some(gw) = gateway().await else { return };
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _g = tracing::subscriber::set_default(subscriber);

    // run に永続化され Langfuse の trace id になる値（32-hex）。
    let langfuse_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let remote = SpanContext::new(
        TraceId::from_hex(langfuse_trace_id).unwrap(),
        SpanId::from_hex("00f067aa0ba902b7").unwrap(),
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    let parent = tracing::info_span!("chat.turn");
    parent.set_parent(opentelemetry::Context::new().with_remote_span_context(remote));

    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.model = Some("fast".into());
    let mut stream = gw
        .stream(&ctx(&tenant), req)
        .instrument(parent.clone())
        .await
        .unwrap();
    while let Some(d) = stream.next().await {
        d.unwrap();
    }
    drop(stream);
    drop(parent);
    provider.force_flush().unwrap();

    let spans = exporter.get_finished_spans().unwrap();
    let llm = spans
        .iter()
        .find(|s| s.name == "chat fast")
        .unwrap_or_else(|| {
            panic!(
                "LLM の span が無い: {:?}",
                spans.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });
    let turn = spans.iter().find(|s| s.name == "chat.turn").unwrap();
    assert_eq!(llm.span_context.trace_id().to_string(), langfuse_trace_id);
    assert_eq!(llm.parent_span_id, turn.span_context.span_id());
    assert!(llm
        .attributes
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.request.model" && kv.value.as_str() == "stub-real-1"));
    // finish_reasons は規約どおり文字列配列。
    let reasons = llm
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "gen_ai.response.finish_reasons")
        .expect("finish_reasons");
    assert!(
        matches!(&reasons.value, opentelemetry::Value::Array(opentelemetry::Array::String(v)) if v.len() == 1),
        "{:?}",
        reasons.value
    );
}

/// LLM の span がフィルタで無効でも、呼び出し元の span（`otel.name` を持つ）を書き換えない。
#[tokio::test]
async fn disabled_llm_span_does_not_touch_the_callers_span() {
    use tracing::Instrument;
    let Some(gw) = gateway().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let cap = capture::Captured::default();
    let _g = cap.guard_without("gen_ai.chat");
    let parent = tracing::info_span!(
        "http.request",
        "otel.name" = "POST /messages",
        "shiki.llm.model" = tracing::field::Empty,
        "gen_ai.request.model" = tracing::field::Empty,
    );
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.model = Some("fast".into());
    let mut stream = gw
        .stream(&ctx(&tenant), req)
        .instrument(parent.clone())
        .await
        .unwrap();
    while let Some(d) = stream.next().await {
        d.unwrap();
    }
    drop(stream);
    drop(parent);
    assert!(cap.find("gen_ai.chat").is_none(), "LLM の span は無効");
    let p = cap.span("http.request");
    assert_eq!(p.field("otel.name"), "POST /messages");
    assert!(!p.fields.contains_key("shiki.llm.model"));
    assert!(!p.fields.contains_key("gen_ai.request.model"));
}
