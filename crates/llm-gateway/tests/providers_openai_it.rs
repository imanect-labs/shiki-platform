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

use llm_gateway::model::{Block, GenerateRequest, Message, Role, StopReason, StreamDelta, ToolDef};
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
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":4,\"cache_write_tokens\":3}}}\n",
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
            // prompt_tokens の内訳（#581）。
            assert_eq!(usage.cache_read_tokens, 4);
            assert_eq!(usage.cache_write_tokens, 3);
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

/// 本文の先頭に `<think>…</think>` を入れて返すモデルの SSE（タグがチャンク境界で分かれる）。
const SSE_THINK_IN_CONTENT: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"<thi\"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"nk>考え中</th\"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"ink>\\n\\n答えは2\"}}]}\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
    "data: [DONE]\n",
);

#[tokio::test]
async fn openai_think_tags_in_content_become_thinking() {
    let base = spawn_mock(StatusCode::OK, SSE_THINK_IN_CONTENT).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "m")
        .with_think_tag_models(["m".to_string()]);
    let req = GenerateRequest::new(vec![Message::text(Role::User, "1+1")]);
    let deltas = drain(&provider, &req).await;
    assert_eq!(collect(&deltas, true), "考え中");
    assert_eq!(collect(&deltas, false), "答えは2");
}

#[tokio::test]
async fn openai_think_tags_pass_through_for_models_not_flagged() {
    // 対象に指定していないモデルは、先頭の `<think>` も本文のまま（XML 生成などで
    // 正規の出力が思考と取り違えられて消えないように・#584）。
    let base = spawn_mock(StatusCode::OK, SSE_THINK_IN_CONTENT).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "m")
        .with_think_tag_models(["other".to_string()]);
    let req = GenerateRequest::new(vec![Message::text(Role::User, "1+1")]);
    let deltas = drain(&provider, &req).await;
    assert_eq!(collect(&deltas, true), "");
    assert_eq!(collect(&deltas, false), "<think>考え中</think>\n\n答えは2");
}

/// Gemini API の OpenAI 互換口（Gemma 4）が返す形（2026-10-19 に実測した応答を縮めたもの）。
/// 思考は本文の `<thought>`、ツール呼び出しに `index` が無く `extra_content` に
/// `thought_signature` が付き、ツールを呼んでも `finish_reason` は `stop`。
const SSE_GEMINI: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"<thought>天気を調べる\",\"extra_content\":{\"google\":{\"thought\":true}},\"role\":\"assistant\"},\"index\":0}]}\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"</thought>\",\"role\":\"assistant\",\"tool_calls\":[",
    "{\"extra_content\":{\"google\":{\"thought_signature\":\"c2lnLTE=\"}},\"function\":{\"arguments\":\"{\\\"city\\\":\\\"東京\\\"}\",\"name\":\"get_weather\"},\"id\":\"call_1\",\"type\":\"function\"},",
    "{\"function\":{\"arguments\":\"{\\\"city\\\":\\\"大阪\\\"}\",\"name\":\"get_weather\"},\"id\":\"call_2\",\"type\":\"function\"}",
    "]},\"index\":0}]}\n",
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":\"stop\",\"index\":0}],\"usage\":{\"completion_tokens\":16,\"prompt_tokens\":44,\"total_tokens\":101}}\n",
    "data: [DONE]\n",
);

