//! 元に戻せる圧縮（archive.rs・agent_archive.rs）の結合テスト。
//!
//! OpenAI 互換のモックサーバに製品の Chat Completions アダプタで繋ぎ、`run_agent` を回す。
//!
//! 検証する性質:
//! - 保存先を渡した run では、読み戻しのメタツール（`recall_read` / `recall_search`）が提示される。
//! - 1 ステップの結果の上限で外した結果は保存され、文脈には参照（`保存済み rN`）が載る。
//!   `recall_read` でその原本が読める（呼び直さなくてよい）。
//! - 要約に置き換えた区間も保存され（`hN`）、要約には索引が付く。要約で消えた古い結果の値を
//!   `recall_search(ref="*")` で探し出せる。
//! - 読み戻しは 1 ステップ 2 回まで（3 回目は実行せずに上限を返す）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use agent_core::{
    run_agent, AgentError, AgentEvent, AgentOptions, ArchiveKind, Checkpoint, ContextArchive,
    EventSink, InMemoryArchive, RunContext, Tool, ToolError, ToolOutcome,
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

/// モックの状態。
#[derive(Default)]
struct Mock {
    turn: usize,
    offered_recall: bool,
    summaries: usize,
    /// 各ターンの直前に届いたツール結果（最後の tool メッセージ群の本文）。
    seen: Vec<String>,
}

/// 大きな読み取りを返すターン数（3 件ずつ・1 ステップの上限を超えて 1 件ずつ外れる）。
const READ_TURNS: usize = 6;

fn call(id: &str, name: &str, args: &Value, index: usize) -> Value {
    json!({"index": index, "id": id, "type": "function",
           "function": {"name": name, "arguments": args.to_string()}})
}

