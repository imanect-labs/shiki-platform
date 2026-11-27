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
                // 見えた前の結果（`$from nodes.<id>.output` で引けるもの）のノード id と、その要素の番号。
                let seen: Value = ctx
                    .node_outputs
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| (k.clone(), v["each"]["item"].clone()))
                            .collect::<serde_json::Map<_, _>>()
                    })
                    .unwrap_or_default()
                    .into();
                NodeResult::ok(json!({ "node": node_type, "each": ctx.each, "seen": seen }))
            }
        }
    }
}

async fn setup() -> Option<PgPool> {
    let db_url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").ok()?;
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
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

/// 繰り返しの中のブロックには、本体の結果と、自分の要素と、それを囲む外側の要素の結果が見える（D-123）。
/// ほかの要素の結果と、中が終わっていない繰り返しの結果は見えない（保存時検証 V5 の祖先と同じ範囲）。
#[tokio::test]
async fn nested_map_blocks_see_the_body_and_every_enclosing_element() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let ir = json!({
        "ir_version": 1, "name": "nested-map-outputs",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "first", "type": "storage.read", "params": {} },
            { "id": "outer", "type": "control.map", "params": { "items": [1, 2] } },
            { "id": "pre", "type": "storage.read", "parent": "outer", "params": {} },
            { "id": "inner", "type": "control.map", "parent": "outer", "params": { "items": [10, 20] } },
            { "id": "a", "type": "storage.read", "parent": "inner", "params": {} },
            { "id": "leaf", "type": "storage.read", "parent": "inner", "params": {} },
            { "id": "after", "type": "storage.read", "params": {} }
        ],
        "edges": [
            { "from": "first", "to": "outer" },
            { "from": "outer", "to": "after" },
            { "from": "pre", "to": "inner" },
            { "from": "a", "to": "leaf" }
        ]
    });
    let run_id = create_run(&store, &tenant, &ir).await;
    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
    assert_eq!(
        store.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded)
    );

    let output = |path: &'static str| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            let out: Value = sqlx::query_scalar(
                "SELECT output FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
            )
            .bind(&tenant)
            .bind(run_id)
            .bind(path)
            .fetch_one(&pool)
            .await
            .unwrap();
            out
        }
    };
    // 内側の 2 段目の中（outer[1].inner[0]）の leaf: 本体の first、外側の要素 1 の pre、同じ要素の a。
    assert_eq!(
        output("outer[1].inner[0].leaf").await["seen"],
        json!({ "first": null, "pre": 2, "a": 10 })
    );
    // 外側の中の pre: 本体の first だけ（ほかの要素の pre・中が終わっていない outer は見えない）。
    assert_eq!(
        output("outer[0].pre").await["seen"],
        json!({ "first": null })
    );
    // 本体の after: 本体の結果だけ（中のブロックの結果は見えない）。
    let after = output("after").await["seen"].clone();
    assert_eq!(
        after
            .as_object()
            .map(|m| m.keys().cloned().collect::<Vec<_>>()),
        Some(vec!["first".to_string(), "outer".to_string()])
    );

    sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}

/// D-85: 中にブロックの無い繰り返しは保存できる（実行と自動実行のオンの前に止める）。止める前に作られた実行が
/// あっても、要素の分だけ中の最後のブロックを待ち続けず、やり直さない失敗にする（後続は動かさない）。
/// 要素の無い繰り返しは、中が空でもそのまま次へ進む（いままでどおり）。
#[tokio::test]
async fn map_without_inner_blocks_fails_instead_of_waiting() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let ir = |items: Value| {
        json!({
            "ir_version": 1, "name": "empty-map",
            "declared_scopes": ["storage.read"],
            "nodes": [
                { "id": "map", "type": "control.map", "params": { "items": items } },
                { "id": "after", "type": "storage.read", "params": {} }
            ],
            "edges": [{ "from": "map", "to": "after" }]
        })
    };
    let failing = create_run(&store, &tenant, &ir(json!([1, 2]))).await;
    let passing = create_run(&store, &tenant, &ir(json!([]))).await;

    let w = worker(pool.clone(), &tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        store.run_status(&tenant, failing).await.unwrap(),
        Some(RunStatus::Failed),
        "中が空の繰り返しは待ち続けずに失敗する"
    );
    let code: Option<String> = sqlx::query_scalar(
        "SELECT error->>'code' FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND step_path = 'map'",
    )
    .bind(&tenant)
    .bind(failing)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(code.as_deref(), Some("empty_region"));
    let statuses = store.step_statuses(&tenant, failing).await.unwrap();
    assert_eq!(
        statuses.iter().find(|(p, _)| p == "after").map(|(_, s)| *s),
        Some(StepStatus::Cancelled),
        "後続は動かさない"
    );
    assert_eq!(
        store.run_status(&tenant, passing).await.unwrap(),
        Some(RunStatus::Succeeded),
        "要素が無ければ中が空でも次へ進む"
    );

    // 作った実行を片付ける（step・run_event は FK で消える）。
    sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}
