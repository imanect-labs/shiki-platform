//! claim（ready 専用）とリース失効回収 sweeper の結合テスト（#438・実 Postgres）。
//!
//! - claim の実行計画が Index Scan → Limit のままで、Sort / Bitmap に退化していない
//! - リース失効 step を sweeper が回収し、**attempt を消費しない**（engine.md §9.5）
//! - running 中の `attempt` は常に「今回が何回目か」（実行履歴 UI がそのまま表示する）
//! - retryable 失敗は試行を 1 ずつ消費し、max_attempts で terminal 化する
//! - cancel 要求済み run の step は sweeper が触らない（drain が回収する担当）

#![allow(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{NodeResult, RunStore, StepStatus};

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

/// 単一ノードの最小 IR（claim/リースの検証はグラフ形状に依らない）。
fn single_node_ir() -> Value {
    json!({
        "ir_version": 1, "name": "claimme",
        "nodes": [{ "id": "a", "type": "debug.log", "params": {} }],
        "edges": [],
        "triggers": [], "declared_scopes": []
    })
}

async fn create_run(store: &RunStore, tenant: &str) -> Uuid {
    let ir = single_node_ir();
    let graph = RunGraph::build(&workflow_engine::WorkflowIr::from_json(&ir).unwrap());
    store
        .create_run(
            tenant,
            "org",
            Uuid::new_v4(),
            1,
            "interactive",
            None,
            "u:1",
            "user",
            &json!({}),
            &ir,
            &graph,
        )
        .await
        .unwrap()
        .expect("run 作成")
}

/// step_execution の (attempt, fencing_token, status) を直接読む。
async fn step_row(pool: &PgPool, tenant: &str, run_id: Uuid) -> (i32, i64, String) {
    sqlx::query_as(
        "SELECT attempt, fencing_token, status FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'a'",
    )
    .bind(tenant)
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 計画テスト用に step を積む（数十行では Seq Scan が正当に選ばれるため、index が効く規模を作る）。
async fn seed_steps(pool: &PgPool, tenant: &str, runs: i32, steps_per_run: i32) {
    sqlx::query(
        "INSERT INTO workflow_run (tenant_id, run_id, org, workflow_id, version, \
             trigger_kind, principal, status) \
         SELECT $1, gen_random_uuid(), 'org', gen_random_uuid(), 1, 'interactive', 'u:1', 'running' \
         FROM generate_series(1, $2)",
    )
    .bind(tenant)
    .bind(runs)
    .execute(pool)
    .await
    .unwrap();
    // 大半は terminal（実運用と同じく「終わった step が積み上がった」状態）。
    sqlx::query(
        "INSERT INTO step_execution (tenant_id, run_id, step_path, node_id, status, \
             next_retry_at, idempotency_key) \
         SELECT r.tenant_id, r.run_id, 'n' || s, 'n' || s, 'succeeded', now() - interval '1 day', \
                'wf:' || r.tenant_id || ':' || r.run_id || ':n' || s \
         FROM workflow_run r, generate_series(1, $2) s \
         WHERE r.tenant_id = $1",
    )
    .bind(tenant)
    .bind(steps_per_run)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE step_execution, workflow_run")
        .execute(pool)
        .await
        .unwrap();
}

/// claim が「ready の partial index を並び順ごと使い、1 件で打ち切る」計画のままか。
///
/// 退化すると claim コストが O(実行待ち件数) に戻る（#438）。検証するのは **候補選択の形**だけで、
/// 周辺の join が Seq Scan かどうかは行数次第で変わるため見ない。
#[tokio::test]
async fn claim_plan_stays_index_ordered() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", Uuid::new_v4());
    let _ = create_run(&store, &tenant).await;
    seed_steps(&pool, &tenant, 2_000, 10).await;

    for scope in [None, Some(tenant.as_str())] {
        let plan = store.explain_claim_ready_step(scope).await.unwrap();
        let label = if scope.is_some() {
            "tenant 固定"
        } else {
            "全テナント横断"
        };
        assert!(
            plan.contains("Index Scan using step_ready"),
            "{label} claim が ready の partial index を並び順ごと使っていない:\n{plan}"
        );
        assert!(
            !plan.contains("Sort"),
            "{label} claim の計画に Sort が出ている。index の並び順で LIMIT 1 に到達できておらず、\
             候補を全件集めてから 1 件選ぶ形に退化している:\n{plan}"
        );
        assert!(
            !plan.contains("Bitmap Index Scan on step_ready"),
            "{label} claim が ready index の Bitmap 走査に退化している。\
             claim に OR 条件（リース失効 running の回収など）を足していないか:\n{plan}"
        );
    }
}

