//! 作成系の道具を初めて呼んだときの予算の 1 回だけの拡張（D-53 の HD-4 (c)・D-204）の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（stub プロバイダ・resume_it.rs と同型の最小ハーネス）。
//! stub の `loop:` は毎ターン tools[0] を空入力で呼び続けるので、tools[0] を作成系の道具の名前にして回す。
//! 検証する不変条件:
//! - 作成系の道具の初回の呼び出しの後は、広げた手数と 1 応答の上限で回る（何度呼んでも広がるのは 1 回ぶん）。
//!   失敗した呼び出しも「呼んだ」に数える。
//! - チェックポイントから再開しても広げた予算のまま（チェックポイントに欄は無く、履歴から導く・古い形でも同じ）。
//! - `budget_extension` が `None` の経路（サブエージェント・ワークフローの agent.invoke・ミニアプリの AI）は
//!   作成系の道具を呼んでも広がらない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use agent_core::{
    budget::BudgetKind, run_agent, AgentError, AgentEvent, AgentOptions, AgentStop,
    BudgetExtension, Checkpoint, EventSink, RunContext, Tool, ToolError, ToolName, ToolOutcome,
};
use async_trait::async_trait;
use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, LlmGateway, Message as LlmMessage, ModelCatalog, ModelEntry, ProviderConfig,
    ProviderKind, Role as LlmRole,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

/// イベントとチェックポイント保存を記録する sink。
#[derive(Default)]
struct RecordingSink {
    events: Vec<AgentEvent>,
    checkpoints: Vec<Checkpoint>,
}

impl RecordingSink {
    /// 各生成の要求に載せた 1 応答の出力上限（ContextUsage の `reserved_output_tokens`）。
    fn response_caps(&self) -> Vec<Option<u32>> {
        self.events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ContextUsage(u) => Some(u.reserved_output_tokens),
                _ => None,
            })
            .collect()
    }

    fn tool_calls(&self) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
            .count()
    }
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

    async fn save_checkpoint(&mut self, checkpoint: &Checkpoint) -> Result<(), AgentError> {
        self.checkpoints.push(checkpoint.clone());
        Ok(())
    }
}

/// 名前だけ作成系の道具（または別の道具）に似せたテストの道具。`fails` なら毎回失敗を返す。
struct NamedTool {
    name: &'static str,
    fails: bool,
}

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "テストの道具。"
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
        Ok(if self.fails {
            ToolOutcome::error("ir.unknown_node: 検証に通りませんでした")
        } else {
            ToolOutcome::ok("保存しました")
        })
    }
}

fn tools(name: &'static str, fails: bool) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(NamedTool { name, fails })]
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
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

fn run_context(ctx: &AuthContext) -> RunContext<'_> {
    RunContext {
        ctx,
        idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
        trace_id: None,
        input_preview: "loop".into(),
        app_id: None,
        owns_trace: true,
    }
}

fn user_msg() -> Vec<LlmMessage> {
    vec![LlmMessage::text(
        LlmRole::User,
        "loop: 経費のワークフローを作って",
    )]
}

/// 通常の会話の既定に近い予算（2 手・1 応答 2048）に、5 手・1 応答 4096 への拡張を付ける。
fn chat_opts(extension: bool) -> AgentOptions {
    let mut opts = AgentOptions::chat(2);
    opts.max_tokens = Some(2048);
    opts.budget.max_tokens = Some(1_000_000);
    opts.budget.max_cost_usd_micros = Some(1_000_000);
    opts.tool_search.enabled = false;
    if extension {
        opts.budget_extension = Some(BudgetExtension {
            on_tools: vec![ToolName::EmitWorkflow],
            max_steps: 5,
            max_fresh_tokens: 2_000_000,
            max_cost_usd_micros: 2_000_000,
            max_response_tokens: 4096,
        });
    }
    opts
}

