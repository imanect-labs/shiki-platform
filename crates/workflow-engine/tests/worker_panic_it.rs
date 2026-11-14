//! ブロックの実行中の panic の結合テスト（実 Postgres・BUG-215・engine.md §9.5）。
//!
//! panic したブロックの step は失敗（`internal`・やり直さない）になり、run も失敗で終わる。ワーカーのタスクは
//! 止まらずに次の run を流す。前は panic でワーカーのタスクが終わり、heartbeat がリースを延ばし続けて step と run が
//! 実行中のまま残っていた（次の run も流れない）。`STORAGE_TEST_DATABASE_URL` が無ければスキップ。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::run::node_panic::{NODE_PANIC_CODE, NODE_PANIC_MESSAGE};
use workflow_engine::{
    NodeContext, NodeExecutor, NodeResult, RunStatus, RunStore, StepStatus, WorkerConfig,
    WorkflowWorker,
};

/// 秘密めいた文を含む panic の文（利用者に出ないことを確かめる）。
const PANIC_TEXT: &str = "boom: /srv/secret/path at index 7";

/// params の `boom` が真なら panic する executor（実行した回数も数える）。
struct PanicsOnBoom {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl NodeExecutor for PanicsOnBoom {
    async fn execute(&self, _t: &str, p: &Value, _ctx: &NodeContext) -> NodeResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // `boom` なら panic する（ブロックの実装の不具合のかわり）。
        let boom = p.get("boom").and_then(Value::as_bool) == Some(true);
        assert!(!boom, "{PANIC_TEXT}");
        NodeResult::ok(json!({ "done": true }))
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

/// 1 ブロックだけの run（`boom` なら panic する・試す回数は 3）。
async fn one_step_run(store: &RunStore, tenant: &str, boom: bool) -> uuid::Uuid {
    let ir = json!({
        "ir_version": 1, "name": "panics", "declared_scopes": ["storage.read"],
        "nodes": [{
            "id": "rd", "type": "storage.read",
            "retry": { "max_attempts": 3,
                       "backoff": { "base_sec": 1, "max_sec": 1, "jitter": false } },
            "params": { "file": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", "boom": boom }
        }],
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

fn worker(pool: &PgPool, tenant: &str, exec: Arc<dyn NodeExecutor>) -> WorkflowWorker {
    WorkflowWorker::new(
        RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS),
        exec,
        WorkerConfig {
            idle_poll: Duration::from_millis(50),
            ..WorkerConfig::default()
        },
    )
    .scoped_to_tenant(tenant)
}

/// step の（状態, code, retryable, 文, attempt, リースの持ち主）。
async fn step_row(
    pool: &PgPool,
    tenant: &str,
    run: uuid::Uuid,
) -> (
    String,
    Option<String>,
    Option<bool>,
    Option<String>,
    i32,
    Option<String>,
) {
    sqlx::query_as(
        "SELECT status, error->>'code', (error->>'retryable')::bool, error->>'message', attempt, \
                lease_owner \
           FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
    )
    .bind(tenant)
    .bind(run)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn forget(pool: &PgPool, tenant: &str) {
    sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1")
        .bind(tenant)
        .execute(pool)
        .await
        .unwrap();
}

/// panic したブロックの step は、やり直さずに失敗になり、run も失敗で終わる。誤りの文は決まった文で、panic の文を
/// 含まない。テストからの 1 回の処理も panic を外に漏らさない。
#[tokio::test]
async fn a_panicking_block_fails_its_step_and_its_run() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run = one_step_run(&store, &tenant, true).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let w = worker(
        &pool,
        &tenant,
        Arc::new(PanicsOnBoom {
            calls: Arc::clone(&calls),
        }),
    );
    let handled = w.claim_and_run_once("w1").await;
    let status = store.run_status(&tenant, run).await.unwrap();
    let step = step_row(&pool, &tenant, run).await;
    // やり直しを待たずに、もう拾うものが無いこと（試す回数が残っていてもやり直さない）。
    let again = w.claim_and_run_once("w1").await;
    forget(&pool, &tenant).await;

    assert_eq!(handled, Ok(true));
    assert_eq!(status, Some(RunStatus::Failed));
    assert_eq!(
        step,
        (
            StepStatus::Failed.as_str().to_string(),
            Some(NODE_PANIC_CODE.to_string()),
            Some(false),
            Some(NODE_PANIC_MESSAGE.to_string()),
            1,
            None,
        )
    );
    assert!(!NODE_PANIC_MESSAGE.contains("boom"));
    assert_eq!(again, Ok(false));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// ワーカーのタスクは panic で止まらず、panic した run を失敗で終えてから、次の run を流す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_worker_keeps_running_after_a_block_panics() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let calls = Arc::new(AtomicUsize::new(0));
    let w = worker(
        &pool,
        &tenant,
        Arc::new(PanicsOnBoom {
            calls: Arc::clone(&calls),
        }),
    );
    // ワーカーは 1 つだけ（panic で止まれば、次の run を流すものがいない）。
    let handles = w.spawn(1, "panic-it");
    let first = one_step_run(&store, &tenant, true).await;
    let mut seen = Vec::new();
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        let s = store.run_status(&tenant, first).await.unwrap();
        if s != Some(RunStatus::Running) {
            seen.push(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let second = one_step_run(&store, &tenant, false).await;
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        let s = store.run_status(&tenant, second).await.unwrap();
        if s != Some(RunStatus::Running) {
            seen.push(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let finished: Vec<bool> = handles
        .iter()
        .map(tokio::task::JoinHandle::is_finished)
        .collect();
    for h in &handles {
        h.abort();
    }
    forget(&pool, &tenant).await;

    assert_eq!(
        seen,
        vec![Some(RunStatus::Failed), Some(RunStatus::Succeeded)],
        "panic した run は失敗で終わり、次の run は流れる"
    );
    assert_eq!(finished, vec![false], "ワーカーのタスクは終わらない");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
