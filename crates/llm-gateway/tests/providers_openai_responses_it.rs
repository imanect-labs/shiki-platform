//! OpenAI Responses API プロバイダの SSE ストリーミング結線テスト（ローカル axum モック）。
//!
//! 127.0.0.1 の ephemeral port に `/responses` 相当のモックを立て、実際のバイトストリーム経路
//! （`OpenAiResponsesProvider::stream`）が中立 [`StreamDelta`] 列へ写すこと、送出ヘッダ
//! （認証・User-Agent・会話 ID）、4xx/429 のエラー写像、完了イベント前の切断を検証する。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use futures::stream::StreamExt;

use llm_gateway::model::{Block, GenerateRequest, Message, Role, StopReason, StreamDelta};
use llm_gateway::provider::{LlmError, LlmProvider};
use llm_gateway::providers::openai_responses::OpenAiResponsesProvider;

type Seen = Arc<Mutex<Option<(HeaderMap, String)>>>;

/// 固定ステータス＋固定ボディを返し、受けたヘッダと本文を記録するモックを立てる。
async fn spawn_mock(status: StatusCode, body: &'static str) -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(None));
    let rec = seen.clone();
    let app = Router::new().fallback(move |headers: HeaderMap, req_body: String| {
        let rec = rec.clone();
        async move {
            *rec.lock().unwrap() = Some((headers, req_body));
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

/// テキスト・ツール呼び出し・完了を含む代表的な SSE（OpenCode Go の実測を縮めたもの）。
const SSE_BODY: &str = concat!(
    "event: response.created\n",
    "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\n",
    "event: response.output_text.delta\n",
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"調べます\"}\n\n",
    "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"doc_search\",\"arguments\":\"\"}}\n\n",
    "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"q\\\":\"}\n\n",
    "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"1}\"}\n\n",
    "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"doc_search\",\"arguments\":\"{\\\"q\\\":1}\"}}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":6}}}}\n\n",
);

fn provider(base: &str) -> OpenAiResponsesProvider {
    OpenAiResponsesProvider::new(reqwest::Client::new(), base, Some("k".into()), "gpt-x")
        .with_user_agent(Some("shiki-test/1.0"))
        .unwrap()
        .with_session_header(Some("x-opencode-session"))
        .unwrap()
}

async fn drain(
    p: &OpenAiResponsesProvider,
    req: &GenerateRequest,
) -> Vec<Result<StreamDelta, LlmError>> {
    let mut stream = p.stream(req).await.expect("stream should start");
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

#[tokio::test]
async fn streams_text_and_tool_call_and_sends_identity_headers() {
    let (base, seen) = spawn_mock(StatusCode::OK, SSE_BODY).await;
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "経費")]);
    req.session_id = Some("thread-42".into());
    let out: Vec<StreamDelta> = drain(&provider(&base), &req)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        out,
        vec![
            StreamDelta::TextDelta {
                text: "調べます".into()
            },
            StreamDelta::ToolUseStart {
                id: "call_1".into(),
                name: "doc_search".into()
            },
            StreamDelta::ToolUseStop {
                id: "call_1".into(),
                input: serde_json::json!({"q": 1})
            },
            StreamDelta::Done {
                stop_reason: StopReason::ToolUse,
                usage: llm_gateway::model::Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    cache_read_tokens: 6,
                    cache_write_tokens: 0,
                },
            },
        ]
    );
    let (headers, body) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(headers["authorization"], "Bearer k");
    assert_eq!(headers["user-agent"], "shiki-test/1.0");
    assert_eq!(headers["x-opencode-session"], "thread-42");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["prompt_cache_key"], "thread-42");
}

#[tokio::test]
async fn missing_session_id_gets_a_per_call_id_when_header_is_configured() {
    let (base, seen) = spawn_mock(StatusCode::OK, SSE_BODY).await;
    let req = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
    drain(&provider(&base), &req).await;
    let (headers, _) = seen.lock().unwrap().clone().unwrap();
    let sid = headers["x-opencode-session"].to_str().unwrap();
    assert!(
        uuid::Uuid::parse_str(sid).is_ok(),
        "per-call uuid expected, got {sid}"
    );
}

