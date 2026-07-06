//! Anthropic Messages API アダプタの SSE ストリーミング結線テスト（ローカル axum モック）。
//!
//! 127.0.0.1 の ephemeral port に `/v1/messages` 相当のモックを立て、`AnthropicProvider::stream`
//! が content_block_delta / message_delta 系 SSE を中立 [`StreamDelta`] 列へ写すことと、
//! 4xx/5xx のエラーマッピングを検証する。DB 非依存（provider 直叩き）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use futures::stream::StreamExt;

use llm_gateway::model::{GenerateRequest, Message, Role, StopReason, StreamDelta};
use llm_gateway::provider::{LlmError, LlmProvider};
use llm_gateway::providers::anthropic::AnthropicProvider;

/// 固定ステータス＋固定ボディを返すモックサーバを立て、base_url を返す。
async fn spawn_mock(status: StatusCode, body: &'static str) -> String {
    let app = Router::new().fallback(move || async move {
        (status, [("content-type", "text/event-stream")], body).into_response()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// message_start → text/thinking delta → tool_use ブロック → message_delta → message_stop。
const SSE_BODY: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"doc_search\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":1}\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

/// ストリームを最後まで排出して中立デルタ列へ集める。
async fn drain(provider: &AnthropicProvider, req: &GenerateRequest) -> Vec<StreamDelta> {
    let mut stream = provider.stream(req).await.expect("stream should start");
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item.expect("delta should be Ok"));
    }
    out
}

#[tokio::test]
async fn anthropic_stream_yields_text_thinking_tool_and_done() {
    let base = spawn_mock(StatusCode::OK, SSE_BODY).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-x");
    let req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);

    let deltas = drain(&provider, &req).await;

    assert!(deltas.contains(&StreamDelta::TextDelta { text: "Hi".into() }));
    assert!(deltas.contains(&StreamDelta::ThinkingDelta { text: "hmm".into() }));
    assert!(deltas.contains(&StreamDelta::ToolUseStart {
        id: "toolu_1".into(),
        name: "doc_search".into(),
    }));
    let stop = deltas
        .iter()
        .find_map(|d| match d {
            StreamDelta::ToolUseStop { id, input } => Some((id.clone(), input.clone())),
            _ => None,
        })
        .expect("ToolUseStop expected");
    assert_eq!(stop.0, "toolu_1");
    assert_eq!(stop.1["q"], 1);

    // message_stop で Done（stop_reason=tool_use・usage は input/output 双方反映）。
    match deltas.last().expect("non-empty") {
        StreamDelta::Done { stop_reason, usage } => {
            assert_eq!(*stop_reason, StopReason::ToolUse);
            assert_eq!(usage.prompt_tokens, 12);
            assert_eq!(usage.completion_tokens, 7);
        }
        other => panic!("last delta should be Done, got {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_client_error_maps_to_bad_request() {
    let base = spawn_mock(StatusCode::BAD_REQUEST, "bad").await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-x");
    let req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);

    let res = provider.stream(&req).await;
    assert!(matches!(res, Err(LlmError::BadRequest(_))));
}

#[tokio::test]
async fn anthropic_server_error_maps_to_unavailable() {
    let base = spawn_mock(StatusCode::SERVICE_UNAVAILABLE, "down").await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-x");
    let req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);

    let res = provider.stream(&req).await;
    assert!(matches!(res, Err(LlmError::Unavailable(_))));
}
