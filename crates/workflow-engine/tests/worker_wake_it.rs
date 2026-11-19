//! run ワーカーの待機（#1334）の結合テスト（実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行する（未設定ならスキップ）。
//!
//! 検証: 空振りして長く眠っているワーカーが、step が ready になった通知（`step_execution` の
//! トリガ）で起きて run を進める／次の ready step の再試行時刻を引ける。

#![allow(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{NodeContext, NodeExecutor, NodeResult, RunStatus, RunStore, WorkerConfig};

/// 通知が届かなかったときに眠る時間。これより十分早く run が終われば通知で起きている。
const LONG: Duration = Duration::from_secs(30);

struct OkExecutor;

#[async_trait]
impl NodeExecutor for OkExecutor {
    async fn execute(&self, _t: &str, _p: &Value, _ctx: &NodeContext) -> NodeResult {
        NodeResult::ok(json!({}))
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
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

fn linear_ir() -> Value {
    json!({
        "ir_version": 1, "name": "linear",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "a", "type": "storage.read", "params": {} },
            { "id": "b", "type": "storage.read", "params": {} }
        ],
        "edges": [{ "from": "a", "to": "b" }]
    })
}

async fn create_run(store: &RunStore, tenant: &str) -> uuid::Uuid {
    let ir = linear_ir();
    let parsed = workflow_engine::WorkflowIr::from_json(&ir).unwrap();
    let graph = RunGraph::build(&parsed);
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
            &graph,
        )
        .await
        .expect("create_run")
        .expect("admitted")
}

async fn listening_waker(pool: &PgPool) -> jobq::Waker {
    let waker = jobq::Waker::listen_with_idle_max(vec![pool.clone()], LONG);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !waker.is_listening() {
        assert!(Instant::now() < deadline, "LISTEN を張れること");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    waker
}

#[tokio::test]
async fn idle_worker_wakes_when_a_step_becomes_ready() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let waker = listening_waker(&pool).await;
    let worker = workflow_engine::WorkflowWorker::new(
        store.clone(),
        Arc::new(OkExecutor),
        WorkerConfig {
            // LISTEN が効いていなければ 30 秒眠る（＝下の期限に間に合わない）。
            idle_poll: LONG,
            ..WorkerConfig::default()
        },
    )
    .scoped_to_tenant(&tenant)
    .with_jobq_waker(waker);
    let handles = worker.spawn(1, "wake-it");
    // 空振りして眠りに入るまで待つ。
    tokio::time::sleep(Duration::from_millis(500)).await;

    let started = Instant::now();
    let run_id = create_run(&store, &tenant).await;
    loop {
        let status = store.run_status(&tenant, run_id).await.unwrap();
        if status == Some(RunStatus::Succeeded) {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "通知で起きて run を終えること（status={status:?}）"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for h in handles {
        h.abort();
    }
}

#[tokio::test]
async fn next_ready_step_due_reports_the_head_of_the_ready_queue() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    assert_eq!(
        store.next_ready_step_due(Some(&tenant)).await.unwrap(),
        None,
        "ready な step が無ければ None"
    );
    let run_id = create_run(&store, &tenant).await;
    let due = store.next_ready_step_due(Some(&tenant)).await.unwrap();
    assert_eq!(due, Some(Duration::ZERO), "起動直後の step は今すぐ取れる");

    // 再試行待ち（next_retry_at が未来）なら、その時刻までの時間を返す。
    sqlx::query(
        "update step_execution set next_retry_at = now() + interval '20 seconds' \
         where tenant_id = $1 and run_id = $2 and status = 'ready'",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    let due = store
        .next_ready_step_due(Some(&tenant))
        .await
        .unwrap()
        .unwrap();
    assert!(
        due > Duration::from_secs(15) && due <= Duration::from_secs(20),
        "{due:?}"
    );

    // キャンセル要求済み run の step は claim されないので数えない。
    sqlx::query(
        "update workflow_run set cancel_requested = true where tenant_id = $1 and run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        store.next_ready_step_due(Some(&tenant)).await.unwrap(),
        None
    );
    // 横断版も引ける（他のテストの step が混ざるので値は問わない）。
    store.next_ready_step_due(None).await.unwrap();
}
