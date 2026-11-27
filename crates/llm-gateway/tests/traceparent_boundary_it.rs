//! 外向き HTTP の `traceparent` の送り先の境界（shiki-http-trace）。
//!
//! OTel が有効な状態で、内部サービス（Langfuse）には `traceparent` が付き、外部の LLM
//! プロバイダ（OpenAI 互換・OpenAI Responses・Anthropic）には付かないことを、実際に受信した
//! リクエストヘッダで確かめる。トレース ID を外部へ出さないための回帰テスト。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Router;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::Instrument;
use tracing_subscriber::layer::SubscriberExt;

use llm_gateway::config::LangfuseConfig;
use llm_gateway::langfuse::{GenerationTrace, LangfuseClient};
use llm_gateway::model::{GenerateRequest, Message, Role, Usage};
use llm_gateway::provider::LlmProvider;
use llm_gateway::providers::anthropic::AnthropicProvider;
use llm_gateway::providers::openai::OpenAiProvider;
use llm_gateway::providers::openai_responses::OpenAiResponsesProvider;

/// 受け取ったリクエストの `traceparent`（無ければ None）を全件ためる。
type Seen = Arc<Mutex<Vec<Option<String>>>>;

async fn capture(State(seen): State<Seen>, headers: HeaderMap) -> StatusCode {
    let tp = headers
        .get("traceparent")
        .map(|v| v.to_str().unwrap().to_owned());
    seen.lock().unwrap().push(tp);
    // 本文は見ないので、どの呼び出し側もエラーとして扱う応答でよい。
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn spawn_capture() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().fallback(capture).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

/// OTel を有効にした subscriber（トレースは捨てる）。
fn otel_subscriber() -> impl tracing::Subscriber + Send + Sync {
    let tracer = SdkTracerProvider::builder().build().tracer("test");
    tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer))
}

fn only(seen: &Seen) -> Option<String> {
    let all = seen.lock().unwrap().clone();
    assert_eq!(all.len(), 1, "受信は 1 件のはず: {all:?}");
    all.into_iter().next().unwrap()
}

async fn call_provider(provider: &dyn LlmProvider) {
    let req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    let _ = provider
        .stream(&req)
        .instrument(tracing::info_span!("chat.generation"))
        .await;
}

#[tokio::test]
async fn external_llm_providers_get_no_traceparent() {
    let _g = tracing::subscriber::set_default(otel_subscriber());

    let (base, seen) = spawn_capture().await;
    call_provider(&OpenAiProvider::new(
        reqwest::Client::new(),
        base,
        None,
        "m",
    ))
    .await;
    assert_eq!(only(&seen), None, "OpenAI 互換に traceparent が出ている");

    let (base, seen) = spawn_capture().await;
    call_provider(&OpenAiResponsesProvider::new(
        reqwest::Client::new(),
        base,
        None,
        "m",
    ))
    .await;
    assert_eq!(
        only(&seen),
        None,
        "OpenAI Responses に traceparent が出ている"
    );

    let (base, seen) = spawn_capture().await;
    call_provider(&AnthropicProvider::new(
        reqwest::Client::new(),
        base,
        "k",
        "m",
    ))
    .await;
    assert_eq!(only(&seen), None, "Anthropic に traceparent が出ている");
}

#[tokio::test]
async fn internal_langfuse_gets_traceparent() {
    let _g = tracing::subscriber::set_default(otel_subscriber());
    let (base, seen) = spawn_capture().await;
    let client = LangfuseClient::new(
        reqwest::Client::new(),
        &LangfuseConfig {
            base_url: base,
            public_key: "pk".into(),
            secret_key: "sk".into(),
        },
    );
    client
        .record_generation(GenerationTrace {
            trace_id: "t",
            generation_id: "g",
            name: "chat.generation",
            model: "m",
            provider: "openai",
            input: "in",
            output: "out",
            usage: Usage::default(),
            user_id: "u",
            first_in_trace: true,
            started_at: None,
            ended_at: chrono::Utc::now(),
            metadata: serde_json::json!({}),
        })
        .instrument(tracing::info_span!("chat.generation"))
        .await;
    let tp = only(&seen).expect("Langfuse に traceparent が付いていない");
    assert!(tp.starts_with("00-"), "{tp}");
}