#[tokio::test]
async fn openai_gemini_shaped_stream_runs_tools_and_keeps_signatures() {
    let base = spawn_mock(StatusCode::OK, SSE_GEMINI).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "gemma")
        .with_think_tag_models(["gemma".to_string()]);
    let req = GenerateRequest::new(vec![Message::text(Role::User, "天気")]);
    let deltas = drain(&provider, &req).await;

    assert_eq!(collect(&deltas, true), "天気を調べる");
    assert_eq!(collect(&deltas, false), "");
    // index が無くても 2 つの呼び出しが混ざらない。
    let stops: Vec<(String, String)> = deltas
        .iter()
        .filter_map(|d| match d {
            StreamDelta::ToolUseStop { id, input } => {
                Some((id.clone(), input["city"].as_str()?.to_string()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        stops,
        [
            ("call_1".to_string(), "東京".to_string()),
            ("call_2".to_string(), "大阪".to_string())
        ]
    );
    // 署名は付いてきた呼び出しの分だけ、送り返せる形で残る。
    let items: Vec<&serde_json::Value> = deltas
        .iter()
        .filter_map(|d| match d {
            StreamDelta::ReasoningItem { item, .. } => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["tool_call_id"], "call_1");
    assert_eq!(
        items[0]["extra_content"]["google"]["thought_signature"],
        "c2lnLTE="
    );
    // finish_reason は stop でも、呼び出しがあるのでツールのターンになる。
    assert!(matches!(
        deltas.last(),
        Some(StreamDelta::Done {
            stop_reason: StopReason::ToolUse,
            ..
        })
    ));
}

/// 呼び出しが無い `stop` は EndTurn、`length` は呼び出しがあっても MaxTokens、
/// finish_reason が届かずに切れたストリームは昇格させない。
#[tokio::test]
async fn openai_stop_is_promoted_only_with_tool_calls_and_a_real_stop() {
    const CALL: &str = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"c1\",\"function\":{\"name\":\"a\",\"arguments\":\"{}\"}}]}}]}\n";
    let cases: [(String, StopReason); 3] = [
        (
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\ndata: [DONE]\n".into(),
            StopReason::EndTurn,
        ),
        (
            format!("{CALL}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"length\"}}]}}\ndata: [DONE]\n"),
            StopReason::MaxTokens,
        ),
        (format!("{CALL}data: [DONE]\n"), StopReason::EndTurn),
    ];
    for (body, want) in cases {
        let body: &'static str = Box::leak(body.into_boxed_str());
        let base = spawn_mock(StatusCode::OK, body).await;
        let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "m");
        let req = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
        let deltas = drain(&provider, &req).await;
        match deltas.last() {
            Some(StreamDelta::Done { stop_reason, .. }) => assert_eq!(*stop_reason, want, "{body}"),
            other => panic!("{other:?}"),
        }
    }
}

/// 本文（false）か思考（true）のデルタをつなげる。
fn collect(deltas: &[StreamDelta], thinking: bool) -> String {
    deltas
        .iter()
        .filter_map(|d| match (d, thinking) {
            (StreamDelta::TextDelta { text }, false)
            | (StreamDelta::ThinkingDelta { text }, true) => Some(text.as_str()),
            _ => None,
        })
        .collect()
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
        model.clone(),
    )
    // 本文に `<think>` を入れるモデル（minimax-m3 等）でも検証できるよう、対象に指定する
    // （入れないモデルには影響しない）。
    .with_think_tag_models([model])
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
    // 本文に思考を `<think>` で入れるモデル（minimax-m3 等）でも回答に残らない（#584）。
    assert!(!text.contains("<think>"), "answer: {text}");
    assert!(
        matches!(deltas.last(), Some(StreamDelta::Done { usage, .. }) if usage.prompt_tokens > 0)
    );
}

/// 実エンドポイントでツールを 1 往復させる（`OPENAI_TEST_URL` が無ければ黙ってスキップ）。
///
/// 署名を送り返さないと 2 往復目で落ちるプロバイダ（Gemini API の OpenAI 互換口の Gemma 4）を
/// 確かめる。例: `OPENAI_TEST_URL=https://generativelanguage.googleapis.com/v1beta/openai`
///     `OPENAI_TEST_MODEL=gemma-4-31b-it` `OPENAI_TEST_KEY=<キー>`
#[tokio::test]
async fn openai_live_tool_round_trip() {
    let Ok(base) = std::env::var("OPENAI_TEST_URL") else {
        return;
    };
    let model = std::env::var("OPENAI_TEST_MODEL").unwrap_or_else(|_| "m".into());
    let provider = OpenAiProvider::new(
        reqwest::Client::new(),
        base,
        std::env::var("OPENAI_TEST_KEY").ok(),
        model.clone(),
    )
    .with_think_tag_models([model])
    .with_user_agent(Some("shiki-it/1.0"))
    .unwrap()
    .with_session_header(std::env::var("OPENAI_TEST_SESSION_HEADER").ok().as_deref())
    .unwrap();
    let tool = ToolDef::new(
        "weather.get",
        "都市の今日の天気を返す",
        serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
        }),
    );
    let mut req = GenerateRequest::new(vec![Message::text(
        Role::User,
        "weather.get で東京の天気を調べて、一文で答えて",
    )]);
    req.tools = vec![tool];
    req.session_id = Some(format!("shiki-it-{}", uuid::Uuid::new_v4()));
    req.max_tokens = Some(8000);

    let first = drain(&provider, &req).await;
    assert!(
        matches!(
            first.last(),
            Some(StreamDelta::Done {
                stop_reason: StopReason::ToolUse,
                ..
            })
        ),
        "{first:?}"
    );
    // エージェントが積むのと同じ形で履歴を作る（推論アイテム → 呼び出し）。
    let mut assistant = Vec::new();
    let mut results = Vec::new();
    let mut names = std::collections::HashMap::new();
    for d in &first {
        match d {
            StreamDelta::ToolUseStart { id, name } => {
                names.insert(id.clone(), name.clone());
            }
            StreamDelta::ReasoningItem { provider, item } => assistant.push(Block::Reasoning {
                provider: provider.clone(),
                item: item.clone(),
            }),
            StreamDelta::ToolUseStop { id, input } => {
                assistant.push(Block::ToolUse {
                    id: id.clone(),
                    name: names[id].clone(),
                    input: input.clone(),
                });
                results.push(Block::tool_result(
                    id.clone(),
                    "晴れ、最高気温 22 度",
                    false,
                ));
            }
            _ => {}
        }
    }
    assert_eq!(
        names.values().next().map(String::as_str),
        Some("weather.get")
    );
    req.messages.push(Message {
        role: Role::Assistant,
        content: assistant,
    });
    req.messages.push(Message {
        role: Role::Tool,
        content: results,
    });

    let second = drain(&provider, &req).await;
    let text = collect(&second, false);
    assert!(text.contains("22"), "answer: {text}");
    assert!(!text.contains("<thought>"), "answer: {text}");
}
