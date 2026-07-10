//! 実行履歴 read クエリとレイアウト永続化の結合テスト（Task 10.14 backend・実 Postgres）。
//!
//! - list_runs: keyset・status/トリガ種フィルタ・作成日降順
//! - run_detail: workflow_id 束縛（別 workflow の run_id は None = 存在秘匿）・has_output
//! - step_detail: output 本体の遅延取得・workflow_id 束縛
//! - list_events: after_seq・seq 昇順
//! - EditorLayoutStore: 既定 `{}`・upsert・256KB 上限

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
use uuid::Uuid;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{
    EditorLayoutStore, LayoutError, NodeContext, NodeExecutor, NodeResult, RunListFilter, RunStore,
    WorkerConfig, WorkflowWorker,
};

/// pass-through executor（`fail` param のあるノードだけ permanent 失敗）。
struct PassExecutor;

#[async_trait]
impl NodeExecutor for PassExecutor {
    async fn execute(&self, _t: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        if params.get("fail").is_some() {
            NodeResult::fail("boom", "意図的失敗", false)
        } else {
            NodeResult::ok(json!({ "out_of": ctx.step_path }))
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
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

fn two_step_ir(fail_second: bool) -> Value {
    let b_params = if fail_second {
        json!({ "fail": true })
    } else {
        json!({})
    };
    json!({
        "ir_version": 1, "name": "hist",
        "declared_scopes": ["storage.read"],
        "nodes": [
            { "id": "a", "type": "storage.read", "params": {} },
            { "id": "b", "type": "storage.read", "params": b_params }
        ],
        "edges": [{ "from": "a", "to": "b" }]
    })
}

async fn run_to_completion(
    pool: &PgPool,
    tenant: &str,
    workflow_id: Uuid,
    trigger_kind: &str,
    ir: &Value,
) -> Uuid {
    let store = RunStore::new(pool.clone());
    let parsed = workflow_engine::WorkflowIr::from_json(ir).unwrap();
    let graph = RunGraph::build(&parsed);
    let run_id = store
        .create_run(
            tenant,
            "acme",
            workflow_id,
            1,
            trigger_kind,
            None,
            "alice",
            "user",
            &json!({ "who": "alice" }),
            ir,
            &graph,
        )
        .await
        .expect("create_run")
        .expect("admitted");
    let w = WorkflowWorker::new(
        RunStore::new(pool.clone()),
        Arc::new(PassExecutor),
        WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
    run_id
}

#[tokio::test]
async fn history_queries_project_only_needed_fields_with_binding() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let other_wf = Uuid::new_v4();
    let store = RunStore::new(pool.clone());

    let ok_run = run_to_completion(&pool, &tenant, wf, "interactive", &two_step_ir(false)).await;
    let failed_run = run_to_completion(&pool, &tenant, wf, "schedule", &two_step_ir(true)).await;
    let other_run =
        run_to_completion(&pool, &tenant, other_wf, "interactive", &two_step_ir(false)).await;

    // --- list_runs: 全件（作成日降順）→ status フィルタ → トリガ種フィルタ → keyset。
    let all = store
        .list_runs(&tenant, wf, &RunListFilter::default(), None, 50)
        .await
        .unwrap();
    assert_eq!(all.len(), 2, "他 workflow の run は混ざらない");
    assert!(all[0].created_at >= all[1].created_at);

    let failed_only = store
        .list_runs(
            &tenant,
            wf,
            &RunListFilter {
                statuses: vec!["failed".into()],
                ..Default::default()
            },
            None,
            50,
        )
        .await
        .unwrap();
    assert_eq!(failed_only.len(), 1);
    assert_eq!(failed_only[0].run_id, failed_run);

    let schedule_only = store
        .list_runs(
            &tenant,
            wf,
            &RunListFilter {
                trigger_kinds: vec!["schedule".into()],
                ..Default::default()
            },
            None,
            50,
        )
        .await
        .unwrap();
    assert_eq!(schedule_only.len(), 1);

    let page1 = store
        .list_runs(&tenant, wf, &RunListFilter::default(), None, 1)
        .await
        .unwrap();
    let page2 = store
        .list_runs(
            &tenant,
            wf,
            &RunListFilter::default(),
            Some((page1[0].created_at, page1[0].run_id)),
            1,
        )
        .await
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_ne!(page1[0].run_id, page2[0].run_id, "keyset で前進する");

    // --- run_detail: workflow_id 束縛（他 workflow の run_id は None = 404 秘匿）。
    assert!(store
        .run_detail(&tenant, wf, other_run)
        .await
        .unwrap()
        .is_none());
    let d = store
        .run_detail(&tenant, wf, failed_run)
        .await
        .unwrap()
        .expect("detail");
    assert_eq!(d.status, "failed");
    assert_eq!(d.input, json!({ "who": "alice" }));
    let step_a = d.steps.iter().find(|s| s.step_path == "a").unwrap();
    assert!(step_a.has_output, "成功 step は has_output=true");
    let step_b = d.steps.iter().find(|s| s.step_path == "b").unwrap();
    assert_eq!(step_b.status, "failed");
    assert!(
        step_b.error.as_ref().is_some_and(|e| !e.0.is_null()),
        "失敗 step に error 詳細"
    );

    // --- step_detail: output 本体の遅延取得＋束縛。
    let sd = store
        .step_detail(&tenant, wf, ok_run, "a")
        .await
        .unwrap()
        .expect("step detail");
    assert_eq!(sd.output, json!({ "out_of": "a" }));
    assert!(store
        .step_detail(&tenant, other_wf, ok_run, "a")
        .await
        .unwrap()
        .is_none());

    // --- list_events: seq 昇順・after_seq で前進・束縛。
    let events = store
        .list_events(&tenant, wf, ok_run, 0, 100)
        .await
        .unwrap();
    assert!(events.len() >= 3, "run.started + step 系 + run.succeeded");
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
    let rest = store
        .list_events(&tenant, wf, ok_run, events[0].seq, 100)
        .await
        .unwrap();
    assert_eq!(rest.len(), events.len() - 1);
    assert!(store
        .list_events(&tenant, wf, other_run, 0, 100)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn editor_layout_roundtrip_and_limit() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let layouts = EditorLayoutStore::new(pool.clone());

    assert_eq!(
        layouts.get(&tenant, wf).await.unwrap(),
        json!({}),
        "未保存は空オブジェクト"
    );
    let v1 = json!({ "positions": { "a": { "x": 10.0, "y": 20.0 } } });
    layouts.put(&tenant, wf, &v1).await.unwrap();
    assert_eq!(layouts.get(&tenant, wf).await.unwrap(), v1);
    // upsert（上書き）。
    let v2 = json!({ "positions": { "a": { "x": 1.0, "y": 2.0 }, "b": { "x": 3.0, "y": 4.0 } } });
    layouts.put(&tenant, wf, &v2).await.unwrap();
    assert_eq!(layouts.get(&tenant, wf).await.unwrap(), v2);
    // 256KB 上限。
    let big = json!({ "blob": "x".repeat(300 * 1024) });
    assert!(matches!(
        layouts.put(&tenant, wf, &big).await,
        Err(LayoutError::TooLarge)
    ));
}