/// リース失効 → sweeper が ready へ戻す。**attempt は消費されない**（engine.md §9.5）。
#[tokio::test]
async fn sweeper_reclaims_expired_lease_without_consuming_attempt() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", Uuid::new_v4());
    let run_id = create_run(&store, &tenant).await;

    // リースを過去に切って claim（＝ワーカーが即死した状態を作る）。
    let first = store
        .claim_ready_step("w-dead", -1, Some(&tenant))
        .await
        .unwrap()
        .expect("claim");
    assert_eq!(first.attempt, 1, "初回 claim は 1 回目の実行");
    let (attempt_db, fencing_first, status) = step_row(&pool, &tenant, run_id).await;
    assert_eq!(status, "running");
    assert_eq!(
        attempt_db, 1,
        "running 中の attempt は「今回が何回目か」（実行履歴 UI がそのまま表示する）"
    );

    // claim はもう拾わない（ready 専用になったため takeover は sweeper の担当）。
    assert!(
        store
            .claim_ready_step("w2", 60, Some(&tenant))
            .await
            .unwrap()
            .is_none(),
        "claim は失効 running を拾わない"
    );

    // sweeper が回収して ready に戻す。
    let reclaimed = store.reclaim_expired_leases(Some(&tenant)).await.unwrap();
    assert_eq!(reclaimed, 1);
    let (attempt_db, fencing_after, status) = step_row(&pool, &tenant, run_id).await;
    assert_eq!(status, "ready");
    assert_eq!(
        attempt_db, 0,
        "クラッシュした実行は試行として数えない（claim の +1 を打ち消す）"
    );
    assert_eq!(
        fencing_after, fencing_first,
        "sweeper は fencing を進めない"
    );

    // 別ワーカーが拾い直しても「1 回目の実行」のまま（クラッシュで試行を減らさない）。
    let second = store
        .claim_ready_step("w2", 60, Some(&tenant))
        .await
        .unwrap()
        .expect("再 claim");
    assert_eq!(second.attempt, 1, "takeover は試行を消費しない（§9.5）");
    assert!(
        second.fencing_token > first.fencing_token,
        "再 claim は fencing を進めてゾンビ書込を無効化する"
    );
}

/// retryable 失敗は試行を 1 ずつ消費し、max_attempts 回実行したところで terminal 化する。
#[tokio::test]
async fn retryable_failure_consumes_one_attempt_per_execution() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", Uuid::new_v4());
    let run_id = create_run(&store, &tenant).await;
    let ir = single_node_ir();
    let graph = RunGraph::build(&workflow_engine::WorkflowIr::from_json(&ir).unwrap());
    let retryable = NodeResult::fail("upstream_unavailable", "boom", true);
    let max_attempts = 3;

    for expected in 1..=max_attempts {
        let claimed = store
            .claim_ready_step("w1", 60, Some(&tenant))
            .await
            .unwrap()
            .expect("claim");
        assert_eq!(
            claimed.attempt, expected,
            "claim は「今回が何回目か」を返す（1-origin）"
        );
        store
            .checkpoint_and_advance(
                &claimed,
                &retryable,
                &graph,
                max_attempts,
                workflow_engine::ir::OnError::FailRun,
            )
            .await
            .unwrap();
        let (attempt_db, _, _) = step_row(&pool, &tenant, run_id).await;
        assert_eq!(
            attempt_db, expected,
            "retryable は claim が数えた 1 回をそのまま消費する"
        );
        // backoff を待たずに次を claim できるようにする。
        sqlx::query(
            "UPDATE step_execution SET next_retry_at = now() \
             WHERE tenant_id = $1 AND run_id = $2 AND status = 'ready'",
        )
        .bind(&tenant)
        .bind(run_id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let a = statuses.iter().find(|(p, _)| p == "a").unwrap();
    assert_eq!(a.1, StepStatus::Failed, "max_attempts 到達で terminal");
}

/// cancel 要求済み run の失効リースは sweeper の対象外（`drain_cancel_requested` が回収する担当）。
#[tokio::test]
async fn sweeper_skips_cancel_requested_runs() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", Uuid::new_v4());
    let run_id = create_run(&store, &tenant).await;

    store
        .claim_ready_step("w-dead", -1, Some(&tenant))
        .await
        .unwrap()
        .expect("claim");
    sqlx::query(
        "UPDATE workflow_run SET cancel_requested = true WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();

    let reclaimed = store.reclaim_expired_leases(Some(&tenant)).await.unwrap();
    assert_eq!(reclaimed, 0, "cancel 要求済み run は sweeper が触らない");
    let (_, _, status) = step_row(&pool, &tenant, run_id).await;
    assert_eq!(status, "running", "running のまま drain へ渡す");
}
