//! run のキャンセル・再開の結合テスト（Task 10.14・engine.md §9.3/§11.4・実 Postgres）。
//!
//! - cancel: 待機中 step を含む run が cancelled 化・購読/タイマーが再発火しない
//! - resume: 失敗 step から再開し成功済み checkpoint を再実行しない（実行回数で検証）

#![allow(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{
    CancelOutcome, NodeContext, NodeExecutor, NodeResult, ResumeOutcome, RunListFilter, RunStore,
    StepStatus, WorkerConfig, WorkflowWorker,
};

/// ノードごとの実行回数を数え、`fail_first` ノードは 1 回目だけ失敗する executor。
struct FlakyExecutor {
    counts: Arc<AtomicUsize>,
    a_counts: Arc<AtomicUsize>,
}

#[async_trait]
impl NodeExecutor for FlakyExecutor {
    async fn execute(&self, node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        if node_type == "control.wait" {
            // wait は本物の挙動（typed params 経由）に任せたいが、この IT では suspend 指示のみ再現。
            return NodeResult::wait(workflow_engine::run::Suspend::Timer {
                wake_at: chrono::Utc::now() + chrono::Duration::seconds(3600),
            });
        }
        if ctx.step_path == "a" {
            self.a_counts.fetch_add(1, Ordering::SeqCst);
        }
        if params.get("fail_first").is_some() {
            let n = self.counts.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                return NodeResult::fail("boom", "初回のみ失敗", false);
            }
        }
        NodeResult::ok(json!({ "step": ctx.step_path }))
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
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

async fn create_run(store: &RunStore, tenant: &str, wf: Uuid, ir: &Value) -> Uuid {
    let parsed = workflow_engine::WorkflowIr::from_json(ir).unwrap();
    let graph = RunGraph::build(&parsed);
    store
        .create_run(
            tenant,
            "acme",
            wf,
            1,
            "interactive",
            None,
            "alice",
            "user",
            &json!({}),
            ir,
            &graph,
        )
        .await
        .expect("create_run")
        .expect("admitted")
}

fn worker(
    pool: PgPool,
    tenant: &str,
    counts: Arc<AtomicUsize>,
    a: Arc<AtomicUsize>,
) -> WorkflowWorker {
    WorkflowWorker::new(
        RunStore::new(pool),
        Arc::new(FlakyExecutor {
            counts,
            a_counts: a,
        }),
        WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant)
}

#[tokio::test]
async fn cancel_drains_waiting_run_and_timer_never_revives() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = RunStore::new(pool.clone());
    let ir = json!({
        "ir_version": 1, "name": "cancelme",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "w", "type": "control.wait", "params": { "kind": "duration", "duration_sec": 3600 } },
            { "id": "after", "type": "storage.read", "params": {} }
        ],
        "edges": [{ "from": "w", "to": "after" }]
    });
    let run_id = create_run(&store, &tenant, wf, &ir).await;
    let w = worker(
        pool.clone(),
        &tenant,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    while w.claim_and_run_once("w1").await.unwrap() {}

    // waiting_timer で停止中 → cancel 要求で即 terminal 化（running なし）。
    let outcome = store.request_cancel(&tenant, wf, run_id).await.unwrap();
    assert_eq!(outcome, CancelOutcome::Requested);
    let d = store
        .run_detail(&tenant, wf, run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.status, "cancelled");
    assert!(d
        .steps
        .iter()
        .all(|s| s.status == "cancelled" || s.status == "succeeded"));

    // タイマー起床が来ても復活しない（wake_at は NULL 化済み）。
    let woke = store
        .wake_due_timers(
            chrono::Utc::now() + chrono::Duration::seconds(7200),
            Some(&tenant),
        )
        .await
        .unwrap();
    assert_eq!(woke, 0, "cancelled step はタイマーで起床しない");

    // 二重キャンセルは already_terminal。
    assert!(matches!(
        store.request_cancel(&tenant, wf, run_id).await.unwrap(),
        CancelOutcome::AlreadyTerminal(_)
    ));
    // 別 workflow_id では存在秘匿（NotFound）。
    assert_eq!(
        store
            .request_cancel(&tenant, Uuid::new_v4(), run_id)
            .await
            .unwrap(),
        CancelOutcome::NotFound
    );
}

#[tokio::test]
async fn resume_restarts_failed_step_without_reexecuting_checkpoints() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = RunStore::new(pool.clone());
    let ir = json!({
        "ir_version": 1, "name": "resumeme",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "a", "type": "storage.read", "params": {} },
            { "id": "b", "type": "storage.read", "params": { "fail_first": true } },
            { "id": "c", "type": "storage.read", "params": {} }
        ],
        "edges": [{ "from": "a", "to": "b" }, { "from": "b", "to": "c" }]
    });
    let run_id = create_run(&store, &tenant, wf, &ir).await;
    let fail_counter = Arc::new(AtomicUsize::new(0));
    let a_counter = Arc::new(AtomicUsize::new(0));
    let w = worker(pool.clone(), &tenant, fail_counter, Arc::clone(&a_counter));
    while w.claim_and_run_once("w1").await.unwrap() {}

    // b の初回失敗で run failed・c は cancelled（失敗ドレイン）。
    let d = store
        .run_detail(&tenant, wf, run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.status, "failed");
    assert_eq!(a_counter.load(Ordering::SeqCst), 1);

    // failed 以外は resume 不可（succeeded run で NotFailed を検証する代わりに二重 resume で確認）。
    let outcome = store.resume_failed(&tenant, wf, run_id).await.unwrap();
    assert_eq!(outcome, ResumeOutcome::Resumed);
    while w.claim_and_run_once("w1").await.unwrap() {}

    let d = store
        .run_detail(&tenant, wf, run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.status, "succeeded", "再開後に完走する: {:?}", d.steps);
    assert_eq!(
        a_counter.load(Ordering::SeqCst),
        1,
        "成功済み checkpoint（a）は再実行されない"
    );
    let step_c = d.steps.iter().find(|s| s.step_path == "c").unwrap();
    assert_eq!(step_c.status, "succeeded", "cancelled だった下流も完走");
    // run.resumed がイベント列に乗る。
    let events = store
        .list_events(&tenant, wf, run_id, 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|e| e.kind == "run.resumed"));
    // 完走済み run の resume は NotFailed。
    assert!(matches!(
        store.resume_failed(&tenant, wf, run_id).await.unwrap(),
        ResumeOutcome::NotFailed(_)
    ));

    // 一覧フィルタの回帰: succeeded 1 件。
    let all = store
        .list_runs(&tenant, wf, &RunListFilter::default(), None, 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, "succeeded");
    let _ = StepStatus::Succeeded;
}
