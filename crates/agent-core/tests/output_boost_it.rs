//! 出力上限の引き上げ（agent_observation.rs の `boosted_options`）の結合テスト。
//!
//! OpenAI 互換のモックサーバに製品の Chat Completions アダプタで繋ぎ、`run_agent` を回す。
//!
//! 検証する性質:
//! - 呼び出しが出力上限で切れた直後のステップだけ、出力上限を引き上げて送る。
//! - 引き上げた要求が窓あふれで拒否されたら、圧縮せず・窓あふれのやり直しを使わずに、
//!   引き上げない上限で同じステップをやり直す（vLLM は prompt＋出力上限が窓を超えると拒否する）。
//! - その次のステップは元の上限に戻る。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use agent_core::{
    run_agent, AgentError, AgentEvent, AgentOptions, AgentStop, Checkpoint, EventSink, RunContext,
    Tool, ToolError, ToolOutcome,
};
use async_trait::async_trait;
use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, LlmGateway, Message as LlmMessage, ModelCatalog, ModelEntry, ProviderConfig,
    ProviderKind, Role as LlmRole,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// モックが受けた要求の出力上限（ツールありの要求だけ・順に）。
#[derive(Default)]
struct Mock {
    max_tokens: Vec<u64>,
}

fn sse(chunks: &[Value]) -> String {
    let mut s = String::new();
    for c in chunks {
        s.push_str("data: ");
        s.push_str(&c.to_string());
        s.push('\n');
    }
    s.push_str("data: [DONE]\n");
    s
}

fn respond(body: &Value, mock: &Mutex<Mock>) -> (&'static str, String) {
    let mut m = mock.lock().unwrap();
    let mt = body["max_tokens"].as_u64().unwrap_or(0);
    m.max_tokens.push(mt);
    match m.max_tokens.len() {
        // 1 件目は書き終え、2 件目は引数の途中で出力上限に達する。
        1 => (
            "200 OK",
            sse(&[
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "id": "a", "type": "function",
                     "function": {"name": "note", "arguments": "{\"text\": \"1\"}"}},
                    {"index": 1, "id": "b", "type": "function",
                     "function": {"name": "note", "arguments": "{\"text\": \"長い本"}}]}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "length"}]}),
            ]),
        ),
        // 引き上げた要求は窓あふれで拒否する（vLLM と同じ文言）。
        _ if mt > 1_000 => (
            "400 Bad Request",
            json!({"error": {"message": format!(
                "This model's maximum context length is 10000 tokens. However, you requested {mt} output tokens"
            )}})
            .to_string(),
        ),
        // 引き上げずにやり直した要求で、切れた分を書き直す。
        3 => (
            "200 OK",
            sse(&[
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "id": "c", "type": "function",
                     "function": {"name": "note", "arguments": "{\"text\": \"2\"}"}}]}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
            ]),
        ),
        _ => (
            "200 OK",
            sse(&[
                json!({"choices": [{"delta": {"content": "書き終えました。"}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            ]),
        ),
    }
}

/// 1 接続 1 リクエストの最小 HTTP サーバ（Content-Length の本文を読み、SSE を返して閉じる）。
async fn spawn_mock(mock: Arc<Mutex<Mock>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mock = Arc::clone(&mock);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 16 * 1024];
                let (head_end, len) = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                        break (pos + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap();
                let (status, payload) = respond(&body, &mock);
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{payload}",
                    payload.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// 各ステップのチェックポイントとイベントを残す sink。
struct Recording {
    events: Vec<AgentEvent>,
    checkpoints: Vec<Checkpoint>,
}

#[async_trait]
impl EventSink for Recording {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        self.events.push(event);
        Ok(())
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    async fn save_checkpoint(&mut self, c: &Checkpoint) -> Result<(), AgentError> {
        self.checkpoints.push(c.clone());
        Ok(())
    }
}

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// メモを書く（変更を伴うツール）。
struct Note;

#[async_trait]
impl Tool for Note {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "note"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "メモを書く。"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    async fn call(
        &self,
        _ctx: &AuthContext,
        _input: Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok("書きました"))
    }
}

/// 窓 10k・出力 1k。2 件目の呼び出しが切れ → 引き上げ（2k）が拒否 → 1k でやり直して書き直す。
#[tokio::test]
async fn boosted_output_limit_falls_back_when_the_window_refuses_it() {
    let Some(pool) = setup().await else { return };
    let mock = Arc::new(Mutex::new(Mock::default()));
    let base = spawn_mock(Arc::clone(&mock)).await;
    let gateway = LlmGateway::build(
        pool,
        reqwest::Client::new(),
        GatewayConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Openai,
                base_url: Some(base),
                ..Default::default()
            },
            catalog: ModelCatalog {
                default_model: "m".into(),
                models: vec![ModelEntry {
                    id: "m".into(),
                    context_window_tokens: Some(10_000),
                    ..Default::default()
                }],
            },
            ..Default::default()
        },
    )
    .unwrap();
    let c = ctx(&format!("t-{}", uuid::Uuid::new_v4()));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(Note)];
    let mut opts = AgentOptions::autonomous(10, None, 10_000_000, 10_000_000);
    opts.max_tokens = Some(1_000);
    opts.offer_plan_tool = false;
    opts.tool_search.enabled = false;
    let mut sink = Recording {
        events: Vec::new(),
        checkpoints: Vec::new(),
    };
    let out = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "メモを 2 つ書いて")],
        &RunContext {
            ctx: &c,
            idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
            trace_id: None,
            input_preview: "boost".into(),
            app_id: None,
            owns_trace: true,
        },
        &opts,
        None,
        None,
        &mut sink,
    )
    .await
    .unwrap();
    let seen = mock.lock().unwrap().max_tokens.clone();
    assert_eq!(
        seen,
        vec![1_000, 2_000, 1_000, 1_000],
        "引き上げ → 拒否 → 元の上限でやり直し → 元の上限"
    );
    assert!(matches!(out.stop, AgentStop::Completed), "{:?}", out.stop);
    // 窓あふれのやり直し（圧縮）は使っていない。
    assert!(!sink.events.iter().any(|e| matches!(
        e,
        AgentEvent::FailureRecovery { action, .. } if *action == agent_core::RecoveryAction::Compacted
    )));
    // 切れた呼び出しは「切れた」と返り、書き直した呼び出しは実行された。
    let s = format!("{:?}", out.checkpoint.messages);
    assert!(s.contains("[出力上限]") && s.contains("\"c\""), "{s}");
}
