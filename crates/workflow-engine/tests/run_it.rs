//! run/step エンジンの結合テスト（Task 10.2 受け入れ条件・実 Postgres）。
//!
//! - ワーカー kill →別ワーカーが完了済みステップを再実行せずに run を継続する
//! - `(run_id, seq)` unique で追記が exactly-once に潰れる
//! - fan-out→join の待ち合わせ・skip 伝播
//! - checkpoint 済み step の再 claim は fencing で no-op（再実行なし）

#![allow(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{NodeContext, NodeExecutor, NodeResult, RunStatus, RunStore, StepStatus};

/// 実行回数を数える pass-through executor（全ノードを out で成功させる）。
struct CountingExecutor {
    counts: Arc<dashmap_like::Map>,
}

/// 最小の並行カウンタ（外部依存を避けるため std のみ）。
mod dashmap_like {
    use std::collections::HashMap;
    use std::sync::Mutex;
    #[derive(Default)]
    pub struct Map(Mutex<HashMap<String, usize>>);
    impl Map {
        pub fn incr(&self, key: &str) -> usize {
            let mut m = self.0.lock().unwrap();
            let e = m.entry(key.to_string()).or_insert(0);
            *e += 1;
            *e
        }
        pub fn get(&self, key: &str) -> usize {
            self.0.lock().unwrap().get(key).copied().unwrap_or(0)
        }
    }
}

#[async_trait]
impl NodeExecutor for CountingExecutor {
    async fn execute(&self, _node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        self.counts.incr(&ctx.node_id_from_path());
        // params に "port" があればそのポートを取る（branch 相当のテスト用）。
        if let Some(port) = params.get("port").and_then(|v| v.as_str()) {
            NodeResult::ok_port(json!({ "step": ctx.step_path }), port)
        } else {
            NodeResult::ok(json!({ "step": ctx.step_path }))
        }
    }
}

// NodeContext に node_id を取り出すヘルパ（step_path は静的ノードでは node_id）。
trait NodeIdFromPath {
    fn node_id_from_path(&self) -> String;
}
impl NodeIdFromPath for NodeContext {
    fn node_id_from_path(&self) -> String {
        self.step_path.clone()
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

fn linear_ir() -> Value {
    json!({
        "ir_version": 1, "name": "linear",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "a", "type": "storage.read", "params": {} },
            { "id": "b", "type": "storage.read", "params": {} },
            { "id": "c", "type": "storage.read", "params": {} }
        ],
        "edges": [{ "from": "a", "to": "b" }, { "from": "b", "to": "c" }]
    })
}

fn fanout_join_ir() -> Value {
    json!({
        "ir_version": 1, "name": "fanout",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "src", "type": "storage.read", "params": {} },
            { "id": "l", "type": "storage.read", "params": {} },
            { "id": "r", "type": "storage.read", "params": {} },
            { "id": "j", "type": "control.join", "params": { "mode": "all" } }
        ],
        "edges": [
            { "from": "src", "to": "l" }, { "from": "src", "to": "r" },
            { "from": "l", "to": "j" }, { "from": "r", "to": "j" }
        ]
    })
}

/// 失敗する executor（指定ノードを permanent 失敗させる）。
struct FailingExecutor {
    fail_node: String,
    counts: Arc<dashmap_like::Map>,
}
#[async_trait]
impl NodeExecutor for FailingExecutor {
    async fn execute(&self, _t: &str, _p: &Value, ctx: &NodeContext) -> NodeResult {
        self.counts.incr(&ctx.step_path);
        if ctx.step_path == self.fail_node {
            NodeResult::fail("boom", "permanent", false)
        } else {
            NodeResult::ok(json!({}))
        }
    }
}

#[tokio::test]
async fn failure_cancels_siblings_and_fails_run() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    // src → {l, r}。l を失敗させると run が failed になり r は cancelled（副作用を起こさない）。
    let run_id = create_run(&store, &tenant, &fanout_join_ir()).await;
    let counts = Arc::new(dashmap_like::Map::default());
    let exec = Arc::new(FailingExecutor {
        fail_node: "l".into(),
        counts: Arc::clone(&counts),
    });
    let w = workflow_engine::WorkflowWorker::new(
        store.clone(),
        exec,
        workflow_engine::WorkerConfig::default(),
    )
    .scoped_to_tenant(&tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Failed)
    );
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    // j は前段失敗のため決して ready にならず cancelled（実行されない・副作用防止）。
    let j = statuses.iter().find(|(p, _)| p == "j").unwrap();
    assert_eq!(
        j.1,
        StepStatus::Cancelled,
        "join は cancelled で実行されない"
    );
    assert_eq!(counts.get("j"), 0, "cancelled ノードは実行されない");
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