/// 初回の呼び出しの後は広げた手数と 1 応答の上限で回る。何度呼んでも広がるのは 1 回ぶん（5 手で止まる）。
/// 検証で断られた（失敗した）呼び出しも「呼んだ」に数える。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_builder_call_extends_the_budget_once() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    for fails in [false, true] {
        let mut sink = RecordingSink::default();
        let outcome = run_agent(
            &gateway,
            &tools("emit_workflow", fails),
            user_msg(),
            &run_context(&c),
            &chat_opts(true),
            None,
            None,
            &mut sink,
        )
        .await
        .expect("run");
        assert_eq!(outcome.stop, AgentStop::Budget(BudgetKind::Steps));
        assert_eq!(outcome.checkpoint.spent.steps, 5, "広げた手数で止まる");
        assert_eq!(sink.tool_calls(), 5);
        // 1 手目は元の上限、2 手目から広げた上限（最後の 1 つはツールを外した着地の生成）。
        let caps = sink.response_caps();
        assert_eq!(caps.len(), 6, "{caps:?}");
        assert_eq!(caps[0], Some(2048), "{caps:?}");
        assert!(caps[1..].iter().all(|c| *c == Some(4096)), "{caps:?}");
    }
}

/// チェックポイントから再開しても広げた予算のまま。チェックポイントに広げたことの欄は無く、履歴から導く
/// （#351 以前の古い形のチェックポイントでも同じ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_runs_keep_the_extended_budget() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let mut first = RecordingSink::default();
    run_agent(
        &gateway,
        &tools("emit_workflow", false),
        user_msg(),
        &run_context(&c),
        &chat_opts(true),
        None,
        None,
        &mut first,
    )
    .await
    .expect("run");
    // 元の上限（2 手）に達した境界のチェックポイント。ここから元の予算で再開すると即座に止まる。
    let at_base_cap = first
        .checkpoints
        .iter()
        .find(|cp| cp.spent.steps == 2)
        .expect("2 手目の境界で保存している")
        .clone();
    let json = serde_json::to_value(&at_base_cap).unwrap();
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "citations",
            "context_overhead_tokens",
            "loop_detector",
            "messages",
            "plan",
            "spent",
            "step"
        ],
        "チェックポイントの形は変えない"
    );
    // 古い形（後から足した欄が無い）でも、履歴の呼び出しから広げる。
    let mut old = json.clone();
    for key in ["loop_detector", "citations", "context_overhead_tokens"] {
        old.as_object_mut().unwrap().remove(key);
    }
    for resume in [json, old] {
        let checkpoint: Checkpoint = serde_json::from_value(resume).unwrap();
        let mut sink = RecordingSink::default();
        let outcome = run_agent(
            &gateway,
            &tools("emit_workflow", false),
            Vec::new(),
            &run_context(&c),
            &chat_opts(true),
            Some(checkpoint),
            None,
            &mut sink,
        )
        .await
        .expect("resume");
        assert_eq!(outcome.stop, AgentStop::Budget(BudgetKind::Steps));
        assert_eq!(
            outcome.checkpoint.spent.steps, 5,
            "再開しても元の 2 手に戻らない"
        );
        assert_eq!(sink.tool_calls(), 3, "残りの 3 手だけ回す");
        let caps = sink.response_caps();
        assert!(
            caps.iter().all(|c| *c == Some(4096)),
            "再開の直後から広げた上限: {caps:?}"
        );
    }
    // 拡張の無い設定で同じチェックポイントから再開すると、元の上限ですぐ止まる（対照）。
    let mut sink = RecordingSink::default();
    let outcome = run_agent(
        &gateway,
        &tools("emit_workflow", false),
        Vec::new(),
        &run_context(&c),
        &chat_opts(false),
        Some(at_base_cap),
        None,
        &mut sink,
    )
    .await
    .expect("resume");
    assert_eq!(outcome.stop, AgentStop::Budget(BudgetKind::Steps));
    assert_eq!(outcome.checkpoint.spent.steps, 2);
    assert_eq!(sink.tool_calls(), 0);
}

