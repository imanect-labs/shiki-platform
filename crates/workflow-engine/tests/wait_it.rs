//! wait ノードの durable 実行の結合テスト（#178 受け入れ条件・実 Postgres）。
//!
//! - wait(duration) がワーカー解放を跨いで wake_at 到来後に継続する（durable）
//! - wait(event) が outbox イベント（`wake_event_waits`）で起床する
//! - wait(event) の timeout が continue（timeout ポート）／fail（run 失敗）で解決する

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::needless_pass_by_value
)]

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::run::{MapFanout, OnItemError, OnTimeout, Suspend};
use workflow_engine::{NodeContext, NodeExecutor, NodeResult, RunStatus, RunStore, StepStatus};

/// wait/map を suspend/fanout 指示に、他ノードを out 成功にする駆動用 executor。
struct WaitMapExecutor;

#[async_trait]
impl NodeExecutor for WaitMapExecutor {
    async fn execute(&self, node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        match node_type {
            "control.wait" => match params["kind"].as_str().unwrap_or("") {
                "duration" => NodeResult::wait(Suspend::Timer {
                    wake_at: Utc::now()
                        + Duration::seconds(params["duration_sec"].as_i64().unwrap_or(0)),
                }),
                "event" => {
                    let timeout_at = params
                        .get("timeout_sec")
                        .and_then(Value::as_i64)
                        .map(|s| Utc::now() + Duration::seconds(s));
                    let on_timeout = if params["on_timeout"] == json!("continue") {
                        OnTimeout::Continue
                    } else {
                        OnTimeout::Fail
                    };
                    NodeResult::wait(Suspend::Event {
                        source: params["source"].as_str().unwrap_or("").to_string(),
                        scope: params.get("scope").cloned().unwrap_or_else(|| json!({})),
                        filter: params.get("filter").cloned(),
                        timeout_at,
                        on_timeout,
                    })
                }
                _ => NodeResult::fail("bad_params", "unknown wait", false),
            },
            "control.map" => NodeResult::map_fanout(MapFanout {
                items: params["items"].as_array().cloned().unwrap_or_default(),
                max_concurrency: 10,
                on_item_error: if params["on_item_error"] == json!("collect") {
                    OnItemError::Collect
                } else {
                    OnItemError::FailMap
                },
            }),
            "control.join" => NodeResult::ok(ctx.input.clone()),
            _ if params.get("fail") == Some(&json!(true)) => {
                NodeResult::fail("boom", "forced failure", false)
            }
            _ => NodeResult::ok(json!({ "node": node_type, "step": ctx.step_path })),
        }
    }
}

async fn setup() -> Option<PgPool> {
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
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

async fn create_run(store: &RunStore, tenant: &str, ir: &Value) -> uuid::Uuid {
    let parsed = workflow_engine::WorkflowIr::from_json(ir).unwrap();
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
            ir,
            &graph,
        )
        .await
        .expect("create_run")
        .expect("admitted")
}

fn worker(pool: PgPool, tenant: &str) -> workflow_engine::WorkflowWorker {
    workflow_engine::WorkflowWorker::new(
        RunStore::new(pool),
        Arc::new(WaitMapExecutor),
        workflow_engine::WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant)
}

/// `w`(wait) → `done`。
fn wait_ir(wait_params: Value) -> Value {
    json!({
        "ir_version": 1, "name": "wait",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "w", "type": "control.wait", "params": wait_params },
            { "id": "done", "type": "storage.read", "params": {} }
        ],
        "edges": [{ "from": "w", "to": "done" }]
    })
}

#[tokio::test]
async fn wait_duration_is_durable_across_worker_release() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = create_run(
        &store,
        &tenant,
        &wait_ir(json!({ "kind": "duration", "duration_sec": 0 })),
    )
    .await;

    // ワーカーを回すと w は waiting_timer になり、ready が尽きて解放される（done は未実行）。
    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    assert_eq!(
        statuses.iter().find(|(p, _)| p == "w").unwrap().1,
        StepStatus::WaitingTimer,
        "w は waiting_timer で待機"
    );
    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Running),
        "run はまだ実行中（durable に待機）"
    );

    // ワーカー無しでスケジューラが wake_at 到来を起床する（＝再起動を跨ぐ durable 継続）。
    let woke = store
        .wake_due_timers(Utc::now() + Duration::seconds(10), Some(&tenant))
        .await
        .unwrap();
    assert_eq!(woke, 1, "1 件起床");
    // 起床で done が ready になるので、改めてワーカーで処理して完走する。
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    for want in ["w", "done"] {
        assert_eq!(
            statuses.iter().find(|(p, _)| p == want).unwrap().1,
            StepStatus::Succeeded,
            "{want} が succeeded"
        );
    }
}

#[tokio::test]
async fn wait_event_is_woken_by_matching_event() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    // folder scope 無しの event 待ち（source 一致で起床）。
    let ir = wait_ir(json!({ "kind": "event", "source": "storage.write" }));
    let run_id = create_run(&store, &tenant, &ir).await;

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
    assert_eq!(
        store
            .step_statuses(&tenant, run_id)
            .await
            .unwrap()
            .iter()
            .find(|(p, _)| p == "w")
            .unwrap()
            .1,
        StepStatus::WaitingEvent
    );

    // イベント到来で起床する（wake_event_waits＝relay の消費経路）。
    let payload = json!({ "doc": 1 });
    let woke = store
        .wake_event_waits(&tenant, "storage.write", None, &payload)
        .await
        .unwrap();
    assert_eq!(woke, 1, "イベントで起床");
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    // wait の出力にイベントペイロードが載る（$from nodes.w.output で参照可能）。
    let outputs = store.step_outputs(&tenant, run_id, "done").await.unwrap();
    let w_out = outputs
        .iter()
        .find(|(id, _)| id == "w")
        .map(|(_, o)| o.clone());
    assert_eq!(w_out, Some(payload));
}

