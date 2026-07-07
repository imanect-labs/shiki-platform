//! Stage A エンドツーエンド結合テスト（Task 10.6a/10.8/10.10・実 Postgres＋live OpenFGA）。
//!
//! IR 保存 → enable（委譲） → interactive 起動 → ワーカーが能力ノードを effect_journal 付きで実行 →
//! run 完走。schedule トリガでも同じワークフローが workflow プリンシパルで起動することを確認する。
//! run 履歴に平文（secret 値・レスポンス本文）が載らないことも検証する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::ArtifactStore;
use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::capability::{op_digest, EffectJournal, JournalDecision};
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{
    Catalog, DelegationStore, GrantRequest, NodeContext, NodeExecutor, NodeResult, RunStatus,
    RunStore, WorkerConfig, WorkflowRunLauncher, WorkflowStore, WorkflowWorker,
};

async fn setup() -> Option<(PgPool, Arc<OpenFgaClient>)> {
    let (Ok(db), Ok(fga)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let model: Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let cfg = OpenFgaConfig {
        base_url: fga,
        store_name: format!("shiki-e2e-{}", Uuid::new_v4()),
    };
    let client = Arc::new(
        OpenFgaClient::connect(reqwest::Client::new(), &cfg, &model)
            .await
            .expect("fga"),
    );
    Some((pool, client))
}

fn user_ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

/// http.request を模した executor（effect_journal に「高々 1 回」記録し、本文は載せない）。
struct JournaledExecutor {
    journal: EffectJournal,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl NodeExecutor for JournaledExecutor {
    async fn execute(&self, node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        // storage.write / http.request のような副作用は effect_journal で冪等化する。
        if node_type == "http.request" || node_type == "storage.write" {
            let digest = op_digest(node_type, params);
            match self
                .journal
                .check(&ctx.tenant_id, &ctx.idempotency_key, &digest)
                .await
            {
                Ok(JournalDecision::Proceed) => {
                    // 実副作用（ここでは呼び出し回数を数える）→ レダクト済み要約を記録。
                    self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let summary = json!({ "status": 200 }); // 本文・secret は載せない。
                    self.journal
                        .record(&ctx.tenant_id, &ctx.idempotency_key, &digest, &summary)
                        .await
                        .unwrap();
                    NodeResult::ok(summary)
                }
                Ok(JournalDecision::AlreadyDone(summary)) => NodeResult::ok(summary),
                // 別ワーカーが実行中: 副作用を送らずリトライ可能に（テストでは発生しない）。
                Ok(JournalDecision::InProgress) => {
                    NodeResult::fail("effect_in_progress", "in progress", true)
                }
                Ok(JournalDecision::DigestMismatch) => {
                    NodeResult::fail("effect_conflict", "digest mismatch", false)
                }
                Err(e) => NodeResult::fail("journal_error", e.to_string(), true),
            }
        } else {
            NodeResult::ok(json!({ "node": node_type }))
        }
    }
}

fn workflow_ir() -> Value {
    json!({
        "ir_version": 1,
        "name": "daily-report",
        "declared_scopes": ["storage.read", "http.egress"],
        "nodes": [
            { "id": "read", "type": "storage.read", "params": {} },
            { "id": "post", "type": "http.request", "params": { "url": "https://api.example.com/x" } }
        ],
        "edges": [{ "from": "read", "to": "post" }],
        "triggers": [
            { "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }
        ]
    })
}

#[tokio::test]
async fn interactive_and_scheduled_run_complete_with_exactly_once_effects() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");

    // 各ストアを組む。
    let artifacts = Arc::new(ArtifactStore::new(
        pool.clone(),
        fga.clone() as Arc<dyn AuthzClient>,
    ));
    let workflows = WorkflowStore::new(artifacts.clone());
    let delegation = DelegationStore::new(pool.clone(), fga.clone() as Arc<dyn AuthzClient>);
    let runs = RunStore::new(pool.clone());
    let journal = EffectJournal::new(pool.clone());

    // ① IR を保存（V1〜V7）。
    let (wf_id, _ir) = workflows
        .create(&alice, &workflow_ir(), &Catalog::default(), None)
        .await
        .expect("save ir");

    // ② enable（委譲）。alice は対象 folder の viewer を持ち、それを委譲する。
    let folder = alice.ns().folder("reports");
    fga.write_tuple(&alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();
    delegation
        .enable(
            &alice,
            wf_id,
            1,
            &["storage.read".into(), "http.egress".into()],
            &[GrantRequest {
                scope: "storage.read".into(),
                object: folder.clone(),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect("enable");

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let exec = Arc::new(JournaledExecutor {
        journal: journal.clone(),
        calls: Arc::clone(&calls),
    });

    // ③ interactive 起動（本人権限で）。
    let launcher = WorkflowRunLauncher::new(delegation.clone(), workflows.clone(), runs.clone());
    let run_id = launcher
        .start_interactive(&alice, wf_id, &json!({ "date": "2026-07-07" }))
        .await
        .expect("start")
        .expect("run id");

    // ④ ワーカーが全 step を実行して完走する。
    let worker = WorkflowWorker::new(runs.clone(), exec.clone(), WorkerConfig::default())
        .scoped_to_tenant(&tenant);
    while worker.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        runs.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded),
        "interactive run が完走する"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "副作用は 1 回"
    );

    // ⑤ schedule トリガでも同じワークフローが workflow プリンシパルで起動する。
    let sched_run = <WorkflowRunLauncher as workflow_engine::RunLauncher>::launch(
        &launcher, &tenant, wf_id, "schedule", "trg-1",
    )
    .await
    .expect("scheduled run id");
    let worker2 = WorkflowWorker::new(runs.clone(), exec.clone(), WorkerConfig::default())
        .scoped_to_tenant(&tenant);
    while worker2.claim_and_run_once("w2").await.unwrap() {}
    assert_eq!(
        runs.run_status(&tenant, sched_run).await.unwrap(),
        Some(RunStatus::Succeeded),
        "schedule run も完走する（workflow プリンシパル）"
    );

    // ⑥ run 履歴に平文が載らない（effect_journal の result_summary は status のみ）。
    let summaries: Vec<Value> =
        sqlx::query_scalar("SELECT result_summary FROM effect_journal WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(!summaries.is_empty());
    for s in &summaries {
        assert!(s.get("body").is_none(), "本文が載っていない");
        assert!(s.get("secret").is_none(), "secret 値が載っていない");
    }
}

#[tokio::test]
async fn suspended_workflow_scheduled_launch_creates_no_run() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");
    let artifacts = Arc::new(ArtifactStore::new(
        pool.clone(),
        fga.clone() as Arc<dyn AuthzClient>,
    ));
    let workflows = WorkflowStore::new(artifacts.clone());
    let delegation = DelegationStore::new(pool.clone(), fga.clone() as Arc<dyn AuthzClient>);
    let runs = RunStore::new(pool.clone());

    let (wf_id, _ir) = workflows
        .create(&alice, &workflow_ir(), &Catalog::default(), None)
        .await
        .expect("save");
    let folder = alice.ns().folder("reports");
    fga.write_tuple(&alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();
    delegation
        .enable(
            &alice,
            wf_id,
            1,
            &["storage.read".into(), "http.egress".into()],
            &[GrantRequest {
                scope: "storage.read".into(),
                object: folder.clone(),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect("enable");

    // alice が folder viewer を失う → 委譲失効。
    fga.delete_tuple(&alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();

    let launcher = WorkflowRunLauncher::new(delegation.clone(), workflows.clone(), runs.clone());
    // schedule 起動は委譲チェックで弾かれ run を作らない。
    let result = <WorkflowRunLauncher as workflow_engine::RunLauncher>::launch(
        &launcher, &tenant, wf_id, "schedule", "trg-1",
    )
    .await;
    assert!(
        result.is_none(),
        "委譲失効時は run を作らない（fail-closed）"
    );
}

/// RunGraph が e2e で参照されることを型チェックで担保（未使用 import 警告回避）。
#[allow(dead_code)]
fn _uses_graph(ir: &workflow_engine::WorkflowIr) -> RunGraph {
    RunGraph::build(ir)
}