fn tool_turn(calls: &[Value]) -> String {
    sse(&[
        json!({"choices": [{"delta": {"tool_calls": calls}}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ])
}

/// 直近のツール結果（末尾に続く tool メッセージの本文を連結）。
fn last_results(body: &Value) -> String {
    let msgs = body["messages"].as_array().unwrap();
    msgs.iter()
        .rev()
        .take_while(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 文中の最後の `保存済み rN` の参照。
fn last_ref(text: &str) -> Option<String> {
    let at = text.rfind("保存済み r")?;
    let rest = &text[at + "保存済み ".len()..];
    let end = rest[1..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(rest.len(), |e| e + 1);
    Some(rest[..end].to_string())
}

fn respond(body: &Value, mock: &Mutex<Mock>) -> String {
    let mut m = mock.lock().unwrap();
    let tools = body["tools"].as_array().cloned().unwrap_or_default();
    if tools.is_empty() {
        m.summaries += 1;
        let text = format!(
            "## 確認済みの事実\n- 要約{}: 報告を読み進めている（売上はすべて 1234 万円）。\n## 次の一手\n続きを読む",
            m.summaries
        );
        return sse(&[
            json!({"choices": [{"delta": {"content": text}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        ]);
    }
    m.offered_recall = tools.iter().any(|t| t["function"]["name"] == "recall_read")
        && tools
            .iter()
            .any(|t| t["function"]["name"] == "recall_search");
    let seen = last_results(body);
    m.seen.push(seen.clone());
    m.turn += 1;
    let n = m.turn;
    if n <= READ_TURNS {
        let calls: Vec<Value> = (0..3)
            .map(|i| {
                call(
                    &format!("c{n}_{i}"),
                    "big_read",
                    &json!({"id": format!("{n}-{i}")}),
                    i,
                )
            })
            .collect();
        return tool_turn(&calls);
    }
    match n - READ_TURNS {
        // 直前のステップで外れた結果を、参照で読み戻す。
        1 => {
            let r = last_ref(&seen).expect("外した結果の参照が文脈に載る");
            tool_turn(&[call(
                "rr",
                "recall_read",
                &json!({"ref": r, "max_lines": 5}),
                0,
            )])
        }
        // 要約で消えた最初の結果を、全部の記録から探す。3 件目は上限で実行されない。
        2 => tool_turn(&[
            call(
                "rs",
                "recall_search",
                &json!({"ref": "*", "pattern": "報告 1-0:"}),
                0,
            ),
            call(
                "rs2",
                "recall_search",
                &json!({"ref": "*", "pattern": "報告 2-1:"}),
                1,
            ),
            call(
                "rs3",
                "recall_search",
                &json!({"ref": "*", "pattern": "報告 3-1:"}),
                2,
            ),
        ]),
        _ => sse(&[
            json!({"choices": [{"delta": {"content": "集計しました。"}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        ]),
    }
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
                let payload = respond(&body, &mock);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{payload}",
                    payload.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// 読み取り（呼び出しごとに違う本文・日本語 `chars` 字 ≒ `chars` トークンの見積り）。
struct BigRead {
    chars: usize,
}

#[async_trait]
impl Tool for BigRead {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "big_read"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "報告を読む。"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]})
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(
        &self,
        _ctx: &AuthContext,
        input: Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let id = input["id"].as_str().unwrap_or("?");
        Ok(ToolOutcome::ok(format!(
            "報告 {id}: 売上 1234 万円。{}",
            "報".repeat(self.chars)
        )))
    }
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

/// 窓 10k・出力 1k（有効窓 9k・soft 5.4k・1 ステップの結果 2.7k）。1 件 1.3k の読み取りを
/// 3 件ずつ 6 ターン読み、外れた結果と要約した区間を読み戻す。
#[tokio::test]
async fn spilled_results_and_summarized_history_can_be_recalled() {
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
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(BigRead { chars: 1_300 })];
    let archive = Arc::new(InMemoryArchive::new());
    let mut opts = AgentOptions::autonomous(30, None, 10_000_000, 10_000_000);
    opts.max_tokens = Some(1_000);
    opts.offer_plan_tool = false;
    opts.tool_search.enabled = false;
    opts.archive = Some(archive.clone() as Arc<dyn ContextArchive>);
    let mut sink = Recording {
        events: Vec::new(),
        checkpoints: Vec::new(),
    };
    let out = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "報告を全部読んで集計して")],
        &RunContext {
            ctx: &c,
            idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
            trace_id: None,
            input_preview: "archive".into(),
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

    {
        let m = mock.lock().unwrap();
        assert!(m.offered_recall, "読み戻しのメタツールが提示される");
        assert!(
            m.summaries >= 2,
            "6 ターンで圧縮が何度か発火する: {}",
            m.summaries
        );
        // 外した結果は参照（先頭・末尾つき）で載り、読み戻すと原本が返る。
        let recalled = &m.seen[READ_TURNS + 1];
        assert!(recalled.contains("保存済みの記録 r"), "{recalled}");
        assert!(recalled.contains("売上 1234 万円"), "{recalled}");
        assert!(recalled.contains("    1| 報告"), "{recalled}");
        // 要約で消えた最初の結果も、全部の記録から探せる。3 件目は上限で実行しない。
        let searched = &m.seen[READ_TURNS + 2];
        assert!(
            searched.contains("> 報告 1-0: 売上 1234 万円"),
            "{searched}"
        );
        assert!(searched.contains("1 ステップ 2 回まで"), "{searched}");
        assert_eq!(out.checkpoint.recalls, 4, "上限で返したものも数える");
        // 保存先には外した結果と要約した区間の両方がある。
    }
    let items = archive.list(&c).await.unwrap();
    assert!(items.iter().any(|i| i.kind == ArchiveKind::ToolResult));
    assert!(items.iter().any(|i| i.kind == ArchiveKind::History));
    // 要約には索引が付く。
    let head = format!("{:?}", out.checkpoint.messages[0]);
    assert!(
        head.contains("保存済みの記録（recall_read / recall_search で読める"),
        "{head}"
    );
    assert!(head.contains("- h"), "{head}");
    assert!(!sink.events.is_empty());
}