#[tokio::test]
async fn wait_event_with_non_folder_scope_never_wakes() {
    // fail-closed: folder 以外のキーだけを持つ scope（未対応形状）はワイルドカードに縮退せず
    // 一切マッチしない（誤形状の購読が全イベントで起床する事故を防ぐ・Codex P1）。
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let ir = wait_ir(json!({
        "kind": "event", "source": "storage.write", "scope": { "table": "expense" }
    }));
    let run_id = create_run(&store, &tenant, &ir).await;

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    let woke = store
        .wake_event_waits(&tenant, "storage.write", None, &json!({ "doc": 1 }))
        .await
        .unwrap();
    assert_eq!(woke, 0, "非 folder scope は起床しない（fail-closed）");
    assert_eq!(
        store
            .step_statuses(&tenant, run_id)
            .await
            .unwrap()
            .iter()
            .find(|(p, _)| p == "w")
            .unwrap()
            .1,
        StepStatus::WaitingEvent,
        "待機のまま（全購読化しない）"
    );
}

#[tokio::test]
async fn wait_event_timeout_continue_takes_timeout_port() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    // timeout ポートに繋いだ後続を用意する。
    let ir = json!({
        "ir_version": 1, "name": "wto",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "w", "type": "control.wait",
              "params": { "kind": "event", "source": "storage.write", "timeout_sec": 0, "on_timeout": "continue" } },
            { "id": "on_time", "type": "storage.read", "params": {} },
            { "id": "on_out", "type": "storage.read", "params": {} }
        ],
        "edges": [
            { "from": "w", "from_port": "timeout", "to": "on_time" },
            { "from": "w", "to": "on_out" }
        ]
    });
    let run_id = create_run(&store, &tenant, &ir).await;
    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    // timeout_at 到来を回収 → timeout ポート。
    let woke = store
        .expire_due_waits(Utc::now() + Duration::seconds(10), Some(&tenant))
        .await
        .unwrap();
    assert_eq!(woke, 1);
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let st = |p: &str| statuses.iter().find(|(x, _)| x == p).unwrap().1;
    assert_eq!(
        st("on_time"),
        StepStatus::Succeeded,
        "timeout ポートの後続が実行"
    );
    assert_eq!(st("on_out"), StepStatus::Skipped, "out ポートの後続は skip");
}

#[tokio::test]
async fn run_failure_cancels_waiting_step() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    // 2 つの独立エントリ: w(wait) と f(fail)。f を後から走らせ、w が waiting_timer の間に run を失敗させる。
    let ir = json!({
        "ir_version": 1, "name": "wait-cancel",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "w", "type": "control.wait", "params": { "kind": "duration", "duration_sec": 3600 } },
            { "id": "f", "type": "storage.read", "params": { "fail": true } }
        ],
        "edges": []
    });
    let run_id = create_run(&store, &tenant, &ir).await;
    // f を一旦 claim 対象外にして w を先に waiting_timer にする。
    sqlx::query("UPDATE step_execution SET next_retry_at = now() + interval '1 hour' WHERE tenant_id=$1 AND run_id=$2 AND step_path='f'")
        .bind(&tenant).bind(run_id).execute(&pool).await.unwrap();

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
    assert_eq!(
        store
            .step_statuses(&tenant, run_id)
            .await
            .unwrap()
            .iter()
            .find(|(p, _)| p == "w")
            .unwrap()
            .1,
        StepStatus::WaitingTimer,
        "w は waiting_timer で待機中"
    );

    // f を claim 可能にして失敗させる → run 失敗。
    sqlx::query("UPDATE step_execution SET next_retry_at = now() WHERE tenant_id=$1 AND run_id=$2 AND step_path='f'")
        .bind(&tenant).bind(run_id).execute(&pool).await.unwrap();
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Failed)
    );
    // 待機中だった w は cancelled（起床経路で復活しない）。
    assert_eq!(
        store
            .step_statuses(&tenant, run_id)
            .await
            .unwrap()
            .iter()
            .find(|(p, _)| p == "w")
            .unwrap()
            .1,
        StepStatus::Cancelled,
        "run 失敗で waiting_timer step が cancelled になる"
    );
    // 購読も消し込まれ、以後の起床試行が no-op。
    let woke = store
        .wake_due_timers(Utc::now() + Duration::seconds(7200), Some(&tenant))
        .await
        .unwrap();
    assert_eq!(woke, 0, "失敗 run の待機 step は起床しない");
}

#[tokio::test]
async fn wait_event_timeout_fail_fails_run() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let ir = wait_ir(
        json!({ "kind": "event", "source": "storage.write", "timeout_sec": 0, "on_timeout": "fail" }),
    );
    let run_id = create_run(&store, &tenant, &ir).await;
    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    let woke = store
        .expire_due_waits(Utc::now() + Duration::seconds(10), Some(&tenant))
        .await
        .unwrap();
    assert_eq!(woke, 1);
    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Failed),
        "on_timeout=fail は run を失敗させる"
    );
}