#[tokio::test]
async fn stream_cut_before_completed_is_an_error() {
    const CUT: &str = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"途中\"}\n\n";
    let (base, _) = spawn_mock(StatusCode::OK, CUT).await;
    let req = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
    let out = drain(&provider(&base), &req).await;
    assert!(matches!(out[0], Ok(StreamDelta::TextDelta { .. })));
    assert!(matches!(out.last(), Some(Err(LlmError::Unavailable(_)))));
}

#[tokio::test]
async fn http_errors_map_to_bad_request_and_rate_limited() {
    let (base, _) = spawn_mock(
        StatusCode::BAD_REQUEST,
        "{\"type\":\"error\",\"error\":{\"type\":\"ModelProtocolUnsupported\"}}",
    )
    .await;
    let req = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
    let err = provider(&base).stream(&req).await.err().unwrap();
    assert!(matches!(err, LlmError::BadRequest(m) if m.contains("ModelProtocolUnsupported")));

    let (base, _) = spawn_mock(
        StatusCode::TOO_MANY_REQUESTS,
        "{\"error\":{\"message\":\"usage limit reached\"}}",
    )
    .await;
    let err = provider(&base).stream(&req).await.err().unwrap();
    assert!(matches!(err, LlmError::RateLimited(m) if m.contains("usage limit")));
}

/// 実エンドポイント向けのプロバイダ（`OPENAI_RESPONSES_TEST_*` から組む）。
fn live_provider(base: String) -> OpenAiResponsesProvider {
    let model =
        std::env::var("OPENAI_RESPONSES_TEST_MODEL").unwrap_or_else(|_| "gpt-6-luna".into());
    // TLS を中継するプロキシ越しに叩く環境向けに、追加の CA を受け取る（本番の経路は変えない）。
    let mut http = reqwest::Client::builder();
    if let Ok(path) = std::env::var("OPENAI_RESPONSES_TEST_CA") {
        let pem = std::fs::read(path).unwrap();
        for cert in reqwest::Certificate::from_pem_bundle(&pem).unwrap() {
            http = http.add_root_certificate(cert);
        }
    }
    OpenAiResponsesProvider::new(
        http.build().unwrap(),
        base,
        std::env::var("OPENAI_RESPONSES_TEST_KEY").ok(),
        model,
    )
    .with_user_agent(Some("shiki-it/1.0"))
    .unwrap()
    .with_session_header(
        std::env::var("OPENAI_RESPONSES_TEST_SESSION_HEADER")
            .ok()
            .as_deref(),
    )
    .unwrap()
}

/// 実エンドポイントでの往復（`OPENAI_RESPONSES_TEST_URL` が無ければ黙ってスキップ）。
///
/// 例: `OPENAI_RESPONSES_TEST_URL=https://opencode.ai/inference/go/v1`
///     `OPENAI_RESPONSES_TEST_MODEL=gpt-6-luna`
///     `OPENAI_RESPONSES_TEST_SESSION_HEADER=x-opencode-session`（認証はプロキシ注入か `..._KEY`）
///     `OPENAI_RESPONSES_TEST_CA=<PEM>`（TLS を中継するプロキシの CA。必要な環境だけ）
#[tokio::test]
async fn live_round_trip_with_tool_call() {
    let Ok(base) = std::env::var("OPENAI_RESPONSES_TEST_URL") else {
        return;
    };
    let p = live_provider(base);

    // 1 往目: ツールを呼ばせる。
    let tool = llm_gateway::model::ToolDef::new(
        "doc.search",
        "社内文書を検索する",
        serde_json::json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
        }),
    );
    let mut req = GenerateRequest::new(vec![Message::text(
        Role::User,
        "doc.search ツールで「経費規程」を検索してから答えて。",
    )]);
    req.tools = vec![tool.clone()];
    req.session_id = Some(format!("shiki-it-{}", uuid::Uuid::new_v4()));
    req.max_tokens = Some(400);
    let out: Vec<StreamDelta> = drain(&p, &req)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let (id, input) = out
        .iter()
        .find_map(|d| match d {
            StreamDelta::ToolUseStop { id, input } => Some((id.clone(), input.clone())),
            _ => None,
        })
        .expect("tool call expected");
    assert!(out
        .iter()
        .any(|d| matches!(d, StreamDelta::ToolUseStart { name, .. } if name == "doc.search")));
    assert!(input["query"].as_str().is_some_and(|q| q.contains("経費")));

    // 2 往目: ツール結果を返して本文を書かせる。
    req.messages.push(Message {
        role: Role::Assistant,
        content: vec![llm_gateway::model::Block::ToolUse {
            id: id.clone(),
            name: "doc.search".into(),
            input,
        }],
    });
    req.messages.push(Message {
        role: Role::Tool,
        content: vec![llm_gateway::model::Block::ToolResult {
            tool_use_id: id,
            content: "経費規程: 5万円以上は部長承認が必要。".into(),
            is_error: false,
            tool_references: Vec::new(),
        }],
    });
    let out: Vec<StreamDelta> = drain(&p, &req)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let text: String = out
        .iter()
        .filter_map(|d| match d {
            StreamDelta::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("部長"),
        "answer should use the tool result: {text}"
    );
    assert!(matches!(
        out.last(),
        Some(StreamDelta::Done { stop_reason: StopReason::EndTurn, usage }) if usage.completion_tokens > 0
    ));
}

