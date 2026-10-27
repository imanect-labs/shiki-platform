//! wait ノードの durable 実行の結合テスト（#178 受け入れ条件・実 Postgres）。
//!
//! - wait(duration) がワーカー解放を跨いで wake_at 到来後に継続する（durable）
//! - wait(event) が outbox イベント（`wake_event_waits`）で起床する
//! - wait(event) は gate（実行主体がイベントの対象を読めるか）が認めたときだけ起床する
//! - wait(event) の filter の評価エラー（大小比較の型不一致）では起床しない
//! - wait(event) の timeout が continue（timeout ポート）／fail（run 失敗）で解決する

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::needless_pass_by_value
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::run::{MapFanout, OnItemError, OnTimeout, Suspend};
use workflow_engine::{
    EventWaitGate, EventWaitSubscriber, NodeContext, NodeExecutor, NodeResult, RunStatus, RunStore,
    RunStoreError, StepStatus,
};

/// wait(event) の起床を認める／認めない gate（問われた購読者を記録する）。
struct Gate {
    allow: bool,
    asked: Mutex<Vec<EventWaitSubscriber>>,
}

impl Gate {
    fn new(allow: bool) -> Self {
        Gate {
            allow,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<EventWaitSubscriber> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl EventWaitGate for Gate {
    async fn admits(&self, subscriber: &EventWaitSubscriber) -> Result<bool, RunStoreError> {
        self.asked.lock().unwrap().push(subscriber.clone());
        Ok(self.allow)
    }
}

/// 確かめられない（認可基盤の障害など）gate。
struct BrokenGate;

#[async_trait]
impl EventWaitGate for BrokenGate {
    async fn admits(&self, _subscriber: &EventWaitSubscriber) -> Result<bool, RunStoreError> {
        Err(RunStoreError::Internal("authz unavailable".into()))
    }
}

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
                        suspend_rev: None,
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
        RunStore::new(pool, workflow_engine::DEFAULT_LEASE_SECS),
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    let gate = Gate::new(true);
    let woke = store
        .wake_event_waits(&tenant, "storage.write", None, &payload, &gate)
        .await
        .unwrap();
    assert_eq!(woke, 1, "イベントで起床");
    // gate には購読した run の実行主体が渡る（relay がその主体で閲覧を確かめる）。
    let asked = gate.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0],
        EventWaitSubscriber {
            tenant_id: tenant.clone(),
            org: "acme".into(),
            run_id,
            step_path: "w".into(),
            principal: "alice".into(),
            principal_kind: "user".into(),
        }
    );
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let ir = wait_ir(json!({
        "kind": "event", "source": "storage.write", "scope": { "table": "expense" }
    }));
    let run_id = create_run(&store, &tenant, &ir).await;

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    let gate = Gate::new(true);
    let woke = store
        .wake_event_waits(&tenant, "storage.write", None, &json!({ "doc": 1 }), &gate)
        .await
        .unwrap();
    assert_eq!(woke, 0, "非 folder scope は起床しない（fail-closed）");
    assert!(
        gate.asked().is_empty(),
        "scope に合わない購読は gate に問わない"
    );
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

/// scope の無い（テナント内のどのイベントにも一致する）購読でも、gate が認めなければ起こさず、
/// ペイロードも渡さない。購読は残り、認められるイベントが来れば起きる。確かめられなければ Err を返し
/// （relay は配送済みにせず配り直す）、購読は待機のまま。
#[tokio::test]
async fn wait_event_is_not_woken_unless_the_gate_admits() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let ir = wait_ir(json!({ "kind": "event", "source": "storage.write", "scope": {} }));
    let run_id = create_run(&store, &tenant, &ir).await;
    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
    let (st, tn, db) = (&store, &tenant, &pool);
    let w_status = move || async move {
        st.step_statuses(tn, run_id)
            .await
            .unwrap()
            .into_iter()
            .find(|(p, _)| p == "w")
            .unwrap()
            .1
    };
    let w_output = move || async move {
        sqlx::query_scalar::<_, Option<Value>>(
            "SELECT output FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'w'",
        )
        .bind(tn)
        .bind(run_id)
        .fetch_one(db)
        .await
        .unwrap()
    };
    assert_eq!(w_status().await, StepStatus::WaitingEvent);

    // 実行主体が読めない対象のイベント: 起こさない・出力に何も載らない。
    let secret = json!({ "file_id": "someone-else", "actor": "bob" });
    let deny = Gate::new(false);
    let woke = store
        .wake_event_waits(&tenant, "storage.write", None, &secret, &deny)
        .await
        .unwrap();
    assert_eq!(woke, 0, "gate が認めないので起床しない");
    assert_eq!(
        deny.asked().len(),
        1,
        "scope と filter に合った購読は gate に問う"
    );
    assert_eq!(w_status().await, StepStatus::WaitingEvent);
    assert_eq!(
        w_output().await,
        None,
        "読めない対象のペイロードを出力に載せない"
    );

    // 確かめられない: Err を返し、起こさない。
    let err = store
        .wake_event_waits(&tenant, "storage.write", None, &secret, &BrokenGate)
        .await;
    assert!(err.is_err(), "確かめられなければ Err（relay が配り直す）");
    assert_eq!(w_status().await, StepStatus::WaitingEvent);

    // 購読は残っているので、読める対象のイベントで起きる。
    let mine = json!({ "file_id": "mine", "actor": "alice" });
    let woke = store
        .wake_event_waits(&tenant, "storage.write", None, &mine, &Gate::new(true))
        .await
        .unwrap();
    assert_eq!(woke, 1);
    assert_eq!(w_output().await, Some(mine));
}

