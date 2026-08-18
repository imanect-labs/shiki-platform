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
        RunStore::new(pool, workflow_engine::DEFAULT_LEASE_SECS),
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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

/// resume の猶予は固定値ではなく `lease_secs` から導出する（#446）。
///
/// 猶予は「まだ実行中かもしれない旧 worker と外部副作用を併走させない」ためにあるので、
/// リース期間より必ず長くなければならない。固定 35 秒だと `lease_secs` を既定の 30 から
/// 上げた構成（例: 120）で猶予がリースより短くなり、旧 worker が有効リースを保持したまま
/// 別 worker が再 claim できてしまう。
#[tokio::test]
async fn resume_grace_always_outlasts_the_configured_lease() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let ir = json!({
        "ir_version": 1, "name": "graceme",
        "nodes": [{ "id": "a", "type": "debug.log", "params": {} }],
        "edges": [], "triggers": [], "declared_scopes": []
    });

    for lease_secs in [30_i64, 120] {
        let store = RunStore::new(pool.clone(), lease_secs);
        let run_id = create_run(&store, &tenant, wf, &ir).await;
        // 「claim 済みで中断された step」＝失敗ドレインの cancelled かつ attempt>0 を再現する。
        // 猶予は `updated_at`（cancel 時刻）起算なので、fixture 側で明示する。省くと行作成時の
        // updated_at に暗黙依存し、テストの主張（猶予 > リース）が偶然通るだけになる。
        sqlx::query(
            "UPDATE step_execution SET status = 'cancelled', attempt = 1, updated_at = now() \
              WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&tenant)
        .bind(run_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE workflow_run SET status = 'failed', finished_at = now() \
              WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&tenant)
        .bind(run_id)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            store.resume_failed(&tenant, wf, run_id).await.unwrap(),
            ResumeOutcome::Resumed
        );

        let grace: f64 = sqlx::query_scalar(
            "SELECT extract(epoch FROM (next_retry_at - now()))::float8 \
               FROM step_execution WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&tenant)
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        #[allow(clippy::cast_precision_loss)]
        let lease = lease_secs as f64;
        assert!(
            grace > lease,
            "lease_secs={lease_secs} の猶予はリースより長くなければならない（実測 {grace:.1}s）"
        );
    }
}

/// 猶予の起算は resume 時刻ではなく step が cancelled になった時刻。
///
/// now() 起算だと、旧 worker がとうに死んでいる（cancel から `lease_secs` 以上経っている）
/// のに resume のたびに満額待たされる。`lease_secs` を上げた構成ほど死に時間が伸びる。
#[tokio::test]
async fn resume_grace_is_measured_from_cancel_time_not_resume_time() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let ir = json!({
        "ir_version": 1, "name": "staleme",
        "nodes": [{ "id": "a", "type": "debug.log", "params": {} }],
        "edges": [], "triggers": [], "declared_scopes": []
    });
    // lease_secs=300（猶予 305 秒）だが、cancel は 1 日前。
    let store = RunStore::new(pool.clone(), 300);
    let run_id = create_run(&store, &tenant, wf, &ir).await;
    sqlx::query(
        "UPDATE step_execution SET status = 'cancelled', attempt = 1, \
             updated_at = now() - interval '1 day' \
          WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE workflow_run SET status = 'failed', finished_at = now() \
          WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(
        store.resume_failed(&tenant, wf, run_id).await.unwrap(),
        ResumeOutcome::Resumed
    );

    let wait: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM (next_retry_at - now()))::float8 \
           FROM step_execution WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        wait <= 0.0,
        "旧リースは 1 日前に切れているので即時 ready にする（実測 {wait:.1}s 待ち）"
    );
}
