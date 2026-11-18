//! 圧縮（compaction.rs）が**長い run で繰り返し**正しく発火するかの結合テスト。
//!
//! OpenAI 互換のモックサーバ（ツールあり＝読み取りの呼び出し 2 件・ツールなし＝短い要約）に
//! 製品の Chat Completions アダプタで繋ぎ、`run_agent` を回す。stub プロバイダは要約に依頼文を
//! そのまま返すため縮まず、繰り返しの発火を確かめられない。
//!
//! 検証する性質:
//! - 窓の soft を超えるたびに圧縮が発火する（1 run で複数回・直後のステップでも必要なら発火する）。
//! - 要約していない結果は、剪定（非常閾値）で畳まれない（どのステップのチェックポイントにも
//!   畳み込みの注記が無い）。
//! - 2 回目以降の要約依頼には前回の要約が入り、履歴の要約ブロックは常に 1 つで最新のもの。
//! - ツール呼び出しと結果の対応が保たれる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use agent_core::{
    run_agent, AgentError, AgentEvent, AgentOptions, Checkpoint, EventSink, RecoveryAction,
    RunContext, Tool, ToolError, ToolOutcome,
};
use async_trait::async_trait;
use authz::{AuthContext, Principal};
use llm_gateway::{
    Block, GatewayConfig, LlmGateway, Message as LlmMessage, ModelCatalog, ModelEntry,
    ProviderConfig, ProviderKind, Role as LlmRole,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// モックの状態（要約の回数・要約依頼に前回の要約が入っていたか・ツールのターン数）。
#[derive(Default)]
struct Mock {
    /// 要約に足す埋め草の字数（大きい要約を返す場合）。
    summary_pad: usize,
    /// 報告する prompt トークンが手元の見積りの何倍か（重いトークナイザの模擬。0 = 報告しない）。
    tokenizer_ratio: u64,
    tool_turns: usize,
    summaries: usize,
    prompts_with_previous: Vec<bool>,
}

/// ツールのターンを何回返したら終えるか。
const TOOL_TURNS: usize = 12;

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

/// agent-core の見積りと同じ数え方（ASCII 4 バイト ≒ 1・それ以外 1 字 ≒ 1）。
fn est(s: &str) -> u64 {
    let ascii = s.bytes().filter(u8::is_ascii).count() as u64;
    ascii / 4 + s.chars().filter(|c| !c.is_ascii()).count() as u64
}

fn respond(body: &Value, mock: &Mutex<Mock>) -> String {
    let mut m = mock.lock().unwrap();
    let usage = json!({"choices": [], "usage": {
        "prompt_tokens": m.tokenizer_ratio * (est(&body["messages"].to_string()) + est(&body["tools"].to_string())),
        "completion_tokens": 10}});
    let has_tools = body["tools"].as_array().is_some_and(|t| !t.is_empty());
    if !has_tools {
        // 要約係: 前回の要約が依頼に入っているかを記録し、短い要約を返す。
        m.summaries += 1;
        let prompt = body["messages"].to_string();
        m.prompts_with_previous
            .push(prompt.contains("前回までの要約"));
        let text = format!(
            "## 確認済みの事実\n- 要約{}: ここまでに読んだ報告の値（売上 1234 万円）を保持する。{}\n## 次の一手\n続きを読む",
            m.summaries,
            "値".repeat(m.summary_pad)
        );
        return sse(&[
            json!({"choices": [{"delta": {"content": text}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            usage,
        ]);
    }
    if m.tool_turns >= TOOL_TURNS {
        return sse(&[
            json!({"choices": [{"delta": {"content": "全部読み終えました。"}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        ]);
    }
    m.tool_turns += 1;
    let n = m.tool_turns;
    // 並列に 2 件読む（1 ステップの結果は窓の 3 割に収まる大きさ）。
    let calls: Vec<Value> = (0..2)
        .map(|i| {
            json!({"index": i, "id": format!("c{n}_{i}"), "type": "function",
                   "function": {"name": "big_read", "arguments": json!({"id": format!("{n}-{i}")}).to_string()}})
        })
        .collect();
    sse(&[
        json!({"choices": [{"delta": {"tool_calls": calls}}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        usage,
    ])
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

/// 窓 10k・出力 1k（有効窓 9k・soft 5.4k・hard 7.65k・1 ステップの結果 2.7k）。
#[tokio::test]
async fn compaction_fires_repeatedly_without_folding_unsummarized_results() {
    let steps = run_scenario(0, 0, 1_300).await;
    if let Some(steps) = steps {
        // 冷却（直後のステップは要約しない）で、1 つおきに発火する。
        assert!(steps.windows(2).all(|w| w[1] - w[0] >= 2), "{steps:?}");
    }
}

/// 要約が大きく（有効窓の 3 割弱）、圧縮直後のステップで非常閾値を超える場合でも、
/// 冷却を待たずに圧縮して、未要約の結果を剪定で畳まない。
#[tokio::test]
async fn large_summaries_compact_again_before_the_hard_limit() {
    let steps = run_scenario(2_400, 0, 1_300).await;
    if let Some(steps) = steps {
        assert!(steps.windows(2).any(|w| w[1] - w[0] == 1), "{steps:?}");
    }
}

/// 実トークンが手元の見積りの数倍のモデル（日本語で重いトークナイザ・実測で Claude Haiku 5.5）でも、
/// 閾値を超えるたびに要約できる（尻尾の予算を見積りの単位に直して切る）。直さないと、見積りでは
/// 前回の圧縮以降が全部「直近」に収まり、要約に回す部分が無いまま窓に近づいていく。
#[tokio::test]
async fn heavy_tokenizer_still_compacts_repeatedly() {
    // 小さい結果のターンが続く（実測: 30 件の処理で、圧縮の後は短い照会と処理だけが続いた）。
    let steps = run_scenario(0, 3, 200).await;
    if let Some(steps) = steps {
        assert!(steps.len() >= 2, "{steps:?}");
    }
}

/// シナリオを回し、圧縮したステップ（送ったリクエスト数で数えた位置）を返す。DB が無ければ `None`。
async fn run_scenario(
    summary_pad: usize,
    tokenizer_ratio: u64,
    result_chars: usize,
) -> Option<Vec<usize>> {
    let pool = setup().await?;
    let mock = Arc::new(Mutex::new(Mock {
        summary_pad,
        tokenizer_ratio,
        ..Mock::default()
    }));
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
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(BigRead {
        chars: result_chars,
    })];
    let mut opts = AgentOptions::autonomous(20, None, 10_000_000, 10_000_000);
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
        vec![LlmMessage::text(LlmRole::User, "報告を全部読んで集計して")],
        &RunContext {
            ctx: &c,
            idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
            trace_id: None,
            input_preview: "repeat".into(),
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

    // 圧縮が発火したステップ（ContextUsage の数＝それまでに送ったリクエスト数で位置を取る）。
    let mut steps_at = Vec::new();
    let mut usages = 0;
    let mut peak = 0;
    for e in &sink.events {
        match e {
            AgentEvent::ContextUsage(u) => {
                usages += 1;
                peak = peak.max(u.used_tokens);
            }
            AgentEvent::FailureRecovery { action, .. } if *action == RecoveryAction::Compacted => {
                steps_at.push(usages);
            }
            _ => {}
        }
    }
    let m = mock.lock().unwrap();
    eprintln!(
        "compactions at steps {steps_at:?} / summaries {} / with previous {:?} / peak {peak}",
        m.summaries, m.prompts_with_previous
    );
    assert_eq!(m.tool_turns, TOOL_TURNS, "最後まで読み終える");
    // 報告された実 prompt は、窓（10k）− 出力上限（1k）を超えない。
    if tokenizer_ratio > 0 {
        assert!(peak <= 9_000, "peak {peak} が有効窓を超えた: {steps_at:?}");
    }
    assert!(
        steps_at.len() >= 4,
        "12 ターン × 2.6k で何度も発火する: {steps_at:?}"
    );
    assert_eq!(
        m.summaries,
        steps_at.len(),
        "要約の呼び出しはすべて置き換えに使われた"
    );
    // 2 回目以降の要約依頼には前回の要約が入る。
    assert!(!m.prompts_with_previous[0]);
    assert!(m.prompts_with_previous[1..].iter().all(|p| *p));

    // 要約していない結果を剪定で畳んでいない（どのステップでも）。
    for (i, cp) in sink.checkpoints.iter().enumerate() {
        let s = format!("{:?}", cp.messages);
        assert!(
            !s.contains("古い出力を要約のため省略"),
            "step {i} で未要約の結果が畳まれた（圧縮が間に合っていない）"
        );
    }
    // 要約ブロックは 1 つで最新、ツール呼び出しと結果の対応は保たれる。
    let head = &out.checkpoint.messages[0];
    let blocks: Vec<&str> = head
        .content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } if text.starts_with("［これまでの作業の要約") => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].contains(&format!("要約{}", m.summaries)));
    for (i, msg) in out.checkpoint.messages.iter().enumerate() {
        if msg.role == LlmRole::Tool {
            assert_eq!(out.checkpoint.messages[i - 1].role, LlmRole::Assistant);
        }
    }
    Some(steps_at)
}