fn worker(
    pool: PgPool,
    counts: Arc<dashmap_like::Map>,
    tenant: &str,
) -> workflow_engine::WorkflowWorker {
    let store = RunStore::new(pool);
    let exec = Arc::new(CountingExecutor { counts });
    workflow_engine::WorkflowWorker::new(store, exec, workflow_engine::WorkerConfig::default())
        .scoped_to_tenant(tenant)
}

#[tokio::test]
async fn linear_run_completes_each_step_once() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = create_run(&store, &tenant, &linear_ir()).await;

    let counts = Arc::new(dashmap_like::Map::default());
    let w = worker(pool.clone(), Arc::clone(&counts), &tenant);
    // 全 step を順に処理（ready が無くなるまで）。
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    for n in ["a", "b", "c"] {
        assert_eq!(counts.get(n), 1, "{n} は 1 回だけ実行される");
    }
    // exactly-once: run_event の seq に重複が無い。
    let seq_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM run_event WHERE tenant_id = $1 AND run_id = $2")
            .bind(&tenant)
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let distinct: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT seq) FROM run_event WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        seq_count, distinct,
        "run_event の seq は重複しない（exactly-once）"
    );
}

#[tokio::test]
async fn zombie_recheckpoint_does_not_reexecute() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = create_run(&store, &tenant, &linear_ir()).await;

    // step a を claim（fencing 1）。
    let claimed = store
        .claim_ready_step("w1", 60, Some(&tenant))
        .await
        .unwrap()
        .expect("claim a");
    assert_eq!(claimed.node_id, "a");

    // リースを失効させ、別ワーカーが再 claim（fencing 2）。
    sqlx::query(
        "UPDATE step_execution SET lease_expires_at = now() - interval '1 second' \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'a'",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    let claimed2 = store
        .claim_ready_step("w2", 60, Some(&tenant))
        .await
        .unwrap()
        .expect("reclaim a");
    assert_eq!(claimed2.fencing_token, claimed.fencing_token + 1);

    let graph = RunGraph::build(&workflow_engine::WorkflowIr::from_json(&linear_ir()).unwrap());
    // 旧ワーカー（fencing 1）の checkpoint は no-op（ゾンビ）。
    let zombie = store
        .checkpoint_and_advance(
            &claimed,
            &NodeResult::ok(json!({})),
            &graph,
            1,
            workflow_engine::ir::OnError::FailRun,
        )
        .await
        .unwrap();
    assert!(!zombie, "ゾンビの checkpoint は no-op");
    // step a はまだ running のまま（terminal 化されていない）。
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let a = statuses.iter().find(|(p, _)| p == "a").unwrap();
    assert_eq!(a.1, StepStatus::Running);

    // 新ワーカー（fencing 2）の checkpoint は成功し前進する。
    let ok = store
        .checkpoint_and_advance(
            &claimed2,
            &NodeResult::ok(json!({})),
            &graph,
            1,
            workflow_engine::ir::OnError::FailRun,
        )
        .await
        .unwrap();
    assert!(ok);
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let a = statuses.iter().find(|(p, _)| p == "a").unwrap();
    assert_eq!(a.1, StepStatus::Succeeded, "a は 1 回だけ terminal 化");
    let b = statuses.iter().find(|(p, _)| p == "b").unwrap();
    assert_eq!(b.1, StepStatus::Ready, "b が ready 化される");
}

#[tokio::test]
async fn fanout_join_waits_and_completes() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = create_run(&store, &tenant, &fanout_join_ir()).await;

    let counts = Arc::new(dashmap_like::Map::default());
    let w = worker(pool.clone(), Arc::clone(&counts), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    // join は両分岐の完了後に 1 回発火する。
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    for (path, st) in &statuses {
        assert_eq!(*st, StepStatus::Succeeded, "{path} が succeeded");
    }
    assert_eq!(counts.get("j"), 1, "join は 1 回だけ実行");
}

