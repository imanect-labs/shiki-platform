//! OpenAI 互換プロバイダの SSE ストリーミング結線テスト（ローカル axum モック）。
//!
//! 127.0.0.1 の ephemeral port に `/chat/completions` 相当のモックを立て、実際の
//! バイトストリーム経路（`OpenAiProvider::stream`）が中立 [`StreamDelta`] 列へ正しく
//! 写すことと、4xx/5xx のエラーマッピングを検証する。DB 非依存（provider 直叩き）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::http::StatusCode;
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
