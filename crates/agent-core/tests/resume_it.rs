//! resume 配線（#351）と実行中ポリシ再評価（#350）の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（stub プロバイダ・agent_it.rs と同型の
//! 最小ハーネス）。検証する不変条件:
//! - ステップ境界ごとに `EventSink::save_checkpoint` が呼ばれ、消費（spent）が単調に進む。
//! - チェックポイントからの resume で予算消費が**継続積算**される（リセットされない）。
//! - `Approver::current_policy` の返す現在ポリシが承認ゲートで優先される（緩和/厳格化の両方向）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use agent_core::{
    approval::{ApprovalDecision, ApprovalPolicy, Approver},
    budget::BudgetKind,
    run_agent, AgentError, AgentEvent, AgentOptions, AgentStop, Checkpoint, EventSink, RunContext,
    Tool, ToolError, ToolOutcome,
};
use async_trait::async_trait;
use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, LlmGateway, Message as LlmMessage, ModelCatalog, ModelEntry, ProviderConfig,
    ProviderKind, Role as LlmRole,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

/// イベントとチェックポイント保存を記録する sink（save_checkpoint の呼び出し境界を検証）。
struct RecordingSink {
    events: Vec<AgentEvent>,
    checkpoints: Vec<Checkpoint>,
}

impl RecordingSink {
    fn new() -> Self {
        RecordingSink {
            events: Vec::new(),
            checkpoints: Vec::new(),
        }
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

/// 常に成功する安全ツール（stub の `loop:` 駆動で複数ステップ回す用）。
struct MockOkTool;

#[async_trait]
impl Tool for MockOkTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "noop"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "常に成功するテストツール。"
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
        Ok(ToolOutcome::ok("ok"))
    }
}

/// 要承認ツール（実行回数を数える）。
struct MockConfirmTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for MockConfirmTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "danger"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "承認が必要なテストツール。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": true })
    }
    fn requires_confirmation(&self) -> bool {
        true
    }
    async fn call(
        &self,
        _ctx: &AuthContext,
        _input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutcome::ok("実行しました"))
    }
}

/// 現在ポリシを差し替えられるフェイク承認者（実行中トグルの再現・#350）。
struct PolicyApprover {
    decision: ApprovalDecision,
    current: Option<ApprovalPolicy>,
}

#[async_trait]
impl Approver for PolicyApprover {
    async fn decide(
        &self,
        _tool_call_id: &str,
        _name: &str,
        _input: &serde_json::Value,
    ) -> ApprovalDecision {
        self.decision
    }

    async fn current_policy(&self) -> Option<ApprovalPolicy> {
        self.current.clone()
    }
}

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
        app_id: None,
    }
}

/// ステップ境界ごとに save_checkpoint が呼ばれ、spent が単調に進む（#351）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_saved_at_each_step_boundary() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockOkTool)];
    let mut sink = RecordingSink::new();

    let outcome = run_agent(
        &gateway,
        &tools,
        user_msg("loop: keep calling noop"),
        &run_context(&c, "loop"),
        &AgentOptions::autonomous(3, None, 1_000_000, 1_000_000),
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(outcome.stop, AgentStop::Budget(BudgetKind::Steps));
    assert_eq!(
        sink.checkpoints.len(),
        3,
        "ステップ境界ごとに 1 回ずつ保存される（max_steps=3）"
    );
    let steps: Vec<usize> = sink.checkpoints.iter().map(|cp| cp.spent.steps).collect();
    assert_eq!(steps, vec![1, 2, 3], "spent.steps が境界ごとに単調に進む");
    // 最終チェックポイント＝戻り値の checkpoint（同一のステップ境界状態）。
    assert_eq!(
        sink.checkpoints.last().unwrap().spent.steps,
        outcome.checkpoint.spent.steps
    );
}