/// `budget_extension` が `None` の経路は、作成系の道具を呼んでも広がらない（サブエージェント・ワークフローの
/// agent.invoke は `AgentOptions::autonomous`、ミニアプリの AI は `AgentOptions::chat` から組む）。拡張があっても、
/// 作成系でない道具では広がらない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_without_the_extension_keep_their_budget() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let mut autonomous = AgentOptions::autonomous(2, None, 1_000_000, 1_000_000);
    autonomous.offer_plan_tool = false;
    autonomous.tool_search.enabled = false;
    let cases: [(&str, AgentOptions, &'static str); 3] = [
        ("chat の既定", chat_opts(false), "emit_workflow"),
        ("autonomous の既定", autonomous, "emit_workflow"),
        ("作成系でない道具", chat_opts(true), "doc_search"),
    ];
    for (label, opts, tool) in cases {
        assert!(
            label == "作成系でない道具" || opts.budget_extension.is_none(),
            "{label}"
        );
        let mut sink = RecordingSink::default();
        let outcome = run_agent(
            &gateway,
            &tools(tool, false),
            user_msg(),
            &run_context(&c),
            &opts,
            None,
            None,
            &mut sink,
        )
        .await
        .expect("run");
        assert_eq!(
            outcome.stop,
            AgentStop::Budget(BudgetKind::Steps),
            "{label}"
        );
        assert_eq!(outcome.checkpoint.spent.steps, 2, "{label}");
        let caps = sink.response_caps();
        assert!(
            caps.iter().all(|c| *c == opts.max_tokens),
            "{label}: {caps:?}"
        );
    }
}

/// 履歴の道具の結果に添えた残量の知らせ（`[予算]`・#1176）の本文を、順に取り出す。
fn budget_notes(messages: &[LlmMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.role == LlmRole::Tool)
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            llm_gateway::Block::ToolResult { content, .. } => {
                content.find("[予算]").map(|i| content[i..].to_string())
            }
            _ => None,
        })
        .collect()
}

/// 残量の知らせ（#1176）は、作成系の道具を呼んだその結果から広げた上限で数える。広げる前の上限で
/// 「残り 1 手・畳め」と知らせると、広げた手数を使わずに畳んでしまう（D-204 と #1176 のかみ合わせ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_notes_count_against_the_extended_budget() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let run = |extension: bool| {
        let gateway = &gateway;
        let c = &c;
        async move {
            let mut sink = RecordingSink::default();
            run_agent(
                gateway,
                &tools("emit_workflow", false),
                user_msg(),
                &run_context(c),
                &chat_opts(extension),
                None,
                None,
                &mut sink,
            )
            .await
            .expect("run")
        }
    };
    // 広げる run: 1 手目の結果から、広げた 5 手に対する残り（4 手）を知らせ、畳めとは言わない。
    let notes = budget_notes(&run(true).await.checkpoint.messages);
    assert!(notes.len() >= 2, "{notes:?}");
    assert!(notes[0].starts_with("[予算] 残り 4 ステップ"), "{notes:?}");
    assert!(!notes[0].contains("新しい調査を始めないこと"), "{notes:?}");
    // 上限に近づいたら、広げた上限に対して畳めと知らせる（残り 2 手から）。
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("[予算] 残り 2 ステップ")
                && n.contains("新しい調査を始めないこと")),
        "{notes:?}"
    );
    // 対照: 広げない run は、元の 2 手に対する残り（1 手）で畳めと知らせる。
    let notes = budget_notes(&run(false).await.checkpoint.messages);
    assert!(notes[0].starts_with("[予算] 残り 1 ステップ"), "{notes:?}");
    assert!(notes[0].contains("新しい調査を始めないこと"), "{notes:?}");
}
