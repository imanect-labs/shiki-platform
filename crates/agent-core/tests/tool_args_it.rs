//! モデルが返したツールの引数の JSON が読めないとき、ツールを動かさないことの結合テスト（BUG-225）。
//!
//! 3 つのアダプタ（OpenAI 互換・Anthropic・Responses API）を、決まった SSE を返す送り先に向けた実 LlmGateway で
//! run_agent に通す。必須の項目の無い読むだけのツールが既定の値で動かないこと、副作用のあるツールに壊れた引数が
//! 届かないことを確かめる。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（gateway が組織の AI 設定を引く・会計の行を書く）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use agent_core::{
    run_agent, AgentError, AgentEvent, AgentOptions, AgentStop, EventSink, RunContext, Tool,
    ToolError, ToolOutcome,
};
use async_trait::async_trait;
use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, LlmGateway, Message as LlmMessage, ModelCatalog, ModelEntry, ProviderConfig,
    ProviderKind, Role as LlmRole,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
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
        tenant.into(),
    )
}

/// 呼ばれた回数を数えるツール。`required` が空なら、引数が `{}` でも既定の値で動く（読むだけのツールの形）。
struct CountingTool {
    name: &'static str,
    required: &'static [&'static str],
    read_only: bool,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.name
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "テスト用のツール。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "filter": { "type": "string" }, "id": { "type": "string" } },
            "required": self.required,
        })
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    async fn call(
        &self,
        _ctx: &AuthContext,
        _input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutcome::ok("ok"))
    }
}

/// イベントを貯めるだけの sink。
struct TestSink(Vec<AgentEvent>);

#[async_trait]
impl EventSink for TestSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        self.0.push(event);
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

/// どの要求にも `body` を SSE で返す送り先（接続ごとに 1 応答・`connection: close`）。
async fn spawn_sse(body: String) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                // 要求をヘッダと content-length の本文まで読み切ってから答える。
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

fn gateway(pool: PgPool, kind: ProviderKind, base: String) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind,
            base_url: Some(base),
            api_key: Some("k".into()),
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                ..Default::default()
            }],
        },
        langfuse: None,
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// 読むだけのツール（`list_items`・必須の項目なし）と副作用のあるツール（`delete_item`・`id` が必須）を、どちらも
/// 引数の JSON が途中までの形で呼ぶ応答。`at_limit` なら出力の上限で止まった応答にする。
fn broken_calls(kind: ProviderKind, at_limit: bool) -> String {
    let lines: Vec<String> = match kind {
        ProviderKind::Openai => vec![
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"list_items","arguments":"{\"filter\":"}}]}}]}"#.into(),
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c2","function":{"name":"delete_item","arguments":"{\"id\":\"a1\","}}]}}]}"#.into(),
            format!(
                r#"{{"choices":[{{"delta":{{}},"finish_reason":"{}"}}]}}"#,
                if at_limit { "length" } else { "tool_calls" }
            ),
            "[DONE]".into(),
        ],
        ProviderKind::Anthropic => vec![
            r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#.into(),
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"list_items","input":{}}}"#.into(),
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"filter\":"}}"#.into(),
            r#"{"type":"content_block_stop","index":0}"#.into(),
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"c2","name":"delete_item","input":{}}}"#.into(),
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"id\":\"a1\","}}"#.into(),
            r#"{"type":"content_block_stop","index":1}"#.into(),
            format!(
                r#"{{"type":"message_delta","delta":{{"stop_reason":"{}"}},"usage":{{"output_tokens":3}}}}"#,
                if at_limit { "max_tokens" } else { "tool_use" }
            ),
            r#"{"type":"message_stop"}"#.into(),
        ],
        ProviderKind::OpenaiResponses => vec![
            r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"c1","name":"list_items"}}"#.into(),
            r#"{"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"c1","name":"list_items","arguments":"{\"filter\":"}}"#.into(),
            r#"{"type":"response.output_item.added","item":{"id":"fc_2","type":"function_call","call_id":"c2","name":"delete_item"}}"#.into(),
            r#"{"type":"response.output_item.done","item":{"id":"fc_2","type":"function_call","call_id":"c2","name":"delete_item","arguments":"{\"id\":\"a1\","}}"#.into(),
            if at_limit {
                r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"},"usage":{}}}"#.into()
            } else {
                r#"{"type":"response.completed","response":{"usage":{}}}"#.into()
            },
        ],
        other => panic!("{other:?}"),
    };
    lines
        .iter()
        .map(|l| ["data: ", l, "\n\n"].concat())
        .collect()
}

/// 1 回の run_agent（ツールの呼ばれた回数と結果）。
async fn run(
    pool: &PgPool,
    tenant: &str,
    kind: ProviderKind,
    at_limit: bool,
) -> (usize, usize, Result<AgentStop, AgentError>, Vec<AgentEvent>) {
    let base = spawn_sse(broken_calls(kind, at_limit)).await;
    let gw = gateway(pool.clone(), kind, base);
    let reads = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(CountingTool {
            name: "list_items",
            required: &[],
            read_only: true,
            calls: Arc::clone(&reads),
        }),
        Arc::new(CountingTool {
            name: "delete_item",
            required: &["id"],
            read_only: false,
            calls: Arc::clone(&writes),
        }),
    ];
    let c = ctx(tenant);
    let mut sink = TestSink(Vec::new());
    let out = run_agent(
        &gw,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "一覧を出して a1 を消して")],
        &RunContext {
            ctx: &c,
            idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
            trace_id: None,
            input_preview: String::new(),
            app_id: None,
            owns_trace: true,
        },
        &AgentOptions::chat(4),
        None,
        None,
        &mut sink,
    )
    .await
    .map(|o| o.stop);
    (
        reads.load(Ordering::SeqCst),
        writes.load(Ordering::SeqCst),
        out,
        sink.0,
    )
}

/// 引数の JSON が読めない呼び出しは、3 つのアダプタのどれでもツールを動かさない（BUG-225）。応答が出力の上限で
/// 止まったときは打ち切りとして止まり、それ以外は生成の失敗になる（`{}` にして読むだけのツールを既定の値で
/// 動かしたり、副作用のあるツールへ壊れた呼び出しを届けたりしない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_tool_arguments_never_reach_a_tool() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    for kind in [
        ProviderKind::Openai,
        ProviderKind::Anthropic,
        ProviderKind::OpenaiResponses,
    ] {
        let (reads, writes, out, events) = run(&pool, &tenant, kind, false).await;
        assert_eq!((reads, writes), (0, 0), "{kind:?}: {out:?}");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCall { .. })),
            "{kind:?}: {events:?}"
        );
        match out {
            Err(AgentError::Llm(m)) => assert!(
                m.contains("モデルがツールに渡す値を読める形で返しませんでした"),
                "{kind:?}: {m}"
            ),
            other => panic!("{kind:?}: {other:?}"),
        }

        let (reads, writes, out, _) = run(&pool, &tenant, kind, true).await;
        assert_eq!((reads, writes), (0, 0), "{kind:?}: {out:?}");
        assert_eq!(out.unwrap(), AgentStop::Truncated, "{kind:?}");
    }
    // 打ち切りで終えた生成の会計の行を片付ける。
    sqlx::query("DELETE FROM llm_usage WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}