/// チェックポイントからの resume で予算消費が継続積算される（リセットされない・#351）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_continues_budget_from_checkpoint() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockOkTool)];

    // 1 回目: max_steps=1 で 1 ステップだけ進めて checkpoint を得る（中断相当）。
    let mut sink1 = RecordingSink::new();
    let first = run_agent(
        &gateway,
        &tools,
        user_msg("loop: keep calling noop"),
        &run_context(&c, "loop"),
        &AgentOptions::autonomous(1, None, 1_000_000, 1_000_000),
        None,
        None,
        &mut sink1,
    )
    .await
    .expect("1 回目成功");
    assert_eq!(first.checkpoint.spent.steps, 1);
    let saved = serde_json::to_string(&first.checkpoint).expect("直列化可能");

    // 2 回目: durable 経由を模して JSON から復元し、max_steps=2 の予算で再開する。
    // 既に 1 ステップ消費済みなので、あと 1 ステップで予算停止する（継続積算の証明）。
    let restored: Checkpoint = serde_json::from_str(&saved).expect("復元可能");
    let mut sink2 = RecordingSink::new();
    let second = run_agent(
        &gateway,
        &tools,
        Vec::new(), // resume 時は無視される（checkpoint の履歴が起点）
        &run_context(&c, "loop"),
        &AgentOptions::autonomous(2, None, 1_000_000, 1_000_000),
        Some(restored),
        None,
        &mut sink2,
    )
    .await
    .expect("2 回目成功");

    assert_eq!(second.stop, AgentStop::Budget(BudgetKind::Steps));
    assert_eq!(
        second.checkpoint.spent.steps, 2,
        "resume 後は 1（復元）+1（追加実行）= 2 で停止する"
    );
    assert_eq!(
        sink2.checkpoints.len(),
        1,
        "再開後は追加 1 ステップ分のみ保存"
    );
}

/// current_policy の緩和が承認ゲートで優先される（承認なしで実行・イベントも出ない・#350）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_policy_relaxation_pre_authorizes() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let calls = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockConfirmTool {
        calls: calls.clone(),
    })];
    // スナップショット（opts.approval）は deny_all のまま、現在ポリシが danger を事前許可する。
    let approver = PolicyApprover {
        decision: ApprovalDecision::Rejected, // 呼ばれたら却下（呼ばれないことの証明）
        current: Some(ApprovalPolicy::auto(["danger".to_string()])),
    };
    let mut sink = RecordingSink::new();

    run_agent(
        &gateway,
        &tools,
        user_msg("loop: do danger"),
        &run_context(&c, "loop"),
        &AgentOptions::autonomous(1, None, 1_000_000, 1_000_000),
        None,
        Some(&approver),
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "現在ポリシの緩和で承認なしに実行"
    );
    assert_eq!(
        sink.events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ApprovalRequested { .. }))
            .count(),
        0,
        "承認要求は出ない（カードのちらつきも無い）"
    );
}

/// current_policy の厳格化がスナップショットの事前許可を上書きする（承認が要る・#350）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_policy_tightening_overrides_snapshot() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let gateway = stub_gateway(pool);
    let c = ctx(&tenant);
    let calls = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockConfirmTool {
        calls: calls.clone(),
    })];
    // スナップショットは danger を事前許可しているが、現在ポリシは deny_all（実行中の厳格化）。
    let approver = PolicyApprover {
        decision: ApprovalDecision::Rejected,
        current: Some(ApprovalPolicy::deny_all()),
    };
    let mut opts = AgentOptions::autonomous(1, None, 1_000_000, 1_000_000);
    opts.approval = ApprovalPolicy::auto(["danger".to_string()]);
    let mut sink = RecordingSink::new();

    run_agent(
        &gateway,
        &tools,
        user_msg("loop: do danger"),
        &run_context(&c, "loop"),
        &opts,
        None,
        Some(&approver),
        &mut sink,
    )
    .await
    .expect("run_agent 成功");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "厳格化された現在ポリシが優先され、却下で実行されない"
    );
    assert_eq!(
        sink.events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ApprovalRequested { .. }))
            .count(),
        1,
        "承認要求が流れる"
    );
}
