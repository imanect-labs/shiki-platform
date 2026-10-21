//! Anthropic Messages API アダプタの SSE ストリーミング結線テスト（ローカル axum モック）。
//!
//! 127.0.0.1 の ephemeral port に `/v1/messages` 相当のモックを立て、`AnthropicProvider::stream`
//! が content_block_delta / message_delta 系 SSE を中立 [`StreamDelta`] 列へ写すことと、
//! 4xx/5xx のエラーマッピングを検証する。DB 非依存（provider 直叩き）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use futures::stream::StreamExt;

use llm_gateway::model::{Block, GenerateRequest, Message, Role, StopReason, StreamDelta, ToolDef};
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
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":20}}}\n\n",
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
            // input_tokens はキャッシュ分を含まないので、総数は 12 + 100 + 20（#581）。
            assert_eq!(usage.prompt_tokens, 132);
            assert_eq!(usage.cache_read_tokens, 100);
            assert_eq!(usage.cache_write_tokens, 20);
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

type Seen = Arc<Mutex<Option<HeaderMap>>>;

/// 固定ステータス＋固定ボディを返し、受けたヘッダを記録するモックを立てる。
async fn spawn_recording_mock(status: StatusCode, body: &'static str) -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(None));
    let rec = seen.clone();
    let app = Router::new().fallback(move |headers: HeaderMap| {
        let rec = rec.clone();
        async move {
            *rec.lock().unwrap() = Some(headers);
            (status, [("content-type", "text/event-stream")], body).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn anthropic_sends_identity_headers_when_configured() {
    let (base, seen) = spawn_recording_mock(StatusCode::OK, SSE_BODY).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "m")
        .with_user_agent(Some("shiki-test/1.0"))
        .unwrap()
        .with_session_header(Some("x-opencode-session"))
        .unwrap();
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.session_id = Some("thread-42".into());
    drain(&provider, &req).await;
    let headers = seen.lock().unwrap().clone().unwrap();
    assert_eq!(headers["x-api-key"], "k");
    assert_eq!(headers["user-agent"], "shiki-test/1.0");
    assert_eq!(headers["x-opencode-session"], "thread-42");
}

#[tokio::test]
async fn anthropic_sends_no_session_header_when_unset() {
    let (base, seen) = spawn_recording_mock(StatusCode::OK, SSE_BODY).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "m");
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.session_id = Some("thread-42".into());
    drain(&provider, &req).await;
    let headers = seen.lock().unwrap().clone().unwrap();
    assert!(headers.get("x-opencode-session").is_none());
}

/// tool_use の名前が wire 名（`document_edit`）で返る SSE。
const SSE_DOTTED_TOOL: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_2\",\"name\":\"document_edit\"}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

type SeenBody = Arc<Mutex<Option<serde_json::Value>>>;

/// 受けたリクエストボディ（JSON）を記録するモック。
async fn spawn_body_mock(body: &'static str) -> (String, SeenBody) {
    let seen: SeenBody = Arc::new(Mutex::new(None));
    let rec = seen.clone();
    let app = Router::new().fallback(move |req: String| {
        let rec = rec.clone();
        async move {
            *rec.lock().unwrap() = serde_json::from_str(&req).ok();
            (
                StatusCode::OK,
                [("content-type", "text/event-stream")],
                body,
            )
                .into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn anthropic_dotted_tool_names_round_trip_through_wire_names() {
    // ドット入りのツール名は API が 400 で拒否する（#582）。送るときは wire 名、受けた
    // tool_use は元の名前へ戻す。
    let (base, seen) = spawn_body_mock(SSE_DOTTED_TOOL).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "m");
    let mut req = GenerateRequest::new(vec![
        Message::text(Role::User, "edit"),
        Message {
            role: Role::Assistant,
            content: vec![Block::ToolUse {
                id: "toolu_1".into(),
                name: "document.edit".into(),
                input: serde_json::json!({}),
            }],
        },
        Message {
            role: Role::Tool,
            content: vec![Block::tool_result("toolu_1", "ok", false)],
        },
    ]);
    req.tools = vec![ToolDef::new(
        "document.edit",
        "d",
        serde_json::json!({"type": "object"}),
    )];
    let deltas = drain(&provider, &req).await;

    let body = seen.lock().unwrap().clone().unwrap();
    assert_eq!(body["tools"][0]["name"], "document_edit");
    assert_eq!(body["messages"][1]["content"][0]["name"], "document_edit");
    assert!(deltas.contains(&StreamDelta::ToolUseStart {
        id: "toolu_2".into(),
        name: "document.edit".into(),
    }));
}

/// 実エンドポイントでの往復（`ANTHROPIC_TEST_URL` が無ければ黙ってスキップ）。
///
/// 例（OpenCode Go）: `ANTHROPIC_TEST_URL=https://opencode.ai/zen/go` `ANTHROPIC_TEST_MODEL=claude-haiku-5-5`
///     `ANTHROPIC_TEST_SESSION_HEADER=x-opencode-session` `ANTHROPIC_TEST_KEY=<キー>`
#[tokio::test]
async fn anthropic_live_round_trip() {
    let Ok(base) = std::env::var("ANTHROPIC_TEST_URL") else {
        return;
    };
    let model = std::env::var("ANTHROPIC_TEST_MODEL").unwrap_or_else(|_| "m".into());
    let provider = AnthropicProvider::new(
        reqwest::Client::new(),
        base,
        std::env::var("ANTHROPIC_TEST_KEY").unwrap_or_default(),
        model,
    )
    .with_user_agent(Some("shiki-it/1.0"))
    .unwrap()
    .with_session_header(
        std::env::var("ANTHROPIC_TEST_SESSION_HEADER")
            .ok()
            .as_deref(),
    )
    .unwrap();
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "1+1は？数字のみで答えて")]);
    req.session_id = Some(format!("shiki-it-{}", uuid::Uuid::new_v4()));
    req.max_tokens = Some(2000);
    let deltas = drain(&provider, &req).await;
    let text: String = deltas
        .iter()
        .filter_map(|d| match d {
            StreamDelta::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains('2'), "answer: {text}");
    assert!(
        matches!(deltas.last(), Some(StreamDelta::Done { usage, .. }) if usage.prompt_tokens > 0)
    );
}

/// 実エンドポイントで、ドット入りのツール名が 400 にならず、呼び出しが元の名前で戻ること（#582）。
/// `ANTHROPIC_TEST_URL` が無ければ黙ってスキップ（設定は上の往復テストと同じ）。
#[tokio::test]
async fn anthropic_live_dotted_tool_call() {
    let Ok(base) = std::env::var("ANTHROPIC_TEST_URL") else {
        return;
    };
    let model = std::env::var("ANTHROPIC_TEST_MODEL").unwrap_or_else(|_| "m".into());
    let provider = AnthropicProvider::new(
        reqwest::Client::new(),
        base,
        std::env::var("ANTHROPIC_TEST_KEY").unwrap_or_default(),
        model,
    )
    .with_session_header(
        std::env::var("ANTHROPIC_TEST_SESSION_HEADER")
            .ok()
            .as_deref(),
    )
    .unwrap();
    let mut req = GenerateRequest::new(vec![Message::text(
        Role::User,
        "weather.lookup ツールで東京の天気を調べて",
    )]);
    req.tools = vec![ToolDef::new(
        "weather.lookup",
        "都市の天気を返す",
        serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
        }),
    )];
    req.session_id = Some(format!("shiki-it-{}", uuid::Uuid::new_v4()));
    req.max_tokens = Some(2000);
    let deltas = drain(&provider, &req).await;
    assert!(
        deltas.iter().any(
            |d| matches!(d, StreamDelta::ToolUseStart { name, .. } if name == "weather.lookup")
        ),
        "deltas: {deltas:?}"
    );
}
