//! step の時間の上限（ノードの `timeout_sec`）の結合テスト（実 Postgres・BUG-96・D-82・engine.md §9.4）。
//!
//! 上限を過ぎた step はワーカーが打ち切り、やり直せる失敗（`step_timeout`）として試す回数の規則に従う。
//! 以前は `timeout_sec` をどこでも読まず、入れても何も起きなかった。`STORAGE_TEST_DATABASE_URL` が無ければスキップ。

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
use workflow_engine::run::step_timeout::STEP_TIMEOUT;
use workflow_engine::{
    FailureCategory, NodeContext, NodeExecutor, NodeResult, RunStatus, RunStore, StepStatus,
    WorkerConfig, WorkflowWorker,
};

/// 1 回目だけ 30 秒かかり（打ち切られる）、2 回目からはすぐ終わる executor。止める合図が立ったかも数える。
struct SlowOnce {
    calls: Arc<AtomicUsize>,
    cancelled: Arc<AtomicUsize>,
}

#[async_trait]
impl NodeExecutor for SlowOnce {
    async fn execute(&self, _t: &str, _p: &Value, ctx: &NodeContext) -> NodeResult {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let cancel = ctx.cancel.clone();
            let cancelled = Arc::clone(&self.cancelled);
            // 打ち切られた後に合図が立つことを、別のタスクで見る（このフューチャは捨てられる）。
            tokio::spawn(async move {
                let until = Instant::now() + Duration::from_secs(10);
                while Instant::now() < until {
                    if cancel.is_cancelled() {
                        cancelled.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            });
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        NodeResult::ok(json!({ "done": true }))
    }
}

async fn setup() -> Option<PgPool> {
    let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
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

/// 時間の上限 1 秒の storage.read だけの run を作る（`retry` は試す回数）。
async fn one_step_run(store: &RunStore, tenant: &str, max_attempts: u32) -> uuid::Uuid {
    one_step_run_of(store, tenant, "storage.read", max_attempts).await
}

/// 時間の上限 1 秒の `node_type` のブロックだけの run を作る（executor は試験用なので params は見ない）。
async fn one_step_run_of(
    store: &RunStore,
    tenant: &str,
    node_type: &str,
    max_attempts: u32,
) -> uuid::Uuid {
    let ir = json!({
        "ir_version": 1, "name": "slow", "declared_scopes": ["storage.read"],
        "nodes": [{
            "id": "rd", "type": node_type, "timeout_sec": 1,
            "retry": { "max_attempts": max_attempts,
                       "backoff": { "base_sec": 1, "max_sec": 1, "jitter": false } },
            "params": { "file": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" }
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
        WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant)
}

async fn step_error(
    pool: &PgPool,
    tenant: &str,
    run: uuid::Uuid,
) -> (String, Option<String>, Option<bool>, i32) {
    sqlx::query_as(
        "SELECT status, error->>'code', (error->>'retryable')::bool, attempt FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
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

/// 試す回数が 1 なら、上限を過ぎた step は打ち切られて失敗し、run も失敗する。理由は時間切れ（やり直せる失敗）。
#[tokio::test]
async fn steps_past_their_time_limit_fail_as_a_timeout() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run = one_step_run(&store, &tenant, 1).await;
    let exec = Arc::new(SlowOnce {
        calls: Arc::new(AtomicUsize::new(0)),
        cancelled: Arc::new(AtomicUsize::new(0)),
    });
    let w = worker(&pool, &tenant, Arc::clone(&exec) as Arc<dyn NodeExecutor>);
    let started = Instant::now();
    assert!(w.claim_and_run_once("w1").await.unwrap());
    let elapsed = started.elapsed();
    let status = store.run_status(&tenant, run).await.unwrap();
    let step = step_error(&pool, &tenant, run).await;
    // 合図を見るタスクが動く間を置く。
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cancelled = exec.cancelled.load(Ordering::SeqCst);
    forget(&pool, &tenant).await;

    assert!(
        elapsed < Duration::from_secs(10),
        "1 秒の上限で打ち切る: {elapsed:?}"
    );
    assert_eq!(status, Some(RunStatus::Failed));
    assert_eq!(
        step,
        (
            StepStatus::Failed.as_str().to_string(),
            Some(STEP_TIMEOUT.to_string()),
            Some(true),
            1
        )
    );
    assert_eq!(cancelled, 1, "打ち切ったら止める合図も立てる");
    assert_eq!(
        workflow_engine::run::failure::category_of(STEP_TIMEOUT),
        FailureCategory::Timeout
    );
}

/// 打ち切りはやり直せる失敗なので、試す回数が残っていればやり直し、2 回目で成功する（retry の規則どおり）。
#[tokio::test]
async fn timed_out_steps_are_retried_within_their_attempts() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run = one_step_run(&store, &tenant, 2).await;
    let exec = Arc::new(SlowOnce {
        calls: Arc::new(AtomicUsize::new(0)),
        cancelled: Arc::new(AtomicUsize::new(0)),
    });
    let w = worker(&pool, &tenant, Arc::clone(&exec) as Arc<dyn NodeExecutor>);
    assert!(w.claim_and_run_once("w1").await.unwrap());
    let after_first = (
        store.run_status(&tenant, run).await.unwrap(),
        step_error(&pool, &tenant, run).await,
    );
    // backoff（1 秒）を待たずに拾えるよう、次に試す時刻を今にする。
    sqlx::query(
        "UPDATE step_execution SET next_retry_at = now() WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run)
    .execute(&pool)
    .await
    .unwrap();
    let mut ran = false;
    for _ in 0..50 {
        if w.claim_and_run_once("w1").await.unwrap() {
            ran = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let final_status = store.run_status(&tenant, run).await.unwrap();
    let calls = exec.calls.load(Ordering::SeqCst);
    forget(&pool, &tenant).await;

    // 1 回目の打ち切りでは run は続き、step は 1 回目を数えたまま次に試す順番を待つ（やり直せる失敗）。
    assert_eq!(after_first.0, Some(RunStatus::Running));
    assert_eq!(
        (after_first.1 .0.as_str(), after_first.1 .3),
        (StepStatus::Ready.as_str(), 1)
    );
    assert!(ran, "やり直しを拾う");
    assert_eq!(calls, 2);
    assert_eq!(final_status, Some(RunStatus::Succeeded));
}

/// 次に試すまでの秒（ready に戻した step の `next_retry_at - now()`）。
async fn seconds_until_retry(pool: &PgPool, tenant: &str, run: uuid::Uuid) -> f64 {
    sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM next_retry_at - now())::float8 FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
    )
    .bind(tenant)
    .bind(run)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 効果の記録の予約を step の外の Tx で取る種（CSV を保存など）を打ち切ったら、やり直しは予約を回収できるまで
/// （回収の猶予 300 秒より後）待つ。回収の前にやり直すと「別のワーカーが処理中」で試す回数を使い切り、効果が
/// 済んでいても失敗になっていた。予約を残さない種（ファイルを読む）は、いままでどおり backoff だけ待つ。
#[tokio::test]
async fn timeouts_that_may_leave_a_reservation_retry_after_the_reclaim() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let mut waits = Vec::new();
    for node_type in ["csv.write", "storage.read"] {
        let run = one_step_run_of(&store, &tenant, node_type, 3).await;
        let exec = Arc::new(SlowOnce {
            calls: Arc::new(AtomicUsize::new(0)),
            cancelled: Arc::new(AtomicUsize::new(0)),
        });
        let w = worker(&pool, &tenant, exec as Arc<dyn NodeExecutor>);
        assert!(w.claim_and_run_once("w1").await.unwrap());
        let step = step_error(&pool, &tenant, run).await;
        waits.push((
            node_type,
            step.0,
            seconds_until_retry(&pool, &tenant, run).await,
        ));
    }
    forget(&pool, &tenant).await;

    let (_, status, csv_wait) = &waits[0];
    assert_eq!(status, StepStatus::Ready.as_str(), "やり直す");
    assert!(
        *csv_wait > 300.0,
        "CSV を保存の打ち切りは、予約を回収できる 300 秒より後にやり直す: {csv_wait}"
    );
    let (_, status, read_wait) = &waits[1];
    assert_eq!(status, StepStatus::Ready.as_str());
    assert!(
        *read_wait < 10.0,
        "ファイルを読むの打ち切りは backoff（1 秒）だけ待つ: {read_wait}"
    );
}

