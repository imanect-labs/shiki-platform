//! map ノードの動的 fan-out の結合テスト（#178 受け入れ条件・実 Postgres）。
//!
//! - fan-out → 集約（要素は入力順・各要素の each コンテキストが分離）
//! - on_item_error=fail_map（既定）: 1 要素失敗で map 失敗・他要素は完走
//! - on_item_error=collect: 失敗を errors[] に集約し map 成功
//! - 要素ごと step は独立の冪等キー（PIT-31 の要素分離）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::needless_pass_by_value
)]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::run::{MapFanout, OnItemError};
use workflow_engine::{NodeContext, NodeExecutor, NodeResult, RunStatus, RunStore, StepStatus};

/// map を fanout 指示に。work は `fail_on_index` と一致する要素で失敗、他は each を含む out。
struct MapExecutor;

#[async_trait]
impl NodeExecutor for MapExecutor {
    async fn execute(&self, node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        match node_type {
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
            _ => {
                let idx = ctx.each.as_ref().and_then(|e| e["index"].as_i64());
                if let (Some(fi), Some(i)) =
                    (params.get("fail_on_index").and_then(Value::as_i64), idx)
                {
                    if fi == i {
                        return NodeResult::fail("boom", "element failed", false);
                    }
                }
                NodeResult::ok(json!({ "node": node_type, "each": ctx.each }))
            }
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
}

fn worker(pool: PgPool, tenant: &str) -> workflow_engine::WorkflowWorker {
    workflow_engine::WorkflowWorker::new(
        RunStore::new(pool),
        Arc::new(MapExecutor),
        workflow_engine::WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant)
}

/// `map`(items) → `after`。領域は単一ノード `work`（入口=出口）。
fn map_ir(map_params: Value, work_params: Value) -> Value {
    let mut mp = json!({ "items": [10, 20, 30] });
    mp.as_object_mut()
        .unwrap()
        .extend(map_params.as_object().cloned().unwrap_or_default());
    json!({
        "ir_version": 1, "name": "map",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "map", "type": "control.map", "params": mp },
            { "id": "after", "type": "storage.read", "params": {} },
            { "id": "work", "type": "storage.read", "parent": "map", "params": work_params }
        ],
        "edges": [{ "from": "map", "to": "after" }]
    })
}

#[tokio::test]
async fn map_fans_out_and_aggregates_in_order() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = create_run(&store, &tenant, &map_ir(json!({}), json!({}))).await;

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    // 3 要素 × work が実行され、map の後続 after も実行される。
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    for i in 0..3 {
        assert_eq!(
            statuses
                .iter()
                .find(|(p, _)| *p == format!("map[{i}].work"))
                .unwrap()
                .1,
            StepStatus::Succeeded,
            "要素 {i} が実行される"
        );
    }
    assert_eq!(
        statuses.iter().find(|(p, _)| p == "after").unwrap().1,
        StepStatus::Succeeded
    );

    // map 集約: items は入力順・各要素の each.index が分離している。
    let outputs = store.step_outputs(&tenant, run_id, "after").await.unwrap();
    let map_out = outputs
        .iter()
        .find(|(id, _)| id == "map")
        .map(|(_, o)| o.clone())
        .unwrap();
    let items = map_out["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    for (i, item) in items.iter().enumerate() {
        assert_eq!(item["each"]["index"], json!(i), "要素順が保たれる");
        assert_eq!(item["each"]["item"], json!((i as i64 + 1) * 10));
    }
    assert!(map_out["errors"].as_array().unwrap().is_empty());

    // PIT-31: 要素ごと step は独立の冪等キー（step_path 依存）。
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM step_execution WHERE tenant_id=$1 AND run_id=$2 AND step_path LIKE 'map[%'",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let uniq: std::collections::HashSet<_> = keys.iter().collect();
    assert_eq!(keys.len(), uniq.len(), "要素の冪等キーは重複しない");
}

