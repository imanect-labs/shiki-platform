//! OpenAI 互換プロバイダの SSE ストリーミング結線テスト（ローカル axum モック）。
//!
//! 127.0.0.1 の ephemeral port に `/chat/completions` 相当のモックを立て、実際の
//! バイトストリーム経路（`OpenAiProvider::stream`）が中立 [`StreamDelta`] 列へ正しく
//! 写すことと、4xx/5xx のエラーマッピングを検証する。DB 非依存（provider 直叩き）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use futures::stream::StreamExt;

use llm_gateway::model::{GenerateRequest, Message, Role, StopReason, StreamDelta};
use llm_gateway::provider::{LlmError, LlmProvider};
use llm_gateway::providers::openai::OpenAiProvider;

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

/// text / reasoning / tool_call / usage / [DONE] を含む代表的な SSE。
const SSE_BODY: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"doc_search\",\"arguments\":\"{\\\"q\\\":1}\"}}]}}]}\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n",
    "data: [DONE]\n",
);

/// ストリームを最後まで排出して中立デルタ列へ集める。
async fn drain(provider: &OpenAiProvider, req: &GenerateRequest) -> Vec<StreamDelta> {
    let mut stream = provider.stream(req).await.expect("stream should start");
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item.expect("delta should be Ok"));
    }
    out
}

#[tokio::test]
async fn openai_stream_yields_text_thinking_tool_and_done() {
    let base = spawn_mock(StatusCode::OK, SSE_BODY).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "gpt-x");
    let req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);

    let deltas = drain(&provider, &req).await;

    assert!(deltas.contains(&StreamDelta::TextDelta {
        text: "Hello".into()
    }));
    assert!(deltas.contains(&StreamDelta::ThinkingDelta {
        text: "think".into()
    }));
    assert!(deltas.contains(&StreamDelta::ToolUseStart {
        id: "call_1".into(),
        name: "doc_search".into(),
    }));
    // ツール入力 JSON は完了時に 1 回だけ累積して出る。
    let stop = deltas
        .iter()
        .find_map(|d| match d {
            StreamDelta::ToolUseStop { id, input } => Some((id.clone(), input.clone())),
            _ => None,
        })
        .expect("ToolUseStop expected");
    assert_eq!(stop.0, "call_1");
    assert_eq!(stop.1["q"], 1);

    // 末尾は Done（finish_reason=tool_calls → ToolUse・usage 反映）。
    match deltas.last().expect("non-empty") {
        StreamDelta::Done { stop_reason, usage } => {
            assert_eq!(*stop_reason, StopReason::ToolUse);
            assert_eq!(usage.prompt_tokens, 10);
            assert_eq!(usage.completion_tokens, 5);
        }
        other => panic!("last delta should be Done, got {other:?}"),
    }
}

#[tokio::test]
async fn openai_client_error_maps_to_bad_request() {
    let base = spawn_mock(StatusCode::BAD_REQUEST, "bad model").await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "gpt-x");
    let req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);

    let res = provider.stream(&req).await;
    assert!(matches!(res, Err(LlmError::BadRequest(_))));
}

#[tokio::test]
async fn openai_server_error_maps_to_unavailable() {
    let base = spawn_mock(StatusCode::SERVICE_UNAVAILABLE, "down").await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, Some("k".into()), "gpt-x");
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
async fn openai_sends_identity_headers_when_configured() {
    let (base, seen) = spawn_recording_mock(StatusCode::OK, SSE_BODY).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, Some("k".into()), "m")
        .with_user_agent(Some("shiki-test/1.0"))
        .unwrap()
        .with_session_header(Some("x-opencode-session"))
        .unwrap();
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.session_id = Some("thread-42".into());
    drain(&provider, &req).await;
    let headers = seen.lock().unwrap().clone().unwrap();
    assert_eq!(headers["authorization"], "Bearer k");
    assert_eq!(headers["user-agent"], "shiki-test/1.0");
    assert_eq!(headers["x-opencode-session"], "thread-42");
}

#[tokio::test]
async fn openai_sends_no_session_header_when_unset() {
    let (base, seen) = spawn_recording_mock(StatusCode::OK, SSE_BODY).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, Some("k".into()), "m");
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.session_id = Some("thread-42".into());
    drain(&provider, &req).await;
    let headers = seen.lock().unwrap().clone().unwrap();
    assert!(headers.get("x-opencode-session").is_none());
}

/// 実エンドポイントでの往復（`OPENAI_TEST_URL` が無ければ黙ってスキップ）。
///
/// 例（OpenCode Go）: `OPENAI_TEST_URL=https://opencode.ai/zen/go/v1` `OPENAI_TEST_MODEL=qwen3.7-plus`
///     `OPENAI_TEST_SESSION_HEADER=x-opencode-session` `OPENAI_TEST_KEY=<キー>`
#[tokio::test]
async fn openai_live_round_trip() {
    let Ok(base) = std::env::var("OPENAI_TEST_URL") else {
        return;
    };
    let model = std::env::var("OPENAI_TEST_MODEL").unwrap_or_else(|_| "m".into());
    let provider = OpenAiProvider::new(
        reqwest::Client::new(),
        base,
        std::env::var("OPENAI_TEST_KEY").ok(),
        model,
    )
    .with_user_agent(Some("shiki-it/1.0"))
    .unwrap()
    .with_session_header(std::env::var("OPENAI_TEST_SESSION_HEADER").ok().as_deref())
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