#[tokio::test]
async fn endless_line_without_newline_is_cut_off_with_an_error() {
    // 改行を送らないまま 9 MiB を超える 1 行（上限 8 MiB）。
    static HUGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let body: &'static str = HUGE.get_or_init(|| format!("data: {}", "x".repeat(9 * 1024 * 1024)));
    let (base, _) = spawn_mock(StatusCode::OK, body).await;
    let req = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
    let out = drain(&provider(&base), &req).await;
    assert!(matches!(out.last(), Some(Err(LlmError::Unavailable(m))) if m.contains("size limit")));
}

/// 推論アイテムの往復（#542）: 推論 → ツール呼び出し → 推論アイテムを先頭に戻した 2 ターン目。
#[tokio::test]
async fn live_reasoning_items_round_trip() {
    let Ok(base) = std::env::var("OPENAI_RESPONSES_TEST_URL") else {
        return;
    };
    let p = live_provider(base);
    let tool = llm_gateway::model::ToolDef::new(
        "calc",
        "四則演算を正確に計算する",
        serde_json::json!({
            "type": "object",
            "properties": {"expr": {"type": "string"}},
            "required": ["expr"],
        }),
    );
    let mut req = GenerateRequest::new(vec![Message::text(
        Role::User,
        "原価 1,280 円・送料 95 円で、粗利率 35% を確保する税抜の販売価格（円未満切り上げ）を、calc ツールで検算しながら求めて。",
    )]);
    req.tools = vec![tool];
    req.effort = Some(llm_gateway::model::Effort::High);
    req.session_id = Some(format!("shiki-it-{}", uuid::Uuid::new_v4()));
    // ツールを呼ぶ限り、推論アイテムを先頭に積んだ assistant メッセージとツール結果を戻して続ける。
    let mut saw_reasoning = false;
    let mut text = String::new();
    for _ in 0..4 {
        let out: Vec<StreamDelta> = drain(&p, &req)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let mut content = Vec::new();
        let mut names = std::collections::HashMap::new();
        text.clear();
        for d in &out {
            match d {
                StreamDelta::ReasoningItem { provider, item } => {
                    saw_reasoning = true;
                    content.push(Block::Reasoning {
                        provider: provider.clone(),
                        item: item.clone(),
                    });
                }
                StreamDelta::TextDelta { text: t } => text.push_str(t),
                StreamDelta::ToolUseStart { id, name } => {
                    names.insert(id.clone(), name.clone());
                }
                StreamDelta::ToolUseStop { id, input } => content.push(Block::ToolUse {
                    id: id.clone(),
                    name: names[id].clone(),
                    input: input.clone(),
                }),
                _ => {}
            }
        }
        let results: Vec<Block> = content
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse { id, input, .. } => Some(Block::ToolResult {
                    tool_use_id: id.clone(),
                    content: match input["expr"].as_str().unwrap_or_default() {
                        e if e.contains("0.35") || e.contains("0.65") => "2115.3846".into(),
                        e if e.contains('+') && !e.contains('/') => "1375".into(),
                        _ => "35.02".into(),
                    },
                    is_error: false,
                    tool_references: Vec::new(),
                }),
                _ => None,
            })
            .collect();
        if results.is_empty() {
            break;
        }
        req.messages.push(Message {
            role: Role::Assistant,
            content,
        });
        req.messages.push(Message {
            role: Role::Tool,
            content: results,
        });
    }
    assert!(saw_reasoning, "推論アイテムが返ること");
    assert!(
        text.contains("2,116") || text.contains("2116"),
        "回答: {text}"
    );
}