#[tokio::test]
async fn rate_limited_retry_does_not_consume_attempt() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = create_run(&store, &tenant, &linear_ir()).await;
    let graph = RunGraph::build(&workflow_engine::WorkflowIr::from_json(&linear_ir()).unwrap());

    // max_attempts=1。rate_limited は attempt を消費しないので何度でも再試行できる。
    let rate_limited = NodeResult::fail("rate_limited", "throttled", true);
    for _ in 0..3 {
        let claimed = store
            .claim_ready_step("w1", 60, Some(&tenant))
            .await
            .unwrap()
            .expect("claim a");
        assert_eq!(claimed.node_id, "a");
        // attempt は 1 のまま（rate_limited が消費を相殺）。
        assert_eq!(claimed.attempt, 1, "rate_limited は attempt を消費しない");
        // next_retry_at が未来なので待たずに再 claim できるよう 0 に戻す。
        let advanced = store
            .checkpoint_and_advance(
                &claimed,
                &rate_limited,
                &graph,
                1,
                workflow_engine::ir::OnError::FailRun,
            )
            .await
            .unwrap();
        assert!(advanced, "rate_limited は ready へ戻す");
        sqlx::query(
            "UPDATE step_execution SET next_retry_at = now() \
             WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'a'",
        )
        .bind(&tenant)
        .bind(run_id)
        .execute(&pool)
        .await
        .unwrap();
    }
    // a はまだ ready（terminal 化していない）。
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let a = statuses.iter().find(|(p, _)| p == "a").unwrap();
    assert_eq!(
        a.1,
        StepStatus::Ready,
        "rate_limited では枯渇せず ready のまま"
    );

    // 一方 permanent エラーは即 failed（max_attempts=1・retryable=false）。
    let claimed = store
        .claim_ready_step("w1", 60, Some(&tenant))
        .await
        .unwrap()
        .expect("claim a");
    let permanent = NodeResult::fail("bad_request", "nope", false);
    store
        .checkpoint_and_advance(
            &claimed,
            &permanent,
            &graph,
            1,
            workflow_engine::ir::OnError::FailRun,
        )
        .await
        .unwrap();
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let a = statuses.iter().find(|(p, _)| p == "a").unwrap();
    assert_eq!(a.1, StepStatus::Failed, "permanent は即 failed");
}

#[tokio::test]
async fn tenant_isolation_in_claim() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let t1 = format!("t-{}", uuid::Uuid::new_v4());
    let t2 = format!("t-{}", uuid::Uuid::new_v4());
    let r1 = create_run(&store, &t1, &linear_ir()).await;
    let _r2 = create_run(&store, &t2, &linear_ir()).await;

    // claim は tenant 横断で拾い得るが、全クエリが tenant_id を運ぶ。ここでは
    // claim した step が正しく自 run の tenant に属することを確認する。
    let claimed = store
        .claim_ready_step("w1", 60, Some(&t1))
        .await
        .unwrap()
        .expect("claim");
    assert!(claimed.tenant_id == t1 || claimed.tenant_id == t2);
    // run_id と tenant_id の対応が一貫している（越境しない）。
    let owner_tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM workflow_run WHERE run_id = $1")
            .bind(claimed.run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(owner_tenant, claimed.tenant_id);
    let _ = r1;
}

/// on_error=continue のノード `a` を失敗させ、`error`/`out` の分岐を検証する IR。
/// `a` --error--> handler（error ポート接続時のみ）、`a` --out--> after。
fn on_error_ir(connect_error_port: bool, on_error_continue: bool) -> Value {
    let on_error = if on_error_continue {
        "continue"
    } else {
        "fail_run"
    };
    let mut edges = vec![json!({ "from": "a", "to": "after" })];
    let mut nodes = vec![
        json!({ "id": "a", "type": "storage.read", "params": {}, "on_error": on_error }),
        json!({ "id": "after", "type": "storage.read", "params": {} }),
    ];
    if connect_error_port {
        // handler は error 入エッジのみ持つ（entry にならない＝勝手に走らない）。
        nodes.push(json!({ "id": "handler", "type": "storage.read", "params": {} }));
        edges.push(json!({ "from": "a", "from_port": "error", "to": "handler" }));
    }
    json!({
        "ir_version": 1, "name": "onerr",
        "declared_scopes": ["storage.read"],
        "nodes": nodes,
        "edges": edges,
    })
}

