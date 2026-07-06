//! Langfuse ingestion クライアントの結線テスト（ローカル axum モック）。
//!
//! `/api/public/ingestion` へ送られる batch の JSON 形（trace-create ＋ generation-create）を
//! 実際に受信して検証する。207 応答を成功として扱い、best-effort 送信がエラーにならないことも確認。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;
use serde_json::Value;

use llm_gateway::config::LangfuseConfig;
use llm_gateway::langfuse::{GenerationTrace, LangfuseClient};
use llm_gateway::model::Usage;

type Captured = Arc<Mutex<Option<Value>>>;

/// 受信した batch を捕捉し 207 を返すハンドラ。
async fn ingest(State(state): State<Captured>, Json(body): Json<Value>) -> StatusCode {
    *state.lock().unwrap() = Some(body);
    StatusCode::MULTI_STATUS
}

#[tokio::test]
async fn record_generation_sends_expected_batch_shape() {
    let captured: Captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/api/public/ingestion", post(ingest))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let cfg = LangfuseConfig {
        base_url: format!("http://{addr}"),
        public_key: "pk".into(),
        secret_key: "sk".into(),
    };
    let client = LangfuseClient::new(reqwest::Client::new(), &cfg);

    client
        .record_generation(GenerationTrace {
            trace_id: "trace-abc",
            name: "chat.generation",
            model: "gpt-x",
            provider: "openai",
            input: "in",
            output: "out",
            usage: Usage {
                prompt_tokens: 11,
                completion_tokens: 22,
            },
            metadata: serde_json::json!({ "tenant_id": "t1" }),
        })
        .await;

    let body = captured.lock().unwrap().clone().expect("batch received");
    let batch = body["batch"].as_array().expect("batch array");
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0]["type"], "trace-create");
    assert_eq!(batch[0]["body"]["id"], "trace-abc");
    assert_eq!(batch[0]["body"]["metadata"]["tenant_id"], "t1");

    assert_eq!(batch[1]["type"], "generation-create");
    assert_eq!(batch[1]["body"]["traceId"], "trace-abc");
    assert_eq!(batch[1]["body"]["model"], "gpt-x");
    assert_eq!(batch[1]["body"]["usage"]["input"], 11);
    assert_eq!(batch[1]["body"]["usage"]["output"], 22);
    assert_eq!(batch[1]["body"]["metadata"]["provider"], "openai");
}
