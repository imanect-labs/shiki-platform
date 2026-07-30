//! サブエージェント委譲（`subagent`・#391）の結合テスト。
//!
//! 実 Postgres が必要（`LlmGateway` がトークン会計を DB へ書くため）。未設定ならスキップする。
//! LLM は stub プロバイダで決定的に駆動する。
//!
//! ここで固定するのは **#391 の受け入れ条件**:
//!
//! - 子は親と**同一の実行主体**で走る（権限昇格が無い）
//! - 子の**生の取得本文が親へ流れない**（合成済み findings のみ）
//! - 子の消費が親へ積める形（[`ToolUsage`]）で返る（親の予算で止められる根拠）
//! - 累計体数の上限が**モデルの観測できる失敗**になる（run は落とさない）
//! - 破壊系ツールを渡していないこと（allowlist の設計ミスは承認ゲートで Reject される）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use agent_core::{SubagentLimits, SubagentTool, Tool, ToolError, ToolName, ToolOutcome};
use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

async fn pool() -> Option<PgPool> {
    let url = std::env::var("STORAGE_TEST_DATABASE_URL")
        .ok()
        .or_else(|| {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            None
        })?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
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

fn ctx() -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("t1".into()),
        },
        "acme".into(),
        "t1".into(),
    )
}

/// 子が使ったツールと実行主体を記録するフェイク（`web_search` を名乗る）。
#[derive(Default)]
struct RecordingTool {
    principals: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        ToolName::WebSearch.as_str()
    }
    fn description(&self) -> &str {
        "検索"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(
        &self,
        c: &AuthContext,
        _input: serde_json::Value,
        _t: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        self.principals.lock().unwrap().push(c.principal.id.clone());
        Ok(ToolOutcome::ok("RAW_PAGE_BODY_親へ漏れてはいけない本文"))
    }
}

fn subagent(pool: &PgPool, child: Vec<Arc<dyn Tool>>, limits: SubagentLimits) -> SubagentTool {
    SubagentTool::new(
        stub_gateway(pool.clone()),
        child,
        limits,
        "run:1".to_string(),
        Arc::new(AtomicBool::new(false)),
    )
    .with_model(Some("m".to_string()))
}

/// stub は `websearch:` 接頭辞で `web_search` を 1 回呼び、次のターンで本文を返す。
fn input(objective: &str) -> serde_json::Value {
    serde_json::json!({ "objective": objective, "boundary": "2026 年の国内のみ" })
}

#[tokio::test]
async fn delegates_without_privilege_escalation_and_returns_only_findings() {
    let Some(pool) = pool().await else { return };
    let rec = Arc::new(RecordingTool::default());
    let tool = subagent(
        &pool,
        vec![rec.clone() as Arc<dyn Tool>],
        SubagentLimits::default(),
    );

    let out = tool
        .call(&ctx(), input("websearch: 市場規模"), None)
        .await
        .expect("委譲は成功する");

    // ① 権限昇格が無い（子は親と同一の実行主体）。
    let principals = rec.principals.lock().unwrap().clone();
    assert!(!principals.is_empty(), "子がツールを使う");
    assert!(
        principals.iter().all(|p| p == "alice"),
        "子は親と同一の実行主体で走る: {principals:?}"
    );

    // ② 生の取得本文が親へ流れない（返るのは合成済み findings だけ）。
    assert!(
        !out.content.contains("RAW_PAGE_BODY"),
        "生の観測が親のコンテキストへ漏れている: {}",
        out.content
    );
    assert!(!out.is_error, "findings が返る: {}", out.content);

    // ③ 監査記録（objective / boundary / ステップ / ツール名）が付く。
    assert_eq!(out.subagent_runs.len(), 1);
    let record = &out.subagent_runs[0];
    assert_eq!(record["boundary"], "2026 年の国内のみ");
    assert!(record["steps"].as_u64().unwrap() >= 1);
    assert!(
        record["tool_calls"]
            .as_array()
            .unwrap()
            .contains(&serde_json::Value::String("web_search".to_string())),
        "使ったツール名が残る（再現性）"
    );

    // ④ 子の消費が親へ積める形で返る（親の Budget が子込みで止まる根拠）。
    let usage = out.usage.expect("usage を返す");
    assert!(usage.tokens > 0, "トークン消費が親へ積まれる");
}

#[tokio::test]
async fn caps_total_subagents_per_run_as_observable_error() {
    let Some(pool) = pool().await else { return };
    let limits = SubagentLimits {
        max_per_run: 1,
        ..SubagentLimits::default()
    };
    let tool = subagent(
        &pool,
        vec![Arc::new(RecordingTool::default()) as Arc<dyn Tool>],
        limits,
    );

    let first = tool
        .call(&ctx(), input("websearch: 一体目"), None)
        .await
        .unwrap();
    assert!(!first.is_error);

    let second = tool
        .call(&ctx(), input("websearch: 二体目"), None)
        .await
        .unwrap();
    assert!(
        second.is_error,
        "上限超過は is_error で観測させる（run は落とさない）"
    );
    assert!(second.content.contains("上限"), "{}", second.content);
    assert!(second.usage.is_none(), "走っていないので消費も無い");
    assert!(second.subagent_runs.is_empty());
}

/// 破壊系ツールを子に渡してしまった場合、承認者が居ないので**実行されない**（fail-closed）。
///
/// allowlist の設計ミスが「黙って破壊系が走る」ではなく「Reject の観測が返る」で終わることを固定する。
/// 子の事前許可は `web_search` / `web_fetch` の 2 名だけなので、それ以外は承認ゲートで止まる。
#[tokio::test]
async fn destructive_child_tool_is_rejected_not_executed() {
    let Some(pool) = pool().await else { return };

    #[derive(Default)]
    struct DestructiveTool {
        ran: Mutex<bool>,
    }
    #[async_trait::async_trait]
    impl Tool for DestructiveTool {
        fn name(&self) -> &str {
            // 実在の破壊系ツール名（allowlist へ誤って混ぜてしまった状況を再現する）。
            // stub は提示に web_search が無いと tools[0] へフォールバックするのでこれが呼ばれる。
            ToolName::FsWrite.as_str()
        }
        fn description(&self) -> &str {
            "危険"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn requires_confirmation(&self) -> bool {
            true
        }
        async fn call(
            &self,
            _c: &AuthContext,
            _i: serde_json::Value,
            _t: Option<&str>,
        ) -> Result<ToolOutcome, ToolError> {
            *self.ran.lock().unwrap() = true;
            Ok(ToolOutcome::ok("実行してしまった"))
        }
    }

    let danger = Arc::new(DestructiveTool::default());
    let tool = subagent(
        &pool,
        vec![danger.clone() as Arc<dyn Tool>],
        SubagentLimits::default(),
    );
    let _ = tool
        .call(&ctx(), input("websearch: 危険な操作"), None)
        .await
        .expect("run 自体は落ちない");
    assert!(
        !*danger.ran.lock().unwrap(),
        "確認が必要なツールは承認者不在で実行されない（fail-closed）"
    );
}
