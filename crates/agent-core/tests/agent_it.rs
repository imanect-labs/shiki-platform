//! `run_agent` ループのエンドツーエンド結合テスト（Task 3.3 / 3.9）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。実 LLM の代わりに決定的 stub
//! プロバイダを使い、**run_agent → llm-gateway(stub) → EventSink** の実コード経路を走らせて、
//! 通常応答・ツール呼出ループ・キャンセル・最大ステップ停止を検証する。
//!
//! stub は「ツールあり＋1 ターン目＋本文が `search:` 始まり」のときだけ最初のツールを 1 回呼ぶ
//! （crates/llm-gateway/src/providers/stub.rs）。この決定的挙動を土台にループの分岐を網羅する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use agent_core::{
    run_agent, AgentError, AgentEvent, AgentOptions, AgentStop, Citation, EventSink, RunContext,
    Tool, ToolError, ToolOutcome,
};
use async_trait::async_trait;
use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, LlmGateway, Message as LlmMessage, ModelCatalog, ModelEntry, ProviderConfig,
    ProviderKind, Role as LlmRole,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

/// イベントを Vec に貯めるだけのインメモリ sink。`cancel` フラグで協調キャンセルを模す。
struct TestSink {
    events: Vec<AgentEvent>,
    cancel: bool,
}

impl TestSink {
    fn new() -> Self {
        TestSink {
            events: Vec::new(),
            cancel: false,
        }
    }

    /// キャンセル要求済みの sink。
    fn cancelled() -> Self {
        TestSink {
            events: Vec::new(),
            cancel: true,
        }
    }

    /// 指定 variant のイベント数（判別子一致で数える）。
    fn count<F: Fn(&AgentEvent) -> bool>(&self, pred: F) -> usize {
        self.events.iter().filter(|e| pred(e)).count()
    }
}

#[async_trait]
impl EventSink for TestSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        self.events.push(event);
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.cancel
    }
}

/// 決定的なモックツール。stub が呼ぶ最初のツールとして提示し、引用付き結果を返す。
struct MockSearchTool;

#[async_trait]
impl Tool for MockSearchTool {
    // literal 返しでも &'static 化できない（本番 DocSearchTool と同じ allow）。
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "doc_search"
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "テスト用の決定的検索ツール。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"]
        })
    }

    async fn call(
        &self,
        _ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let query = input
            .get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(ToolOutcome {
            content: format!("検索結果: {query}"),
            citations: vec![Citation {
                node_id: "n1".into(),
                chunk_id: "c1".into(),
                snippet: "根拠スニペット".into(),
                page: Some(1),
                heading_path: vec!["第1章".into()],
                score: 0.9,
            }],
            // 成果物 1 件（ループが Artifact イベントとして外部化することの検証用）。
            artifacts: vec![agent_core::ArtifactRef {
                node_id: "artifact-n1".into(),
                name: "result.csv".into(),
            }],
            is_error: false,
        })
    }
}

/// テスト DB へ接続しマイグレーションを適用する（未設定ならスキップ）。
async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
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
        tenant.into(),
    )
}

fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            base_url: None,
            api_key: None,
            timeout_secs: 120,
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                real_id: None,
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
            }],
        },
        langfuse: None,
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

fn user_msg(text: &str) -> Vec<LlmMessage> {
    vec![LlmMessage::text(LlmRole::User, text)]
}

fn run_context<'a>(ctx: &'a AuthContext, preview: &str) -> RunContext<'a> {
    RunContext {
        ctx,
        idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
        trace_id: None,
        input_preview: preview.to_string(),
    }
}

/// ツール無し・ツールを呼ばない発話 → 自然終了し Text イベントが流れる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_agent_completes_without_tools() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![];
    let mut sink = TestSink::new();

    let stop = run_agent(
        &gateway,
        &tools,
        user_msg("hello world"),
        &run_context(&c, "hello world"),
        &AgentOptions::default(),
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(stop, AgentStop::Completed, "ツールを呼ばず自然終了する");
    let text: String = sink
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("hello world"),
        "本文が Text で流れる: {text:?}"
    );
    assert_eq!(
        sink.count(|e| matches!(e, AgentEvent::ToolCall { .. })),
        0,
        "ツール呼出は起きない"
    );
}

/// `search:` 始まりの発話 → stub がツールを呼び、ループが dispatch して観測を戻し完了する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_agent_dispatches_tool_then_completes() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockSearchTool)];
    let mut sink = TestSink::new();

    let stop = run_agent(
        &gateway,
        &tools,
        user_msg("search: 経費規程"),
        &run_context(&c, "search: 経費規程"),
        &AgentOptions {
            max_steps: 4,
            ..AgentOptions::default()
        },
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    // 1 ターン目でツールを呼び、2 ターン目は tool_result 済みで自然終了する。
    assert_eq!(stop, AgentStop::Completed, "ツール実行後に完了する");
    assert_eq!(
        sink.count(|e| matches!(e, AgentEvent::ToolCall { name, .. } if name == "doc_search")),
        1,
        "doc_search が 1 回呼ばれる"
    );
    let tool_result_ok = sink.events.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { ok, content, .. } if *ok && content.contains("経費規程")),
    );
    assert!(tool_result_ok, "ツール観測が成功として戻る");
    assert_eq!(
        sink.count(|e| matches!(e, AgentEvent::Citation(_))),
        1,
        "引用が 1 件流れる"
    );
    // 成果物はツール呼び出し ID に紐づいた Artifact イベントとして流れる（Task 4.11）。
    let artifact_ok = sink.events.iter().any(|e| {
        matches!(e, AgentEvent::Artifact { tool_call_id, artifact }
            if !tool_call_id.is_empty() && artifact.name == "result.csv" && artifact.node_id == "artifact-n1")
    });
    assert!(artifact_ok, "成果物イベントが 1 件流れる");
}

/// 事前にキャンセル要求済みの sink → ループはステップ境界で即停止する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_agent_stops_on_cancellation() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![];
    let mut sink = TestSink::cancelled();

    let stop = run_agent(
        &gateway,
        &tools,
        user_msg("hello world"),
        &run_context(&c, "hello world"),
        &AgentOptions::default(),
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(stop, AgentStop::Cancelled, "キャンセルで停止する");
    assert!(
        sink.events.is_empty(),
        "ステップ境界で即停止しイベントは流れない"
    );
}

/// `max_steps=1` でツールが呼ばれ続ける入力 → ステップ上限で MaxSteps 停止する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_agent_stops_on_max_steps() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockSearchTool)];
    let mut sink = TestSink::new();

    let stop = run_agent(
        &gateway,
        &tools,
        user_msg("search: 上限テスト"),
        &run_context(&c, "search: 上限テスト"),
        &AgentOptions {
            max_steps: 1,
            ..AgentOptions::default()
        },
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    // step 0 でツールを呼び観測まで進むが、上限 1 のためループ終端で MaxSteps 停止。
    assert_eq!(stop, AgentStop::MaxSteps, "最大ステップで停止する");
    assert_eq!(
        sink.count(|e| matches!(e, AgentEvent::ToolCall { .. })),
        1,
        "上限内で 1 回はツールを呼ぶ"
    );
}
