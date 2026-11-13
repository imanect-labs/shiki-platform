//! 1 応答の出力上限（`max_tokens`）で**ツール呼び出しが途中で切れた**ときのループの振る舞い。
//!
//! 実測（プレビュー環境・エージェントモード）: 大きな表の編集・委譲の指示文を書いている途中で
//! 出力上限に達すると、プロバイダは解釈できない引数を `{}` にして停止理由 `max_tokens` を返す。
//! ループはこれを「完了ではない停止（Truncated）」として畳み、ツールを実行しないまま run が
//! 終わっていた（画面ではツールカードが出たまま止まる）。切れた呼び出しは実行せずに
//! 「切れた・分けて呼び直せ」と返し、続きを生成させる。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（会計の記録先）。

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
    Block, GatewayConfig, LlmGateway, Message as LlmMessage, ModelCatalog, ModelEntry,
    ProviderConfig, ProviderKind, Role as LlmRole,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

#[derive(Default)]
struct TestSink {
    events: Vec<AgentEvent>,
}

#[async_trait]
impl EventSink for TestSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        self.events.push(event);
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

/// 呼ばれた回数を数えるだけのツール（切れた `{}` で呼ばれていないことを確かめる）。
struct CountingTool(Arc<AtomicUsize>);

#[async_trait]
impl Tool for CountingTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "sheet_edit"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "表を編集するテストツール。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": true })
    }
    async fn call(
        &self,
        _ctx: &AuthContext,
        _input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutcome::ok("ok"))
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

fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
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

fn run_context(ctx: &AuthContext) -> RunContext<'_> {
    RunContext {
        ctx,
        idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
        trace_id: None,
        input_preview: "truncate".to_string(),
        app_id: None,
        owns_trace: true,
    }
}

/// 切れた呼び出しは実行せず「切れた」と返し、run は止まらずに続きを生成して完了する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_tool_call_is_reported_and_the_run_continues() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let calls = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool(Arc::clone(&calls)))];
    let mut sink = TestSink::default();

    let outcome = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "truncate: 表を全部埋めて")],
        &run_context(&c),
        &AgentOptions::autonomous(10, None, 1_000_000_000, 1_000_000_000),
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(
        outcome.stop,
        AgentStop::Completed,
        "切れた呼び出しで run を畳まず、続きを生成して完了する"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "引数が切れた（`{{}}` になった）呼び出しは実行しない"
    );
    // 画面のツールカードを「実行していない」で閉じる（出たまま止まらない）。
    let closed = sink.events.iter().any(|e| {
        matches!(e, AgentEvent::ToolResult { tool_call_id, ok: false, content }
            if tool_call_id == "stubtool_cut" && content.contains("出力上限"))
    });
    assert!(
        closed,
        "切れた呼び出しに失敗の結果を返す: {:?}",
        sink.events
    );
    // モデルへは「分けて呼び直せ」を観測として返している（tool_use と対になる tool_result）。
    let observed = outcome.checkpoint.messages.iter().any(|m| {
        m.role == LlmRole::Tool
            && m.content.iter().any(|b| {
                matches!(b, Block::ToolResult { tool_use_id, content, is_error: true, .. }
                    if tool_use_id == "stubtool_cut" && content.contains("分け"))
            })
    });
    assert!(observed, "{:?}", outcome.checkpoint.messages);
    // 続きの生成で本文が出ている（stub はツール結果を見たら通常応答を返す）。
    let text: String = sink
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("回答"), "続きの本文が出る: {text}");
}

/// 本文の途中で出力上限に達しても run を畳まず、同じ run のまま続きを書かせて完了する
/// （利用者に「続けて」と送り直させない）。続きは直前の本文に連結されて画面に出る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_text_is_continued_in_the_same_run() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool(Arc::new(AtomicUsize::new(0))))];
    let mut sink = TestSink::default();

    let outcome = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(
            LlmRole::User,
            "truncatetext: 報告を書いて",
        )],
        &run_context(&c),
        &AgentOptions::autonomous(10, None, 1_000_000_000, 1_000_000_000),
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(outcome.stop, AgentStop::Completed, "続きを書いて完了する");
    let text: String = sink
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("報告の前半です。表の") && text.contains("続きの行です。回答を終えます。"),
        "切れた本文と続きが 1 つの回答として流れる: {text}"
    );
    assert!(
        !text.contains(llm_gateway::CONTINUATION_MARK),
        "催促は画面に出さない: {text}"
    );
    // 続きの生成の思考は流さない（流すと本文が別ブロックに割れ、表・```ブロックが壊れる）。
    assert!(
        !sink
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Thinking(_))),
        "{:?}",
        sink.events
    );
    let nudges = outcome
        .checkpoint
        .messages
        .iter()
        .filter(|m| {
            m.role == LlmRole::User
                && m.content.iter().any(|b| {
                    matches!(b, Block::Text { text } if text.starts_with(llm_gateway::CONTINUATION_MARK))
                })
        })
        .count();
    assert_eq!(nudges, 1, "{:?}", outcome.checkpoint.messages);
}

/// 催促しても切れ続けるモデルでは、上限回数で Truncated として畳む（ステップ予算を空費しない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_continuation_is_bounded() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool(Arc::new(AtomicUsize::new(0))))];
    let mut sink = TestSink::default();

    let outcome = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "truncatetext:forever")],
        &run_context(&c),
        &AgentOptions::autonomous(20, None, 1_000_000_000, 1_000_000_000),
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(outcome.stop, AgentStop::Truncated);
    // 最初の応答 + 催促 4 回ぶんの続き。
    assert_eq!(
        outcome.checkpoint.spent.steps, 5,
        "{:?}",
        outcome.checkpoint.spent
    );
}

/// 次のステップへ進む予算が無ければ続けず、Truncated のまま畳む（未完の本文を
/// `Budget` の正常終了に見せず、chat が中断の案内を出せるようにする）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_continuation_keeps_truncated_without_budget() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool(Arc::new(AtomicUsize::new(0))))];
    let mut sink = TestSink::default();

    let outcome = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(
            LlmRole::User,
            "truncatetext: 報告を書いて",
        )],
        &run_context(&c),
        &AgentOptions::autonomous(1, None, 1_000_000_000, 1_000_000_000),
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(outcome.stop, AgentStop::Truncated);
    assert_eq!(outcome.checkpoint.spent.steps, 1);
}
