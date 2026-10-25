//! イベントトリガの発火の結果・再試行・抑止の結合テスト（GAP-07 の T0・T2・実 Postgres）。
//!
//! - 起動の一時失敗は捨てずに再試行し、回数の上限で打ち切る（D-21）
//! - 委譲の不成立・多重度あふれの見送りは再試行しない
//! - 同じイベントで run を二重に作らない（`(trigger_id, trigger_event_id)` の一意）
//! - 因果の鎖: 自分の書込で自分を起こさない・循環・深さ（D-26）。起動した run に鎖を引き継ぐ
//! - queued の上限で発火を捨てる

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{
    EventTriggerRef, FireOutcome, LaunchOutcome, RunLauncher, RunOrigin, RunStore, SchedulerStore,
    SkipReason, WorkflowIr,
};

/// 決めた結果を順に返す launcher（尽きたら最後の結果を返し続ける）。呼ばれた event_id を記録する。
struct ScriptedLauncher {
    outcomes: Mutex<Vec<LaunchOutcome>>,
    calls: Mutex<Vec<Option<i64>>>,
}

impl ScriptedLauncher {
    fn new(outcomes: Vec<LaunchOutcome>) -> Self {
        ScriptedLauncher {
            outcomes: Mutex::new(outcomes),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<Option<i64>> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl RunLauncher for ScriptedLauncher {
    async fn launch(
        &self,
        _t: &str,
        _w: Uuid,
        _k: &str,
        _tid: &str,
        _payload: &Value,
        event_id: Option<i64>,
    ) -> LaunchOutcome {
        self.calls.lock().unwrap().push(event_id);
        let mut o = self.outcomes.lock().unwrap();
        if o.len() > 1 {
            o.remove(0)
        } else {
            o[0].clone()
        }
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
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

/// 有効な registration とイベントトリガ 1 本。
async fn register(pool: &PgPool, tenant: &str, workflow_id: Uuid, trigger_id: &str) {
    sqlx::query(
        "INSERT INTO workflow_registration (tenant_id, workflow_id, org, status, enabled_version, consented_scopes) \
         VALUES ($1, $2, 'acme', 'enabled', 1, '{}')",
    )
    .bind(tenant)
    .bind(workflow_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_trigger (tenant_id, trigger_id, workflow_id, version, kind, source, spec) \
         VALUES ($1, $2, $3, 1, 'event', 'data.record.created', $4)",
    )
    .bind(tenant)
    .bind(trigger_id)
    .bind(workflow_id)
    .bind(sqlx::types::Json(json!({ "scope": { "table": Uuid::new_v4() } })))
    .execute(pool)
    .await
    .unwrap();
}

/// firing の outcome・attempts・run_id。
async fn firing(
    pool: &PgPool,
    tenant: &str,
    trigger_id: &str,
    event_id: i64,
) -> (String, i32, Option<Uuid>) {
    sqlx::query_as(
        "SELECT outcome, attempts, run_id FROM trigger_firing \
         WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3",
    )
    .bind(tenant)
    .bind(trigger_id)
    .bind(event_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 再試行の時刻を今にする（指数で延ばした待ちを飛ばす）。
async fn make_due(pool: &PgPool, tenant: &str) {
    sqlx::query(
        "UPDATE trigger_firing SET next_attempt_at = now() - interval '1 second' \
         WHERE tenant_id = $1 AND outcome = 'pending'",
    )
    .bind(tenant)
    .execute(pool)
    .await
    .unwrap();
}

/// 由来の run（ワークフロー `workflow_id`・鎖 `chain`）を 1 つ作る。
async fn cause_run(pool: &PgPool, tenant: &str, workflow_id: Uuid, chain: &[Uuid]) -> Uuid {
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_run (tenant_id, run_id, org, workflow_id, version, trigger_kind, \
                                   principal, status, trigger_chain) \
         VALUES ($1, $2, 'acme', $3, 1, 'event', $3::text, 'running', $4)",
    )
    .bind(tenant)
    .bind(run_id)
    .bind(workflow_id)
    .bind(chain)
    .execute(pool)
    .await
    .unwrap();
    run_id
}

fn target(tenant: &str, workflow_id: Uuid, trigger_id: &str) -> EventTriggerRef {
    EventTriggerRef {
        tenant_id: tenant.into(),
        trigger_id: trigger_id.into(),
        workflow_id,
    }
}

/// 起動の一時失敗は pending で再試行を待ち、再試行で起動する。同じ event_id で起こし直す。
#[tokio::test]
async fn transient_launch_failure_is_retried_with_the_same_event() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    register(&pool, &tenant, wf, "trg-1").await;
    let sched = SchedulerStore::new(pool.clone());
    let run = Uuid::new_v4();
    let launcher = ScriptedLauncher::new(vec![
        LaunchOutcome::Failed("db".into()),
        LaunchOutcome::Started(run),
    ]);
    let t = target(&tenant, wf, "trg-1");
    let out = sched
        .fire_event_trigger(&t, 41, &json!({ "record_id": "r1" }), None, &launcher)
        .await
        .unwrap();
    assert_eq!(out, FireOutcome::Pending);
    assert_eq!(
        firing(&pool, &tenant, "trg-1", 41).await,
        ("pending".into(), 1, None)
    );
    // 時刻が来るまでは再試行しない。
    let r = sched
        .retry_pending_firings(Utc::now(), Some(&tenant), &launcher)
        .await
        .unwrap();
    assert_eq!(r.fired, 0);
    make_due(&pool, &tenant).await;
    let r = sched
        .retry_pending_firings(Utc::now(), Some(&tenant), &launcher)
        .await
        .unwrap();
    assert_eq!(r.fired, 1);
    assert_eq!(
        firing(&pool, &tenant, "trg-1", 41).await,
        ("fired".into(), 1, Some(run))
    );
    assert_eq!(launcher.calls(), vec![Some(41), Some(41)]);
    // 同じイベントの再配送は起動し直さない。
    let again = sched
        .fire_event_trigger(&t, 41, &json!({}), None, &launcher)
        .await
        .unwrap();
    assert_eq!(again, FireOutcome::AlreadyHandled);
    assert_eq!(launcher.calls().len(), 2);
}

/// 一時失敗が続けば上限で打ち切る（gave_up・監査の材料を返す）。
#[tokio::test]
async fn repeated_failures_give_up_after_the_limit() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    register(&pool, &tenant, wf, "trg-1").await;
    let sched = SchedulerStore::new(pool.clone());
    let launcher = ScriptedLauncher::new(vec![LaunchOutcome::Failed("fga".into())]);
    let t = target(&tenant, wf, "trg-1");
    sched
        .fire_event_trigger(&t, 7, &json!({}), None, &launcher)
        .await
        .unwrap();
    let mut gave_up = Vec::new();
    for _ in 0..workflow_engine::scheduler::MAX_LAUNCH_ATTEMPTS {
        make_due(&pool, &tenant).await;
        let r = sched
            .retry_pending_firings(Utc::now(), Some(&tenant), &launcher)
            .await
            .unwrap();
        gave_up.extend(r.gave_up);
    }
    let (outcome, attempts, run) = firing(&pool, &tenant, "trg-1", 7).await;
    assert_eq!(outcome, "gave_up");
    assert_eq!(
        attempts,
        workflow_engine::scheduler::MAX_LAUNCH_ATTEMPTS,
        "上限の回数だけ試す"
    );
    assert_eq!(run, None);
    assert_eq!(gave_up.len(), 1, "打ち切りは 1 回だけ報告する");
    assert_eq!(gave_up[0].event_id, 7);
    assert_eq!(gave_up[0].workflow_id, wf);
}

/// 委譲の不成立・多重度あふれの見送りは再試行しない。無効化したトリガの pending は起こさず見送る。
#[tokio::test]
async fn skipped_launches_and_disabled_triggers_are_not_retried() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    register(&pool, &tenant, wf, "trg-1").await;
    let sched = SchedulerStore::new(pool.clone());
    let skip = ScriptedLauncher::new(vec![LaunchOutcome::Skipped(SkipReason::DelegationInvalid(
        "失効".into(),
    ))]);
    let t = target(&tenant, wf, "trg-1");
    let out = sched
        .fire_event_trigger(&t, 1, &json!({}), None, &skip)
        .await
        .unwrap();
    assert_eq!(out, FireOutcome::Skipped);
    assert_eq!(firing(&pool, &tenant, "trg-1", 1).await.0, "skipped");

    // 一時失敗で pending になった後にトリガを無効化すると、再試行では起こさない。
    let fail = ScriptedLauncher::new(vec![LaunchOutcome::Failed("db".into())]);
    sched
        .fire_event_trigger(&t, 2, &json!({}), None, &fail)
        .await
        .unwrap();
    sqlx::query("UPDATE workflow_trigger SET enabled = false WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
    make_due(&pool, &tenant).await;
    let started = ScriptedLauncher::new(vec![LaunchOutcome::Started(Uuid::new_v4())]);
    let r = sched
        .retry_pending_firings(Utc::now(), Some(&tenant), &started)
        .await
        .unwrap();
    assert_eq!(r.fired, 0);
    assert!(started.calls().is_empty(), "無効化したトリガは起こさない");
    assert_eq!(firing(&pool, &tenant, "trg-1", 2).await.0, "skipped");
}

/// 自分の書込で自分を起こさない・A→B→A の循環・深さの上限。起動した run は鎖を引き継ぐ。
#[tokio::test]
async fn causal_chain_suppresses_self_cycles_and_depth() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    register(&pool, &tenant, a, "trg-a").await;
    register(&pool, &tenant, b, "trg-b").await;
    let sched = SchedulerStore::new(pool.clone());

    // A の run の書込で A は起きない。
    let run_a = cause_run(&pool, &tenant, a, &[]).await;
    let never = ScriptedLauncher::new(vec![LaunchOutcome::Started(Uuid::new_v4())]);
    let out = sched
        .fire_event_trigger(
            &target(&tenant, a, "trg-a"),
            10,
            &json!({}),
            Some(run_a),
            &never,
        )
        .await
        .unwrap();
    assert_eq!(out, FireOutcome::SuppressedCycle);
    assert!(never.calls().is_empty());
    assert_eq!(
        firing(&pool, &tenant, "trg-a", 10).await.0,
        "suppressed_cycle"
    );

    // A の run の書込で B は起き、B の run は鎖 [A] を持つ。
    // 起動された run を launcher の外で作っておく（本物の launcher が作る行の代わり）。
    let started_b = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_run (tenant_id, run_id, org, workflow_id, version, trigger_kind, principal, status) \
         VALUES ($1, $2, 'acme', $3, 1, 'event', $3::text, 'running')",
    )
    .bind(&tenant)
    .bind(started_b)
    .bind(b)
    .execute(&pool)
    .await
    .unwrap();
    let launcher_b = ScriptedLauncher::new(vec![LaunchOutcome::Started(started_b)]);
    let out = sched
        .fire_event_trigger(
            &target(&tenant, b, "trg-b"),
            11,
            &json!({}),
            Some(run_a),
            &launcher_b,
        )
        .await
        .unwrap();
    assert_eq!(out, FireOutcome::Fired(started_b));
    let (chain, cause): (Vec<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT trigger_chain, cause_run_id FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(started_b)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(chain, vec![a]);
    assert_eq!(cause, Some(run_a));

    // B の run（鎖 [A]）の書込で A は起きない（A→B→A の循環）。
    let out = sched
        .fire_event_trigger(
            &target(&tenant, a, "trg-a"),
            12,
            &json!({}),
            Some(started_b),
            &never,
        )
        .await
        .unwrap();
    assert_eq!(out, FireOutcome::SuppressedCycle);

    // 鎖が上限を超える連鎖は起動しない。
    let deep: Vec<Uuid> = (0..workflow_engine::scheduler::MAX_TRIGGER_CHAIN)
        .map(|_| Uuid::new_v4())
        .collect();
    let run_deep = cause_run(&pool, &tenant, Uuid::new_v4(), &deep).await;
    let out = sched
        .fire_event_trigger(
            &target(&tenant, b, "trg-b"),
            13,
            &json!({}),
            Some(run_deep),
            &never,
        )
        .await
        .unwrap();
    assert_eq!(out, FireOutcome::SuppressedDepth);
    assert!(never.calls().is_empty());
}

/// queued の run が上限に達したワークフローの発火は捨てる。
#[tokio::test]
async fn queue_cap_drops_new_firings() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    register(&pool, &tenant, wf, "trg-1").await;
    sqlx::query(
        "INSERT INTO workflow_run (tenant_id, org, workflow_id, version, trigger_kind, principal, status) \
         SELECT $1, 'acme', $2, 1, 'event', $2::text, 'queued' FROM generate_series(1, $3)",
    )
    .bind(&tenant)
    .bind(wf)
    .bind(i32::try_from(workflow_engine::scheduler::MAX_QUEUED_RUNS).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let sched = SchedulerStore::new(pool.clone());
    let launcher = ScriptedLauncher::new(vec![LaunchOutcome::Started(Uuid::new_v4())]);
    let out = sched
        .fire_event_trigger(
            &target(&tenant, wf, "trg-1"),
            5,
            &json!({}),
            None,
            &launcher,
        )
        .await
        .unwrap();
    assert_eq!(out, FireOutcome::DroppedQueueFull);
    assert!(launcher.calls().is_empty());
    assert_eq!(
        firing(&pool, &tenant, "trg-1", 5).await.0,
        "dropped_queue_full"
    );
}

fn wait_ir(max_parallel_runs: u32) -> (Value, RunGraph) {
    let v = json!({
        "ir_version": 1, "name": "wf",
        "policies": { "max_parallel_runs": max_parallel_runs },
        "nodes": [
            { "id": "w", "type": "control.wait",
              "params": { "kind": "event", "source": "storage.write", "scope": {} } }
        ],
        "edges": []
    });
    let ir = WorkflowIr::from_json(&v).unwrap();
    let graph = RunGraph::build(&ir);
    (v, graph)
}

/// 同じイベントで作った run は 1 つだけ（再配送・再試行で create を繰り返しても同じ run を返す）。
#[tokio::test]
async fn same_event_creates_one_run() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let wf = Uuid::new_v4();
    let (ir, graph) = wait_ir(10);
    let origin = RunOrigin {
        trigger_event_id: Some(99),
        parent_run_id: None,
    };
    let input = json!({});
    let principal = wf.to_string();
    let create = || {
        runs.create_run_from(
            &tenant,
            "acme",
            wf,
            1,
            "event",
            Some("trg-1"),
            &principal,
            "workflow",
            &input,
            &ir,
            &graph,
            origin,
        )
    };
    let first = create().await.unwrap().unwrap();
    let second = create().await.unwrap().unwrap();
    assert_eq!(first, second);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_run WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // 親の run から作った子は、親の鎖 ∪ {親のワークフロー} を持つ。
    let child = runs
        .create_run_from(
            &tenant,
            "acme",
            Uuid::new_v4(),
            1,
            "interactive",
            None,
            "alice",
            "user",
            &json!({}),
            &ir,
            &graph,
            RunOrigin {
                trigger_event_id: None,
                parent_run_id: Some(first),
            },
        )
        .await
        .unwrap()
        .unwrap();
    let (chain, cause): (Vec<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT trigger_chain, cause_run_id FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(chain, vec![wf]);
    assert_eq!(cause, Some(first));
}
