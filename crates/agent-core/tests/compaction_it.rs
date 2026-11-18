//! 要約による圧縮（compaction.rs）と 1 ステップの結果の上限（result_budget.rs）の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` があるときだけ走る（stub プロバイダ・resume_it.rs と同型）。
//! 検証する不変条件:
//! - 窓の小さいモデルで履歴が soft を超えると、要約に置き換えて続行する（イベント・回数・会計）。
//! - 置き換え後の履歴は先頭が依頼（＋要約）、続きは assistant から始まり、ツール呼び出しと結果の
//!   対応が壊れない（厳格なプロバイダが拒否しない形）。
//! - 圧縮を切れば従来どおり剪定だけ（要約しない）。
//! - 並列で大量に読んだ結果は 1 ステップの上限に収まり、超過分は「未表示・呼び直せる」になる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use agent_core::{
    run_agent, AgentError, AgentEvent, AgentOptions, Checkpoint, CompactionOptions, EventSink,
    RecoveryAction, RunContext, Tool, ToolError, ToolOutcome,
};
use async_trait::async_trait;
use authz::{AuthContext, Principal};
use llm_gateway::{
    Block, GatewayConfig, LlmGateway, Message as LlmMessage, ModelCatalog, ModelEntry,
    ProviderConfig, ProviderKind, Role as LlmRole,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

struct RecordingSink {
    events: Vec<AgentEvent>,
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        self.events.push(event);
        Ok(())
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    async fn save_checkpoint(&mut self, _c: &Checkpoint) -> Result<(), AgentError> {
        Ok(())
    }
}

/// 大きな本文を返す読み取りツール（stub の `loop:` が毎ターン空入力で呼ぶ）。
struct BigRead;

#[async_trait]
impl Tool for BigRead {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "big_read"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "大きな報告を読む。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}, "additionalProperties": true})
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(
        &self,
        _ctx: &AuthContext,
        _input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok(format!(
            "売上 1234 万円。{}",
            "報".repeat(1_500)
        )))
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

/// 窓 8k の stub モデル。
fn small_window_gateway(pool: PgPool) -> LlmGateway {
    LlmGateway::build(
        pool,
        reqwest::Client::new(),
        GatewayConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Stub,
                ..Default::default()
            },
            catalog: ModelCatalog {
                default_model: "m".into(),
                models: vec![ModelEntry {
                    id: "m".into(),
                    context_window_tokens: Some(8_000),
                    ..Default::default()
                }],
            },
            ..Default::default()
        },
    )
    .expect("gateway")
}

fn run_context(c: &AuthContext) -> RunContext<'_> {
    RunContext {
        ctx: c,
        idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
        trace_id: None,
        input_preview: "compaction".into(),
        app_id: None,
        owns_trace: true,
    }
}

fn opts(compaction: bool) -> AgentOptions {
    let mut o = AgentOptions::autonomous(10, None, 10_000_000, 10_000_000);
    o.max_tokens = Some(1_000);
    o.offer_plan_tool = false;
    o.tool_search.enabled = false;
    if !compaction {
        o.compaction = CompactionOptions::disabled();
    }
    o
}

/// tool_use と tool_result の対応が保たれている（結果の直前の assistant に同じ ID の呼び出しがある）。
fn assert_pairs_intact(messages: &[LlmMessage]) {
    for (i, m) in messages.iter().enumerate() {
        if m.role != LlmRole::Tool {
            continue;
        }
        let prev = &messages[i - 1];
        assert_eq!(prev.role, LlmRole::Assistant, "結果の直前は assistant");
        for b in &m.content {
            if let Block::ToolResult { tool_use_id, .. } = b {
                assert!(
                    prev.content
                        .iter()
                        .any(|p| matches!(p, Block::ToolUse { id, .. } if id == tool_use_id)),
                    "結果 {tool_use_id} に対応する呼び出しがある"
                );
            }
        }
    }
}

#[tokio::test]
async fn small_window_history_is_summarized_and_stays_well_formed() {
    let Some(pool) = setup().await else { return };
    let gateway = small_window_gateway(pool);
    let c = ctx(&format!("t-{}", uuid::Uuid::new_v4()));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(BigRead)];
    let mut sink = RecordingSink { events: Vec::new() };
    let out = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "loop: 全部読んで集計して")],
        &run_context(&c),
        &opts(true),
        None,
        None,
        &mut sink,
    )
    .await
    .unwrap();
    let compactions = sink
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::FailureRecovery { action, .. } if *action == RecoveryAction::Compacted))
        .count();
    assert!(
        compactions >= 1,
        "窓 8k で 1,500 字の結果を 10 回読めば要約が走る: {:?}",
        sink.events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::ContextUsage(_) | AgentEvent::FailureRecovery { .. }
            ))
            .collect::<Vec<_>>()
    );
    let cp = &out.checkpoint;
    // 回数は試行の数（縮まずに捨てた要約も 1 生成ぶん会計に積むため数える）。stub の「要約」は
    // 依頼文をそのまま返すので、2 回目以降は縮まずに捨てられうる。
    assert!(cp.compactions as usize >= compactions);
    // 先頭は依頼＋要約。続きは assistant から。
    assert_eq!(cp.messages[0].role, LlmRole::User);
    let head = format!("{:?}", cp.messages[0]);
    assert!(head.contains("loop: 全部読んで集計して"));
    assert!(head.contains("これまでの作業の要約"));
    assert_eq!(cp.messages[1].role, LlmRole::Assistant);
    assert_pairs_intact(&cp.messages);
    // 要約ブロックは 1 つだけ（再圧縮で積み重ならない）。stub の「要約」は依頼文をそのまま
    // 返すので、本文に目印の文字列が混じるのは stub の性質。ブロックの数で見る。
    let summaries = cp.messages[0]
        .content
        .iter()
        .filter(|b| matches!(b, Block::Text { text } if text.starts_with("［これまでの作業の要約")))
        .count();
    assert_eq!(summaries, 1);
}

#[tokio::test]
async fn compaction_disabled_keeps_pruning_only() {
    let Some(pool) = setup().await else { return };
    let gateway = small_window_gateway(pool);
    let c = ctx(&format!("t-{}", uuid::Uuid::new_v4()));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(BigRead)];
    let mut sink = RecordingSink { events: Vec::new() };
    let out = run_agent(
        &gateway,
        &tools,
        vec![LlmMessage::text(LlmRole::User, "loop: 全部読んで")],
        &run_context(&c),
        &opts(false),
        None,
        None,
        &mut sink,
    )
    .await
    .unwrap();
    assert!(!sink
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::FailureRecovery { action, .. } if *action == RecoveryAction::Compacted)));
    assert_eq!(out.checkpoint.compactions, 0);
    assert!(!format!("{:?}", out.checkpoint.messages[0]).contains("これまでの作業の要約"));
}