/// engine.md §5.6: wait の filter の評価エラー（数値と文字列の大小比較・BUG-14）では起こさない（gate にも
/// 問わない）。型が合うイベントでは起きる。
#[tokio::test]
async fn wait_event_filter_type_error_does_not_wake() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let ir = wait_ir(json!({
        "kind": "event", "source": "storage.write",
        "filter": { "cmp": { "left": { "$from": "event", "path": "/size" }, "op": "gt", "right": "10" } }
    }));
    let run_id = create_run(&store, &tenant, &ir).await;
    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    let gate = Gate::new(true);
    let woke = store
        .wake_event_waits(
            &tenant,
            "storage.write",
            None,
            &json!({ "size": 12 }),
            &gate,
        )
        .await
        .unwrap();
    assert_eq!(woke, 0, "型の合わない filter では起床しない");
    assert!(
        gate.asked().is_empty(),
        "filter に合わない購読は gate に問わない"
    );
    let woke = store
        .wake_event_waits(
            &tenant,
            "storage.write",
            None,
            &json!({ "size": "12" }),
            &gate,
        )
        .await
        .unwrap();
    assert_eq!(woke, 1, "文字列どうしなら比べられる");
    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Running)
    );
}

#[tokio::test]
async fn wait_event_timeout_continue_takes_timeout_port() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    // step の error.code と run の fail_reason に時間切れの理由が残り、画面に返す失敗の理由（GAP-09）も
    // 「時間切れ」の分類になる（BUG-112。理由が無いと「分類できない失敗」になっていた）。
    let (code, retryable, reason): (Option<String>, Option<bool>, Option<String>) = sqlx::query_as(
        "SELECT s.error->>'code', (s.error->>'retryable')::bool, r.fail_reason \
         FROM step_execution s JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id \
         WHERE s.tenant_id = $1 AND s.run_id = $2 AND s.node_id = 'w'",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let alice = authz::AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        "acme".into(),
        tenant.clone(),
    );
    let outcomes = workflow_engine::RunOutcomeReader::new(pool.clone())
        .own_outcomes(&alice, &[run_id], &[])
        .await
        .unwrap();
    for sql in [
        "DELETE FROM wait_subscription WHERE tenant_id = $1",
        "DELETE FROM workflow_run WHERE tenant_id = $1",
    ] {
        sqlx::query(sql).bind(&tenant).execute(&pool).await.unwrap();
    }
    let want = workflow_engine::run::failure::WAIT_TIMEOUT;
    assert_eq!(
        (code.as_deref(), retryable, reason.as_deref()),
        (Some(want), Some(false), Some(want))
    );
    let failure = outcomes[&run_id].failure.as_ref().expect("失敗の理由");
    assert_eq!(failure.category, workflow_engine::FailureCategory::Timeout);
}

/// データ変更の wait は、停止の commit の後に 1 回だけ受付の突合の候補になる（BUG-42）。ファイルの保存の wait は
/// 候補にならず、run が終わった wait は候補から外して突合を済ませた印を付ける。突合を済ませた wait は二度と
/// 候補にならず、停止し直すと（再実行・再開）また 1 回だけ候補になる。
#[tokio::test]
async fn only_data_waits_await_one_reconcile_after_the_suspend() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let table = uuid::Uuid::new_v4().to_string();
    let file_wait = create_run(
        &store,
        &tenant,
        &wait_ir(json!({ "kind": "event", "source": "storage.write" })),
    )
    .await;
    let data_ir = wait_ir(json!({ "kind": "event", "source": "data.record.created",
                                  "scope": { "table": table } }));
    let data_wait = create_run(&store, &tenant, &data_ir).await;
    let ended = create_run(&store, &tenant, &data_ir).await;
    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
    sqlx::query("UPDATE workflow_run SET status = 'failed' WHERE tenant_id = $1 AND run_id = $2")
        .bind(&tenant)
        .bind(ended)
        .execute(&pool)
        .await
        .unwrap();

    let mine = |c: &[workflow_engine::DataWaitCandidate]| -> Vec<uuid::Uuid> {
        c.iter()
            .filter(|c| c.subscriber.tenant_id == tenant)
            .map(|c| c.subscriber.run_id)
            .collect()
    };
    let got = store.data_waits_to_reconcile(10_000).await.unwrap();
    assert_eq!(mine(&got), vec![data_wait], "データ変更の wait だけ");
    let c = got
        .iter()
        .find(|c| c.subscriber.run_id == data_wait)
        .unwrap();
    assert!(
        c.after_event_id().is_some(),
        "テーブル単位は停止時の位置を持つ"
    );
    assert_eq!(c.suspend_rev(), None);
    let reconciled = |run: uuid::Uuid| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT reconciled_at IS NOT NULL FROM wait_subscription \
                 WHERE tenant_id = $1 AND run_id = $2",
            )
            .bind(&tenant)
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert!(
        reconciled(file_wait).await,
        "ファイルの保存の wait は作る時点で印"
    );
    assert!(reconciled(ended).await, "run が終わった wait は外して印");
    assert!(!reconciled(data_wait).await);

    store.mark_data_wait_reconciled(c).await.unwrap();
    let again = store.data_waits_to_reconcile(10_000).await.unwrap();
    assert!(mine(&again).is_empty(), "突合は 1 回だけ");

    // 停止し直すと（再実行・再開の待ち直し）、また 1 回だけ候補になる。
    sqlx::query(
        "UPDATE step_execution SET status = 'ready' \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'w'",
    )
    .bind(&tenant)
    .bind(data_wait)
    .execute(&pool)
    .await
    .unwrap();
    while w.claim_and_run_once("w1").await.unwrap() {}
    let resumed = store.data_waits_to_reconcile(10_000).await.unwrap();
    assert_eq!(mine(&resumed), vec![data_wait]);

    sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}
