//! 並行制御・レート制限の結合テスト（Task 10.5 受け入れ条件）。
//!
//! - concurrency カウンタは上限まで予約でき、超過は拒否でなく「取れない（順番待ち）」
//! - 全階層 all-or-nothing（1 階層でも超過なら部分予約を残さない）
//! - Redis トークンバケットはバースト上限で頭打ち・補充で回復
//! - **結線後の e2e**: node 種上限で直列化・max_parallel_runs の queue/promote・
//!   overflow=skip・run timeout → failed(run_timeout)

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::concurrency::{ConcurrencyStore, Slot};
use workflow_engine::ratelimit::{BucketConfig, TokenBucket};
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{
    ConcurrencyLimits, NodeContext, NodeExecutor, NodeResult, RunListFilter, RunStore,
    WorkerConfig, WorkflowWorker,
};

async fn pg() -> Option<PgPool> {
    let url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

#[tokio::test]
async fn concurrency_limit_queues_beyond_capacity() {
    let Some(pool) = pg().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = ConcurrencyStore::new(pool.clone());

    // workflow 上限 2。
    let slots = vec![Slot::workflow(wf, 2)];
    assert!(
        store.try_acquire(&tenant, &slots).await.unwrap(),
        "1 本目は取れる"
    );
    assert!(
        store.try_acquire(&tenant, &slots).await.unwrap(),
        "2 本目も取れる"
    );
    assert!(
        !store.try_acquire(&tenant, &slots).await.unwrap(),
        "3 本目は上限で取れない（順番待ち）"
    );

    // 1 本解放すると再び取れる。
    store.release(&tenant, &slots).await.unwrap();
    assert!(
        store.try_acquire(&tenant, &slots).await.unwrap(),
        "解放後は取れる"
    );
}

#[tokio::test]
async fn multi_tier_is_all_or_nothing() {
    let Some(pool) = pg().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = ConcurrencyStore::new(pool.clone());

    // global 上限 5・node 上限 1。node を先に埋める。
    let node_slot = Slot::node_kind(wf, "storage.write", 1);
    let combo = vec![Slot::global(5), node_slot.clone()];

    assert!(
        store.try_acquire(&tenant, &combo).await.unwrap(),
        "初回は全階層取れる"
    );
    // node が上限 → combo は取れない。かつ global に部分予約が残らないこと。
    assert!(
        !store.try_acquire(&tenant, &combo).await.unwrap(),
        "node 上限で全体失敗"
    );

    // global 単独ならまだ 4 枠あるので取れる（部分予約が残っていない証拠）。
    let g: i32 = sqlx::query_scalar(
        "SELECT current_n FROM concurrency_counter \
         WHERE tenant_id = $1 AND scope_kind = 'global' AND scope_key = ''",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        g, 1,
        "global は 1（失敗した予約は巻き戻り 2 になっていない）"
    );
}

#[tokio::test]
async fn token_bucket_bursts_then_throttles() {
    let Ok(redis_url) = std::env::var("REDIS_TEST_URL") else {
        eprintln!("REDIS_TEST_URL 未設定のためスキップ");
        return;
    };
    let client = redis::Client::open(redis_url).expect("redis");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("conn");
    let bucket = TokenBucket::new(conn);
    let key = format!("test-{}", Uuid::new_v4());
    // 容量 3・補充ほぼ 0（バーストのみ検証）。
    let cfg = BucketConfig {
        capacity: 3,
        refill_per_sec: 0.001,
    };

    assert!(bucket.try_acquire(&key, cfg, 1).await.unwrap(), "1");
    assert!(bucket.try_acquire(&key, cfg, 1).await.unwrap(), "2");
    assert!(bucket.try_acquire(&key, cfg, 1).await.unwrap(), "3");
    assert!(
        !bucket.try_acquire(&key, cfg, 1).await.unwrap(),
        "4 本目はバースト超過で拒否"
    );

    // 補充を効かせて回復（容量 10・毎秒 1000 補充なら即回復）。
    let fast = BucketConfig {
        capacity: 10,
        refill_per_sec: 1000.0,
    };
    let key2 = format!("test-{}", Uuid::new_v4());
    for _ in 0..10 {
        assert!(bucket.try_acquire(&key2, fast, 1).await.unwrap());
    }
}

// ---------------------------------------------------------------------------
// 結線後 e2e（worker/launcher/tick 経由・10.5 受け入れ条件）
// ---------------------------------------------------------------------------

/// 実行中の同時数を観測する executor（sleep で並行の重なりを作る）。
struct GaugeExecutor {
    current: Arc<AtomicI32>,
    peak: Arc<AtomicI32>,
}

#[async_trait]
impl NodeExecutor for GaugeExecutor {
    async fn execute(&self, _t: &str, _p: &Value, ctx: &NodeContext) -> NodeResult {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        NodeResult::ok(json!({ "step": ctx.step_path }))
    }
}

/// 並列 3 ノード（fan-out）の IR。
fn fanout_ir(name: &str, policies: &Value) -> Value {
    json!({
        "ir_version": 1, "name": name,
        "declared_scopes": ["storage.read"],
        "policies": policies.clone(),
        "nodes": [
            { "id": "src", "type": "storage.read", "params": {} },
            { "id": "p1", "type": "storage.read", "params": {} },
            { "id": "p2", "type": "storage.read", "params": {} },
            { "id": "p3", "type": "storage.read", "params": {} }
        ],
        "edges": [
            { "from": "src", "to": "p1" }, { "from": "src", "to": "p2" },
            { "from": "src", "to": "p3" }
        ]
    })
}

async fn create_run(store: &RunStore, tenant: &str, wf: Uuid, ir: &Value) -> Option<Uuid> {
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
}

fn gauge_worker(pool: PgPool, tenant: &str) -> WorkflowWorker {
    WorkflowWorker::new(
        RunStore::new(pool),
        Arc::new(GaugeExecutor {
            current: Arc::new(AtomicI32::new(0)),
            peak: Arc::new(AtomicI32::new(0)),
        }),
        WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant)
}

#[tokio::test]
async fn node_kind_limit_serializes_steps_without_failing() {
    let Some(pool) = pg().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = RunStore::new(pool.clone());
    let ir = fanout_ir("conc-serialize", &json!({}));
    let run_id = create_run(&store, &tenant, wf, &ir)
        .await
        .expect("admitted");

    let current = Arc::new(AtomicI32::new(0));
    let peak = Arc::new(AtomicI32::new(0));
    let worker = WorkflowWorker::new(
        RunStore::new(pool.clone()),
        Arc::new(GaugeExecutor {
            current: Arc::clone(&current),
            peak: Arc::clone(&peak),
        }),
        WorkerConfig {
            idle_poll: std::time::Duration::from_millis(20),
            ..WorkerConfig::default()
        },
    )
    .scoped_to_tenant(&tenant)
    .with_concurrency(
        ConcurrencyStore::new(pool.clone()),
        ConcurrencyLimits {
            tenant_steps: 64,
            workflow_steps: 16,
            node_kind_steps: 1, // storage.read を直列化。
        },
    );

    // 3 並列タスクで回す（上限が無ければ p1..p3 が重なる）。
    let handles = worker.spawn(3, "conc");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = store.run_status(&tenant, run_id).await.unwrap();
        if status == Some(workflow_engine::RunStatus::Succeeded) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "30s 以内に完走するはず（status={status:?}）"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    for h in handles {
        h.abort();
    }
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "node 種上限 1 で同時実行が直列化される（順番待ちで失敗しない）"
    );
    let d = store
        .run_detail(&tenant, wf, run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(d.steps.iter().all(|s| s.status == "succeeded"));
}

#[tokio::test]
async fn max_parallel_runs_queues_and_promotes() {
    let Some(pool) = pg().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = RunStore::new(pool.clone());
    let ir = fanout_ir(
        "conc-queue",
        &json!({ "max_parallel_runs": 1, "on_trigger_overflow": "queue" }),
    );

    let r1 = create_run(&store, &tenant, wf, &ir).await.expect("r1");
    let r2 = create_run(&store, &tenant, wf, &ir)
        .await
        .expect("r2（queued で作成）");
    let d2 = store.run_detail(&tenant, wf, r2).await.unwrap().unwrap();
    assert_eq!(
        d2.status, "queued",
        "2 本目は滞留（拒否ではなくバックプレッシャ）"
    );
    assert!(
        d2.steps.iter().all(|s| s.status == "pending"),
        "queued run の step は claim されない"
    );

    // r1 が running のうちは promote されない。
    assert_eq!(store.promote_queued_runs(Some(&tenant)).await.unwrap(), 0);

    // r1 を完走させる。
    let worker = gauge_worker(pool.clone(), &tenant);
    while worker.claim_and_run_once("w1").await.unwrap() {}
    assert_eq!(
        store.run_status(&tenant, r1).await.unwrap(),
        Some(workflow_engine::RunStatus::Succeeded)
    );

    // promote → r2 が running になり完走できる。
    assert_eq!(store.promote_queued_runs(Some(&tenant)).await.unwrap(), 1);
    while worker.claim_and_run_once("w1").await.unwrap() {}
    assert_eq!(
        store.run_status(&tenant, r2).await.unwrap(),
        Some(workflow_engine::RunStatus::Succeeded)
    );
    // run.started は promote 時に 1 回だけ乗る。
    let events = store.list_events(&tenant, wf, r2, 0, 200).await.unwrap();
    assert_eq!(events.iter().filter(|e| e.kind == "run.started").count(), 1);
}

#[tokio::test]
async fn overflow_skip_creates_no_run() {
    let Some(pool) = pg().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = RunStore::new(pool.clone());
    let ir = fanout_ir(
        "conc-skip",
        &json!({ "max_parallel_runs": 1, "on_trigger_overflow": "skip" }),
    );
    let r1 = create_run(&store, &tenant, wf, &ir).await;
    assert!(r1.is_some());
    let r2 = create_run(&store, &tenant, wf, &ir).await;
    assert!(r2.is_none(), "skip は run を作らない");
    let all = store
        .list_runs(&tenant, wf, &RunListFilter::default(), None, 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn run_timeout_fails_run() {
    let Some(pool) = pg().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = RunStore::new(pool.clone());
    let ir = fanout_ir("conc-timeout", &json!({ "run_timeout_sec": 1 }));
    let run_id = create_run(&store, &tenant, wf, &ir).await.expect("run");

    // まだ実行していない状態で timeout を経過させる（tick 相当の呼び出し）。
    let n = store
        .expire_run_timeouts(
            chrono::Utc::now() + chrono::Duration::seconds(5),
            Some(&tenant),
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    let d = store
        .run_detail(&tenant, wf, run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.status, "failed", "timeout は cancelled でなく failed");
    assert_eq!(d.fail_reason.as_deref(), Some("run_timeout"));
    assert!(
        d.steps.iter().all(|s| s.status == "cancelled"),
        "未実行 step はドレインで cancelled"
    );
}