#[tokio::test]
async fn map_fail_map_fails_run_but_completes_other_items() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    // 既定 fail_map・要素 1 を失敗させる。
    let run_id = create_run(
        &store,
        &tenant,
        &map_ir(json!({}), json!({ "fail_on_index": 1 })),
    )
    .await;

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Failed),
        "fail_map は 1 要素失敗で run を失敗させる"
    );
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let st = |p: String| statuses.iter().find(|(x, _)| *x == p).map(|(_, s)| *s);
    // 他要素（0,2）は完走している（fail_map は他要素を止めない）。
    assert_eq!(st("map[0].work".into()), Some(StepStatus::Succeeded));
    assert_eq!(st("map[2].work".into()), Some(StepStatus::Succeeded));
    assert_eq!(st("map[1].work".into()), Some(StepStatus::Failed));
    assert_eq!(
        st("after".into()),
        Some(StepStatus::Cancelled),
        "map 後続は cancel"
    );
}

#[tokio::test]
async fn nested_map_fail_map_is_contained_in_outer_item() {
    // ネスト map（深さ 2）: 内側 fail_map の失敗は run を落とさず「外側要素の失敗」として
    // 封じ込め、外側 map の集約（collect）に委ねる（要素失敗の封じ込め規則の一貫性・Codex P2）。
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let ir = json!({
        "ir_version": 1, "name": "nested-map",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "outer", "type": "control.map",
              "params": { "items": [1, 2], "on_item_error": "collect" } },
            { "id": "inner", "type": "control.map", "parent": "outer",
              "params": { "items": [10, 20], "on_item_error": "fail_map" } },
            { "id": "work", "type": "storage.read", "parent": "inner",
              "params": { "fail_on_index": 0 } },
            { "id": "after", "type": "storage.read", "params": {} }
        ],
        "edges": [{ "from": "outer", "to": "after" }]
    });
    let run_id = create_run(&store, &tenant, &ir).await;

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    // 内側 map は各外側要素で fail_map 失敗するが、run は落ちず外側 collect が失敗を集約して成功する。
    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded),
        "内側 fail_map が run 全体を失敗させない"
    );
    let statuses = store.step_statuses(&tenant, run_id).await.unwrap();
    let inner0 = statuses
        .iter()
        .find(|(p, _)| p == "outer[0].inner")
        .expect("内側 map step が存在");
    assert_eq!(
        inner0.1,
        StepStatus::Failed,
        "内側 map は失敗として封じ込め"
    );
    assert_eq!(
        statuses.iter().find(|(p, _)| p == "after").unwrap().1,
        StepStatus::Succeeded,
        "外側の後続は前進する"
    );
    // 外側 map の出力に両要素の失敗が集約される。
    let outputs = store.step_outputs(&tenant, run_id, "after").await.unwrap();
    let outer_out = outputs
        .iter()
        .find(|(id, _)| id == "outer")
        .map(|(_, o)| o.clone())
        .expect("outer の出力");
    assert_eq!(
        outer_out["errors"].as_array().map(Vec::len),
        Some(2),
        "外側 collect が要素失敗を 2 件集約: {outer_out}"
    );
}

#[tokio::test]
async fn map_collect_gathers_errors_and_succeeds() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let run_id = create_run(
        &store,
        &tenant,
        &map_ir(
            json!({ "on_item_error": "collect" }),
            json!({ "fail_on_index": 1 }),
        ),
    )
    .await;

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded),
        "collect は失敗を集約して map 成功"
    );
    let outputs = store.step_outputs(&tenant, run_id, "after").await.unwrap();
    let map_out = outputs
        .iter()
        .find(|(id, _)| id == "map")
        .map(|(_, o)| o.clone())
        .unwrap();
    let errors = map_out["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "失敗 1 件が errors に集約");
    assert_eq!(errors[0]["index"], json!(1));
    assert_eq!(
        store
            .step_statuses(&tenant, run_id)
            .await
            .unwrap()
            .iter()
            .find(|(p, _)| p == "after")
            .unwrap()
            .1,
        StepStatus::Succeeded
    );
}
