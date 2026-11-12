//! prompt caching（T00）の単体。opt-in のモデルだけ、要求のトップレベルに自動キャッシュを付ける。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::providers::anthropic_headers::tests::Logs;

const HAIKU: &str = "claude-haiku-5-5";

fn provider(cached: &[&str]) -> AnthropicProvider {
    AnthropicProvider::new(
        reqwest::Client::new(),
        "https://api.anthropic.com",
        "k",
        HAIKU,
    )
    .with_prompt_cache_models(cached.iter().map(|m| (*m).to_string()))
}

/// system・ツール・往復の済んだ履歴を持つ要求（キャッシュの区切りが置かれうる形をひととおり含む）。
fn request(model: Option<&str>) -> GenerateRequest {
    let mut req = GenerateRequest::new(vec![
        Message::text(Role::User, "表を作って"),
        Message {
            role: Role::Assistant,
            content: vec![Block::ToolUse {
                id: "t1".into(),
                name: "data.create".into(),
                input: json!({ "name": "顧客" }),
            }],
        },
        Message {
            role: Role::Tool,
            content: vec![Block::tool_result("t1", "作りました", false)],
        },
    ]);
    req.model = model.map(str::to_string);
    req.system = Some("あなたはアプリを作る手伝いをします。".into());
    req.tools = vec![ToolDef::new(
        "data.create",
        "表を作る",
        json!({ "type": "object" }),
    )];
    req
}

fn body(p: &AnthropicProvider, req: &GenerateRequest) -> Value {
    p.build_body(req, &ToolNameMap::for_request(req))
}

/// 本文に現れる `cache_control` の数（入れ子のブロックも数える）。
fn cache_controls(v: &Value) -> usize {
    match v {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| usize::from(k == "cache_control") + cache_controls(v))
            .sum(),
        Value::Array(items) => items.iter().map(cache_controls).sum(),
        _ => 0,
    }
}

#[test]
fn opted_out_models_keep_the_body_unchanged() {
    // 既定（何も opt-in しない）と、ほかのモデルだけを opt-in した送り先で、本文は同じで区切りも無い。
    let req = request(Some(HAIKU));
    let plain = body(&provider(&[]), &req);
    assert_eq!(body(&provider(&["claude-opus-5-5"]), &req), plain);
    assert_eq!(cache_controls(&plain), 0, "{plain}");
}

#[test]
fn opted_in_models_get_only_the_top_level_marker() {
    let req = request(Some(HAIKU));
    let cached = body(&provider(&[HAIKU]), &req);
    // トップレベルにだけ 5 分（ttl を書かない）の自動キャッシュ。ブロックには付けない。
    assert_eq!(cached["cache_control"], json!({ "type": "ephemeral" }));
    assert_eq!(cache_controls(&cached), 1, "{cached}");
    // system は文字列のまま（評価の取り込みが文字列を前提にする）。
    assert_eq!(cached["system"], "あなたはアプリを作る手伝いをします。");
    // それ以外は opt-out と同じ本文。
    let mut without = cached.clone();
    without.as_object_mut().unwrap().remove("cache_control");
    assert_eq!(without, body(&provider(&[]), &req));
    // モデルを書かない要求は既定のモデル（実 ID）で判定する。
    assert_eq!(
        body(&provider(&[HAIKU]), &request(None))["cache_control"]["type"],
        "ephemeral"
    );
}

/// 応答の台本（状態・追加のヘッダ・本文）。
type Script = Vec<(u16, Vec<(&'static str, &'static str)>, &'static str)>;

/// 台本を順に返し（尽きたら最後）、受けた要求の本文を残すモック。
async fn server(script: Script) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let record = Arc::clone(&seen);
    let app = axum::Router::new().fallback(move |req: axum::body::Bytes| {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        record
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&req).unwrap());
        let (status, headers, text) = script[n.min(script.len() - 1)].clone();
        async move {
            let mut h = axum::http::HeaderMap::new();
            for (name, value) in headers {
                h.insert(name, value.parse().unwrap());
            }
            (axum::http::StatusCode::from_u16(status).unwrap(), h, text)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

/// 送り直した要求にも区切りが載り、送り直しの warn と成功の debug にレート制限の残りが出る（組織の ID は出ない）。
#[tokio::test]
async fn sent_requests_carry_the_marker_and_logs_carry_the_remaining_limits() {
    const ORG: &str = "org-0c4e-secret";
    const ANSWER: &str = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3,\"cache_read_input_tokens\":2048,\"cache_creation_input_tokens\":0}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"答え\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (base, seen) = server(vec![
        (
            429,
            vec![
                ("retry-after", "0"),
                ("anthropic-ratelimit-input-tokens-remaining", "0"),
                (
                    "anthropic-ratelimit-input-tokens-reset",
                    "2026-11-12T03:04:05Z",
                ),
                ("anthropic-organization-id", ORG),
            ],
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
        ),
        (
            200,
            vec![
                ("content-type", "text/event-stream"),
                ("anthropic-ratelimit-requests-remaining", "3998"),
                ("anthropic-organization-id", ORG),
            ],
            ANSWER,
        ),
    ])
    .await;
    let logs = Logs::default();
    let _guard = logs.capture();
    let p = AnthropicProvider::new(reqwest::Client::new(), base, "k", HAIKU)
        .with_prompt_cache_models([HAIKU.to_string()]);
    let deltas: Vec<_> = p
        .stream(&request(Some(HAIKU)))
        .await
        .unwrap()
        .collect()
        .await;
    let Some(Ok(StreamDelta::Done { usage, .. })) = deltas.last() else {
        panic!("{deltas:?}")
    };
    // 読み出しは入力の総数に含めて数える（会計は読み出しの単価で数える）。
    assert_eq!((usage.prompt_tokens, usage.cache_read_tokens), (2051, 2048));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "429 の後に 1 回送り直す");
    for sent in seen.iter() {
        assert_eq!(sent["cache_control"], json!({ "type": "ephemeral" }));
        assert!(sent["system"].is_string(), "{sent}");
    }

    let text = logs.text();
    let warn = text
        .lines()
        .find(|l| l.contains("WARN"))
        .unwrap_or_default();
    assert!(
        warn.contains("input_tokens_remaining=0")
            && warn.contains("input_tokens_reset=2026-11-12T03:04:05Z"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("DEBUG") && l.contains("requests_remaining=3998")),
        "{text}"
    );
    assert!(!text.contains(ORG), "{text}");
}