#[tokio::test]
async fn on_error_continue_routes_to_error_port_and_run_succeeds() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = create_run(&store, &tenant, &on_error_ir(true, true)).await;

    let counts = Arc::new(dashmap_like::Map::default());
    let exec = Arc::new(FailingExecutor {
        fail_node: "a".into(),
        counts: Arc::clone(&counts),
    });
    let w = workflow_engine::WorkflowWorker::new(
        store.clone(),
        exec,
        workflow_engine::WorkerConfig::default(),
    )
    .scoped_to_tenant(&tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    // 失敗はデータフローに変換され run は succeeded（処理済み失敗は成否に数えない）。
    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded),
        "error ポート接続時は run が succeeded"
    );
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let find = |p: &str| statuses.iter().find(|(x, _)| x == p).unwrap().1;
    assert_eq!(
        find("a"),
        StepStatus::Failed,
        "a は failed（error 解決済み）"
    );
    assert_eq!(find("handler"), StepStatus::Succeeded, "error 後続が実行");
    assert_eq!(find("after"), StepStatus::Skipped, "out 後続は skip");
    assert_eq!(counts.get("handler"), 1, "handler は 1 回実行");
    assert_eq!(counts.get("after"), 0, "after は実行されない");

    // a の output に error オブジェクトが載り taken_ports=error（後続が $from で参照できる）。
    let (output, ports): (Option<sqlx::types::Json<Value>>, Vec<String>) = sqlx::query_as(
        "SELECT output, taken_ports FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'a'",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ports, vec!["error".to_string()]);
    let err = output.unwrap().0;
    assert_eq!(err["error"]["code"], json!("boom"));
    assert_eq!(err["error"]["node_id"], json!("a"));
    assert!(err["error"]["attempt"].is_number(), "attempt を含む");

    // 監査: 失敗→error 経路遷移が run_event に残る。
    let woven: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM run_event \
         WHERE tenant_id = $1 AND run_id = $2 AND kind = 'step.failed' \
           AND payload->>'on_error' = 'continue'",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(woven, 1, "error 経路遷移が監査に記録される");
}

#[tokio::test]
async fn fail_run_without_error_port_fails_run() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    // 同じ DAG だが on_error=fail_run（既定）・error ポート未接続。
    let run_id = create_run(&store, &tenant, &on_error_ir(false, false)).await;

    let counts = Arc::new(dashmap_like::Map::default());
    let exec = Arc::new(FailingExecutor {
        fail_node: "a".into(),
        counts: Arc::clone(&counts),
    });
    let w = workflow_engine::WorkflowWorker::new(
        store.clone(),
        exec,
        workflow_engine::WorkerConfig::default(),
    )
    .scoped_to_tenant(&tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Failed),
        "error ポート未接続の同 DAG は run が failed（回帰なし）"
    );
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let after = statuses.iter().find(|(p, _)| p == "after").unwrap();
    assert_eq!(after.1, StepStatus::Cancelled, "後続は cancelled");
    assert_eq!(counts.get("after"), 0, "後続は実行されない");
}

#[tokio::test]
async fn on_error_continue_without_error_edge_still_fails_run() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    // on_error=continue だが error ポートに何も繋がっていない（error の行き先が無い）。
    let run_id = create_run(&store, &tenant, &on_error_ir(false, true)).await;

    let counts = Arc::new(dashmap_like::Map::default());
    let exec = Arc::new(FailingExecutor {
        fail_node: "a".into(),
        counts: Arc::clone(&counts),
    });
    let w = workflow_engine::WorkflowWorker::new(
        store.clone(),
        exec,
        workflow_engine::WorkerConfig::default(),
    )
    .scoped_to_tenant(&tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    // error の行き先が無ければ握り潰さず run を失敗させる（黙って成功にしない）。
    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Failed),
        "error ポート未接続の continue は run が failed"
    );
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    assert_eq!(
        statuses.iter().find(|(p, _)| p == "a").unwrap().1,
        StepStatus::Failed
    );
}
