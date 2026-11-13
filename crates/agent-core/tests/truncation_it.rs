//! 1 応答の出力上限（`max_tokens`）で**ツール呼び出しが途中で切れた**ときのループの振る舞い。
//!
//! 実測（プレビュー環境・エージェントモード）: 大きな表の編集・委譲の指示文を書いている途中で
//! 出力上限に達すると、停止理由 `max_tokens` で応答が終わる。アダプタは読めない引数の呼び出しを渡さない
//! （BUG-225）ので、ループに届くのは呼び出しの始まり（`ToolUseStart`）だけになる。ループがこれを
//! 「完了ではない停止（Truncated）」として畳むと、ツールを実行しないまま run が終わる（画面ではツールカードが
//! 出たまま止まる）。始まりだけ届いた呼び出しは実行せずに「切れた・分けて呼び直せ」と返し、続きを生成させる。
//! stub の `truncatecall:` は、この形（Start の後にすぐ `Done(MaxTokens)`）を流す。
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
struct CountingTool(&'static str, Arc<AtomicUsize>);

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.0
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
        self.1.fetch_add(1, Ordering::SeqCst);
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
                // effort を落とさず stub へ届ける（思考を絞った書き直しを確かめる）。
                supports_effort: Some(true),
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
        input_preview: "truncatecall".to_string(),
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
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool("sheet_edit", Arc::clone(&calls)))];
    let mut sink = TestSink::default();

    let outcome = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(
            LlmRole::User,
            "truncatecall: 表を全部埋めて",
        )],
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

/// 1 応答に引数まで届いた呼び出しと切れた呼び出しが混ざっても、どちらも実行しない。届いた方には
/// 「引数は届いている・送り直して」、切れた方には「切れた・分けて」と、別々の結果を返す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_and_cut_calls_in_one_truncated_answer_get_their_own_results() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let calls = Arc::new(AtomicUsize::new(0));
    // tools[0] が切れ、tools[1] が引数まで届く（stub の `truncatecallmix:`）。
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(CountingTool("sheet_edit", Arc::clone(&calls))),
        Arc::new(CountingTool("sheet_read", Arc::clone(&calls))),
    ];
    let mut sink = TestSink::default();

    let outcome = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(
            LlmRole::User,
            "truncatecallmix: 表を読んで全部埋めて",
        )],
        &run_context(&c),
        &AgentOptions::autonomous(10, None, 1_000_000_000, 1_000_000_000),
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(outcome.stop, AgentStop::Completed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "上限で止まった応答の呼び出しは実行しない"
    );
    let result = |id: &str| {
        outcome
            .checkpoint
            .messages
            .iter()
            .filter(|m| m.role == LlmRole::Tool)
            .flat_map(|m| &m.content)
            .find_map(|b| match b {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error: true,
                    ..
                } if tool_use_id == id => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{id} の結果が無い: {:?}", outcome.checkpoint.messages))
    };
    let whole = result("stubtool_whole");
    assert!(whole.contains("引数は届いている"), "{whole}");
    assert!(!whole.contains("切れた"), "{whole}");
    let cut = result("stubtool_cut");
    assert!(cut.contains("引数の途中で切れた"), "{cut}");
    assert!(cut.contains("分け"), "{cut}");
    // 切れた呼び出しも画面のツールカードとして出し、失敗の結果で閉じる（出たまま止まらない）。
    for id in ["stubtool_whole", "stubtool_cut"] {
        assert!(
            sink.events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCall { id: i, .. } if i == id)),
            "{id}: {:?}",
            sink.events
        );
        assert!(
            sink.events.iter().any(|e| matches!(e,
                AgentEvent::ToolResult { tool_call_id, ok: false, .. } if tool_call_id == id)),
            "{id}: {:?}",
            sink.events
        );
    }
}

/// 本文の途中で出力上限に達しても run を畳まず、同じ run のまま続きを書かせて完了する
/// （利用者に「続けて」と送り直させない）。続きは直前の本文に連結されて画面に出る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_text_is_continued_in_the_same_run() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool(
        "sheet_edit",
        Arc::new(AtomicUsize::new(0)),
    ))];
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
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool(
        "sheet_edit",
        Arc::new(AtomicUsize::new(0)),
    ))];
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
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool(
        "sheet_edit",
        Arc::new(AtomicUsize::new(0)),
    ))];
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

/// ツール結果を受けた応答が**思考だけで**出力上限に達しても run を畳まず、思考を絞って
/// 次の手を出させて完了する（実測: 集計の途中で思考だけで 8192 トークンを使い切り、
/// 「続けて」の案内で止まった）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thinking_only_response_is_retried_with_lower_effort() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let calls = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool("sheet_edit", Arc::clone(&calls)))];
    let mut sink = TestSink::default();
    let mut opts = AgentOptions::autonomous(10, None, 1_000_000_000, 1_000_000_000);
    opts.effort = Some(llm_gateway::Effort::High);

    let outcome = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "thinkonly: 欠損を数えて")],
        &run_context(&c),
        &opts,
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(outcome.stop, AgentStop::Completed, "{:?}", sink.events);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let text: String = sink
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("回答です（effort=low）"),
        "書き直しは effort を下げて生成する: {text}"
    );
    assert!(
        !text.contains(llm_gateway::THINKING_EXHAUSTED_MARK),
        "{text}"
    );
    // 思考だけの応答は履歴へ積まない（空の assistant を送り返さない）。
    assert!(
        outcome
            .checkpoint
            .messages
            .iter()
            .all(|m| m.role != LlmRole::Assistant || !m.content.is_empty()),
        "{:?}",
        outcome.checkpoint.messages
    );
    assert_eq!(
        outcome.checkpoint.spent.steps, 3,
        "呼び出し・思考だけ・書き直し"
    );
}

/// 書き直させても思考だけで止まり続けるモデルは、上限回数の後に Truncated で畳む。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thinking_only_retries_are_bounded() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CountingTool(
        "sheet_edit",
        Arc::new(AtomicUsize::new(0)),
    ))];
    let mut sink = TestSink::default();

    let outcome = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "thinkonly:forever")],
        &run_context(&c),
        &AgentOptions::autonomous(20, None, 1_000_000_000, 1_000_000_000),
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(outcome.stop, AgentStop::Truncated);
    // 呼び出し 1 + 思考だけ（初回＋書き直し 2 回）。催促を使い切った思考だけの応答には空応答の催促
    // （「本文として書いて」）を送らず、そのまま打ち切りで畳む。
    assert_eq!(
        outcome.checkpoint.spent.steps, 4,
        "{:?}",
        outcome.checkpoint.spent
    );
}
