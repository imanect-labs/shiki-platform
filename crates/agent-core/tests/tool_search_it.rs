//! tool search（遅延ツールの検索→読み込み→呼び出し）の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（stub プロバイダ・agent_it.rs と同型の
//! 最小ハーネス）。stub は `toolsearch:<query>` で 1 ターン目に `tool_search` を呼び、読み込まれた
//! 先頭のツールを 2 ターン目に呼ぶ。検証する不変条件:
//! - 遅延ツールは `defer_loading` で提示され、`tool_search` が末尾に足される。
//! - 検索結果の読み込み参照が履歴（チェックポイント）に残る＝読み込み状態の唯一の正。
//! - 読み込んだツールが通常どおり dispatch される（承認ゲート・イベントの外部化も同じ経路）。
//! - 遅延にする定義が小さい run では何も変わらない（`tool_search` を足さない）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use agent_core::{
    run_agent, AgentError, AgentEvent, AgentOptions, AgentStop, Checkpoint, EventSink, RunContext,
    Tool, ToolError, ToolOutcome, ToolSearchOptions,
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
    checkpoints: Vec<Checkpoint>,
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

/// 名前と説明だけを持ち、呼ばれたら `done:<name>` を返すツール。
struct NamedTool {
    name: &'static str,
    description: &'static str,
}

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
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
        Ok(ToolOutcome::ok(format!("done:{}", self.name)))
    }
}

fn tools() -> Vec<Arc<dyn Tool>> {
    let t = |name, description| -> Arc<dyn Tool> { Arc::new(NamedTool { name, description }) };
    vec![
        t("doc_search", "社内文書を検索する。"),
        t(
            "csv.query",
            "CSV ファイルに読み取り専用 SQL を実行して集計する。",
        ),
        t(
            "csv.patch",
            "CSV の行を編集して新しいバージョンを保存する。",
        ),
        t(
            "office.edit",
            "Office ファイル（docx/xlsx/pptx）を編集する。",
        ),
        t("slide.edit", "スライドを共同編集で書き換える。"),
    ]
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

fn ctx() -> AuthContext {
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        "acme".into(),
        tenant,
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

async fn run(
    gateway: &LlmGateway,
    utterance: &str,
    opts: &AgentOptions,
) -> (AgentStop, RecordingSink) {
    let c = ctx();
    let mut sink = RecordingSink {
        events: Vec::new(),
        checkpoints: Vec::new(),
    };
    let run_ctx = RunContext {
        ctx: &c,
        idempotency_prefix: format!("run-{}:0", uuid::Uuid::new_v4()),
        trace_id: None,
        input_preview: utterance.to_string(),
        app_id: None,
    };
    let outcome = run_agent(
        gateway,
        &tools(),
        vec![LlmMessage::text(LlmRole::User, utterance)],
        &run_ctx,
        opts,
        None,
        None,
        &mut sink,
    )
    .await
    .expect("run_agent 成功");
    (outcome.stop, sink)
}

fn called(sink: &RecordingSink) -> Vec<String> {
    sink.events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// 検索 → 読み込み → 呼び出しが 1 run の中で繋がり、読み込みが履歴に残る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_loads_deferred_tool_then_dispatches_it() {
    let Some(pool) = setup().await else { return };
    let gateway = stub_gateway(pool);
    let mut opts = AgentOptions::chat(4);
    opts.tool_search = ToolSearchOptions {
        min_deferred_tokens: 0,
        ..ToolSearchOptions::default()
    };

    let (stop, sink) = run(&gateway, "toolsearch:CSV を集計したい", &opts).await;

    assert_eq!(stop, AgentStop::Completed);
    assert_eq!(called(&sink), ["tool_search", "csv.query"]);
    let results: Vec<(bool, &str)> = sink
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolResult { ok, content, .. } => Some((*ok, content.as_str())),
            _ => None,
        })
        .collect();
    assert!(
        results[0].0 && results[0].1.contains("- csv.query:"),
        "{results:?}"
    );
    assert_eq!(results[1], (true, "done:csv.query"));

    // 読み込み参照は検索結果のブロックに残る（次ステップ以降の提示はここから導かれる）。
    let checkpoint = sink.checkpoints.last().expect("継続ステップで保存される");
    let refs: Vec<&String> = checkpoint
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            Block::ToolResult {
                tool_references, ..
            } => Some(tool_references),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(refs.first().map(|s| s.as_str()), Some("csv.query"));
}

/// 遅延にする定義が下限未満なら tool search は入らない（従来どおり全提示・stub は直接呼ぶ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_catalog_keeps_every_tool_loaded() {
    let Some(pool) = setup().await else { return };
    let gateway = stub_gateway(pool);
    // 既定の下限（推定 2000 トークン）はこの小さな構成を素通しする。
    let (stop, sink) = run(
        &gateway,
        "toolsearch:CSV を集計したい",
        &AgentOptions::chat(4),
    )
    .await;

    assert_eq!(stop, AgentStop::Completed);
    assert!(
        called(&sink).is_empty(),
        "tool_search は提示されないので呼ばれない"
    );
}
