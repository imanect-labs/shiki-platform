//! リースの切れた step の拾い直しの上限の結合テスト（実 Postgres・BUG-215 の残り）。
//!
//! 回収（sweeper）は attempt を戻すので、試す回数ではリースの切れる step を止められない。ワーカーが落ちる形を
//! くり返す step と、ワーカーが誤りを返して前進できない step（IR を読めない）は、上限まで拾い直したあとに
//! `internal` の失敗で確定し、run も失敗で終わる。再開すると数え直す。
//! `STORAGE_TEST_DATABASE_URL` が無ければスキップ。各テストは自分のテナントに閉じ、最後に run を消す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::run::node_panic::NODE_PANIC_CODE;
use workflow_engine::run::store::{LEASE_RECLAIMS_EXHAUSTED_MESSAGE, MAX_LEASE_RECLAIMS};
use workflow_engine::{
    NodeContext, NodeExecutor, NodeResult, ResumeOutcome, RunStatus, RunStore, WorkerConfig,
    WorkflowWorker,
};

/// 実行した回数を数えて成功する executor。
struct Counting(Arc<AtomicUsize>);

#[async_trait]
impl NodeExecutor for Counting {
    async fn execute(&self, _t: &str, _p: &Value, _c: &NodeContext) -> NodeResult {
        self.0.fetch_add(1, Ordering::SeqCst);
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

/// 分かれた 2 つのブロックの run（`a` と `b`・どちらもすぐ拾える）。
async fn two_step_run(store: &RunStore, tenant: &str) -> (Uuid, Uuid) {
    let ir = json!({
        "ir_version": 1, "name": "reclaims", "declared_scopes": [],
        "nodes": [
            { "id": "a", "type": "debug.log", "params": {} },
            { "id": "b", "type": "debug.log", "params": {} }
        ],
        "edges": []
    });
    let parsed = workflow_engine::WorkflowIr::from_json(&ir).unwrap();
    let workflow_id = Uuid::new_v4();
    let run = store
        .create_run(
            tenant,
            "acme",
            workflow_id,
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
        .expect("admitted");
    (run, workflow_id)
}

/// （状態, 誤りの code, 誤りの文, retryable, 拾い直した回数）。
type StepRow = (String, Option<String>, Option<String>, Option<bool>, i32);

async fn step_row(pool: &PgPool, tenant: &str, run: Uuid, path: &str) -> StepRow {
    sqlx::query_as(
        "SELECT status, error->>'code', error->>'message', (error->>'retryable')::bool, \
                lease_reclaims \
           FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant)
    .bind(run)
    .bind(path)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn cleanup(pool: &PgPool, tenant: &str) {
    sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1")
        .bind(tenant)
        .execute(pool)
        .await
        .unwrap();
}

/// 同じ step でワーカーが上限を超えて落ちたら、拾い直さずに失敗で確定し、run も失敗で終わる（残りの step は
/// 取り消す）。再開すると数え直して、最後まで流れる。
#[tokio::test]
async fn a_step_whose_lease_keeps_expiring_fails_after_the_limit() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", Uuid::new_v4());
    let (run, workflow_id) = two_step_run(&store, &tenant).await;

    // `a` だけを、落ちるワーカーが何度も拾う（`b` は拾わずに残す）。
    let mut reclaimed = Vec::new();
    for _ in 0..=MAX_LEASE_RECLAIMS {
        let claimed = loop {
            let c = store
                .claim_ready_step("w-dead", -1, Some(&tenant))
                .await
                .unwrap()
                .expect("claim");
            if c.step_path == "a" {
                break c;
            }
            // `b` はリースを長く持たせて、回収の対象にしない。
            sqlx::query(
                "UPDATE step_execution SET lease_expires_at = now() + interval '1 hour' \
                 WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'b'",
            )
            .bind(&tenant)
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        };
        assert_eq!(
            claimed.attempt, 1,
            "落ちたワーカーの実行は試す回数に数えない"
        );
        reclaimed.push(store.reclaim_expired_leases(Some(&tenant)).await.unwrap());
    }
    let a = step_row(&pool, &tenant, run, "a").await;
    let b = step_row(&pool, &tenant, run, "b").await;
    let status = store.run_status(&tenant, run).await.unwrap();
    let again = store
        .claim_ready_step("w-x", 60, Some(&tenant))
        .await
        .unwrap();
    let events: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT kind, payload->>'reason' FROM run_event WHERE tenant_id = $1 AND run_id = $2 \
         ORDER BY seq",
    )
    .bind(&tenant)
    .bind(run)
    .fetch_all(&pool)
    .await
    .unwrap();

    // 再開すると数え直し、健全なワーカーが最後まで流す。
    let resumed = store
        .resume_failed(&tenant, workflow_id, run)
        .await
        .unwrap();
    let after_resume = step_row(&pool, &tenant, run, "a").await;
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = WorkflowWorker::new(
        store.clone(),
        Arc::new(Counting(Arc::clone(&calls))),
        WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant.clone());
    // `b` は再開で取り消しから戻り、旧リースの猶予のあとに拾える。猶予を待たずに拾えるようにする。
    sqlx::query("UPDATE step_execution SET next_retry_at = now() WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
    while worker.claim_and_run_once("w-ok").await == Ok(true) {}
    let final_status = store.run_status(&tenant, run).await.unwrap();
    cleanup(&pool, &tenant).await;

    let max = usize::try_from(MAX_LEASE_RECLAIMS).unwrap();
    assert_eq!(reclaimed, [vec![1; max], vec![1]].concat());
    assert_eq!(
        a,
        (
            "failed".to_string(),
            Some(NODE_PANIC_CODE.to_string()),
            Some(LEASE_RECLAIMS_EXHAUSTED_MESSAGE.to_string()),
            Some(false),
            MAX_LEASE_RECLAIMS,
        ),
        "上限まで拾い直したあとは、拾い直さずに失敗で確定する"
    );
    assert_eq!(b.0, "cancelled", "run の残りの step は取り消す");
    assert_eq!(status, Some(RunStatus::Failed));
    assert!(again.is_none(), "もう拾うものが無い");
    assert!(
        events.ends_with(&[
            (
                "step.failed".to_string(),
                Some("lease_reclaims_exhausted".to_string())
            ),
            ("run.failed".to_string(), None),
        ]),
        "{events:?}"
    );
    assert_eq!(resumed, ResumeOutcome::Resumed);
    assert_eq!(after_resume.4, 0, "再開すると数え直す");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(final_status, Some(RunStatus::Succeeded));
}

/// ワーカーが誤りを返して前進できない step（ピンした IR を読めない）は、リースが切れるたびに拾い直されていた。上限で
/// 止まり、ブロックは一度も動かない。
#[tokio::test]
async fn a_step_the_worker_cannot_advance_stops_at_the_limit() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), 1);
    let tenant = format!("t-{}", Uuid::new_v4());
    let (run, _) = two_step_run(&store, &tenant).await;
    sqlx::query(
        "UPDATE workflow_run SET ir_snapshot = '{\"nodes\": 5}'::jsonb \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run)
    .execute(&pool)
    .await
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = WorkflowWorker::new(
        store.clone(),
        Arc::new(Counting(Arc::clone(&calls))),
        WorkerConfig {
            lease_secs: 1,
            idle_poll: Duration::from_millis(50),
            http_timeout_ms: None,
        },
    )
    .scoped_to_tenant(tenant.clone());

    let mut claims = 0;
    let mut status = store.run_status(&tenant, run).await.unwrap();
    for _ in 0..30 {
        while let Err(e) = worker.claim_and_run_once("w").await {
            assert!(!e.is_empty());
            claims += 1;
        }
        status = store.run_status(&tenant, run).await.unwrap();
        if status != Some(RunStatus::Running) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
        store.reclaim_expired_leases(Some(&tenant)).await.unwrap();
    }
    let a = step_row(&pool, &tenant, run, "a").await;
    let b = step_row(&pool, &tenant, run, "b").await;
    cleanup(&pool, &tenant).await;

    assert_eq!(status, Some(RunStatus::Failed), "上限で止まらない");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "ブロックは動かない");
    // 2 つの step がそれぞれ（最初の 1 回 ＋ 上限まで拾い直した回数）だけ拾われる（片方が止めた run は残りを取り消す）。
    let per_step = usize::try_from(MAX_LEASE_RECLAIMS).unwrap() + 1;
    assert!(
        (per_step..=2 * per_step).contains(&claims),
        "拾った回数: {claims}"
    );
    let failed = [&a, &b]
        .into_iter()
        .filter(|s| s.0 == "failed")
        .collect::<Vec<_>>();
    assert_eq!(failed.len(), 1, "{a:?} {b:?}");
    assert_eq!(failed[0].1.as_deref(), Some(NODE_PANIC_CODE));
}
