//! 実行中の step を止める合図（CancelSignal）の結合テスト（実 Postgres・BUG-6 の R3・engine.md §9.3）。
//!
//! ワーカーの heartbeat が「取り消し要求あり」か「リースを失った」（fencing 不一致）を返したら、実行中の
//! ノードの `ctx.cancel` が立つ。立たないと、長く走る agent.invoke が取り消し後も、別のワーカーが同じ step を
//! 走らせている間も、費用を使い続ける。`STORAGE_TEST_DATABASE_URL` が無ければスキップ。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{
    NodeContext, NodeExecutor, NodeResult, RunStore, WorkerConfig, WorkflowWorker,
};

/// 止める合図が立つまで（最大 20 秒）待ち、見えたかを記録する executor。
struct WaitForCancel {
    seen: Arc<AtomicBool>,
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl NodeExecutor for WaitForCancel {
    async fn execute(&self, _t: &str, _p: &Value, ctx: &NodeContext) -> NodeResult {
        self.started.notify_one();
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            if ctx.cancel.is_cancelled() {
                self.seen.store(true, Ordering::SeqCst);
                return NodeResult::fail("cancelled", "止めました", false);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        NodeResult::ok(json!({}))
    }
}

async fn setup() -> Option<PgPool> {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

async fn one_step_run(store: &RunStore, tenant: &str) -> uuid::Uuid {
    let ir = json!({
        "ir_version": 1, "name": "long", "declared_scopes": [],
        "nodes": [{ "id": "a", "type": "agent.invoke", "params": { "instruction": "x" } }],
        "edges": []
    });
    let parsed = workflow_engine::WorkflowIr::from_json(&ir).unwrap();
    store
        .create_run(
            tenant,
            "acme",
            uuid::Uuid::new_v4(),
            1,
            "interactive",
            None,
            "alice",
            "user",
            &json!({}),
            &ir,
            &RunGraph::build(&parsed),
        )
        .await
        .expect("create_run")
        .expect("admitted")
}

/// 実行中の step を持つワーカーを回し、`interfere` で横から run・step を書き換えたあと、合図が見えたかを返す。
async fn run_and_interfere(pool: &PgPool, interfere: &str) -> bool {
    // リース 3 秒（heartbeat は 1 秒ごと）。
    let store = RunStore::new(pool.clone(), 3);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = one_step_run(&store, &tenant).await;
    let seen = Arc::new(AtomicBool::new(false));
    let started = Arc::new(tokio::sync::Notify::new());
    let worker = WorkflowWorker::new(
        store,
        Arc::new(WaitForCancel {
            seen: Arc::clone(&seen),
            started: Arc::clone(&started),
        }),
        WorkerConfig {
            lease_secs: 3,
            ..WorkerConfig::default()
        },
    )
    .scoped_to_tenant(&tenant);
    let task = tokio::spawn(async move { worker.claim_and_run_once("w1").await });
    started.notified().await;
    sqlx::query(interfere)
        .bind(&tenant)
        .bind(run_id)
        .execute(pool)
        .await
        .expect("interfere");
    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("ワーカーが戻る")
        .expect("join")
        .expect("claim_and_run_once");
    seen.load(Ordering::SeqCst)
}

/// run の取り消し要求を heartbeat が拾うと、実行中のノードの合図が立つ。
#[tokio::test]
async fn cancel_request_raises_the_signal() {
    let Some(pool) = setup().await else { return };
    let seen = run_and_interfere(
        &pool,
        "UPDATE workflow_run SET cancel_requested = true WHERE tenant_id = $1 AND run_id = $2",
    )
    .await;
    assert!(seen, "取り消し要求で合図が立つ");
}

/// 別のワーカーにリースを奪われる（fencing 不一致）と、旧ワーカーの実行中のノードの合図が立つ。
#[tokio::test]
async fn lost_lease_raises_the_signal() {
    let Some(pool) = setup().await else { return };
    let seen = run_and_interfere(
        &pool,
        "UPDATE step_execution SET fencing_token = fencing_token + 1 \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'a'",
    )
    .await;
    assert!(seen, "リースの喪失で合図が立つ");
}
