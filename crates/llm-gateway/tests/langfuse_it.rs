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

/// **受信を全件ためる。** 1 件だけ保持する作りだと、
/// 「2 回目が 1 回目を上書きして消える」類の欠陥を検出できない。
type Captured = Arc<Mutex<Vec<Value>>>;

/// 受信した batch を捕捉し 207 を返すハンドラ。
async fn ingest(State(state): State<Captured>, Json(body): Json<Value>) -> StatusCode {
    state.lock().unwrap().push(body);
    StatusCode::MULTI_STATUS
}

/// モックサーバを立てて client を返す。
async fn spawn() -> (LangfuseClient, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
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
    (LangfuseClient::new(reqwest::Client::new(), &cfg), captured)
}

/// テスト用の既定値。差分だけ呼び出し側で上書きする。
fn trace(generation_id: &str, first: bool) -> GenerationTrace<'_> {
    GenerationTrace {
        trace_id: "trace-abc",
        generation_id,
        name: "chat.generation",
        model: "gpt-x",
        provider: "openai",
        input: "in",
        output: "out",
        usage: Usage {
            prompt_tokens: 11,
            completion_tokens: 22,
            ..Default::default()
        },
        user_id: "user-1",
        first_in_trace: first,
        started_at: Some(chrono::Utc::now() - chrono::Duration::seconds(3)),
        metadata: serde_json::json!({ "tenant_id": "t1" }),
    }
}

#[tokio::test]
async fn record_generation_sends_expected_batch_shape() {
    let (client, captured) = spawn().await;
    client.record_generation(trace("gen-1", true)).await;

    let all = captured.lock().unwrap().clone();
    assert_eq!(all.len(), 1);
    let batch = all[0]["batch"].as_array().expect("batch array");
    assert_eq!(batch.len(), 2);

    assert_eq!(batch[0]["type"], "trace-create");
    assert_eq!(batch[0]["body"]["id"], "trace-abc");
    assert_eq!(batch[0]["body"]["metadata"]["tenant_id"], "t1");
    // ユーザー別の集計に要る。metadata に入れるだけでは Langfuse が拾わない。
    assert_eq!(batch[0]["body"]["userId"], "user-1");
    // 一覧に出るのは trace 側。ここが無いと UI で input/output が空に見える。
    assert_eq!(batch[0]["body"]["input"], "in");
    assert_eq!(batch[0]["body"]["output"], "out");

    assert_eq!(batch[1]["type"], "generation-create");
    assert_eq!(batch[1]["body"]["id"], "gen-1");
    assert_eq!(batch[1]["body"]["traceId"], "trace-abc");
    assert_eq!(batch[1]["body"]["model"], "gpt-x");
    assert_eq!(batch[1]["body"]["usage"]["input"], 11);
    assert_eq!(batch[1]["body"]["usage"]["output"], 22);
    assert_eq!(batch[1]["body"]["metadata"]["provider"], "openai");
    // 所要時間。両方無いと UI が 0.00s と表示する。
    assert!(batch[1]["body"]["startTime"].is_string());
    assert!(batch[1]["body"]["endTime"].is_string());
}

/// **同一 trace の複数回の生成が、別々の observation として残ること。**
///
/// Langfuse の ingestion は id で upsert するので、generation の id を
/// trace_id から機械的に作ると 2 回目が 1 回目を上書きして消える。
/// 実環境で 67 呼び出しのうち 22 件が失われていた回帰。
#[tokio::test]
async fn generations_in_same_trace_do_not_overwrite_each_other() {
    let (client, captured) = spawn().await;
    client.record_generation(trace("gen-1", true)).await;
    client.record_generation(trace("gen-2", false)).await;

    let all = captured.lock().unwrap().clone();
    assert_eq!(all.len(), 2, "2 回ぶん送られること");

    let id1 = all[0]["batch"][1]["body"]["id"].clone();
    let id2 = all[1]["batch"][1]["body"]["id"].clone();
    assert_ne!(id1, id2, "generation の id が呼び出しごとに異なること");

    // trace 側は同じ id に集約される（別々の trace に分かれない）。
    assert_eq!(all[0]["batch"][0]["body"]["id"], "trace-abc");
    assert_eq!(all[1]["batch"][0]["body"]["id"], "trace-abc");

    // **2 回目は trace の input を送らない。** 送ると upsert で
    // ユーザーの最初の質問が上書きされて消える。
    assert_eq!(all[0]["batch"][0]["body"]["input"], "in");
    assert!(all[1]["batch"][0]["body"].get("input").is_none());
    // output は毎回送る（最後の応答が残るのが望ましい）。
    assert_eq!(all[1]["batch"][0]["body"]["output"], "out");
}
