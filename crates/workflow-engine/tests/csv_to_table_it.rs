//! CSV の行をテーブルに書く書き方の結合テスト（BUG-255）。
//!
//! emit_workflow の説明文（chat の `workflow_tool/guide.rs`）が教える書き方を、保存の検証と、IR をワーカーで最後まで
//! 流す配管（能力ノードのポートだけを差し替える・制御ノードと script.run は本物）の両方で確かめる。
//!
//! - 勧める書き方: control.map の items に csv.query の items を書き、領域の中の data.record.upsert（か
//!   data.record.create）の data に列ごとの `$from each`（`/item/<CSV の列の名前>`）を書く。SQL で列の名前を参照名に
//!   そろえたときは `/item` をそのまま書ける。
//! - map の上限（[`MAX_MAP_ITEMS`]）を超えるとき: script.run で 1 行ずつ `{data: 行}` に包み、data.bulk_upsert の rows へ。
//! - csv.query の items（平らな行）を rows にそのまま渡すと、実行で包み方を言って断る（説明文の前の誤り）。
//! - 上限: map の要素の数と script.run の戻り値（256 KB）はここで、bulk の行数（5,000 行）は capability_data の単体
//!   テストで確かめる。
//!
//! 保存の検証と上限は DB 不要。ワーカーで流すものは `STORAGE_TEST_DATABASE_URL` が要る。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::needless_pass_by_value
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::ir::params::{MAX_MAP_ITEMS, MAX_WORKFLOW_BULK_ROWS};
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, DataWriteOp, DataWriteReq, ExecCtx, HttpSendReq,
    HttpSendResp, LlmInvokeReq, NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{
    complete_declared_scopes, run_blockers, save_warnings, validate, CapabilityAudit,
    CapabilityNodeExecutor, Catalog, CatalogTable, EffectJournal, NodeContext, NodeExecutor,
    RunStatus, RunStore, WorkerConfig, WorkflowIr, WorkflowWorker,
};

const TABLE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
const FILE: &str = "3f1d2c4b-5a69-4e7f-8a1b-2c3d4e5f6a7b";

/// 説明文の script.run の例（chat の guide.rs と同じ文・行を 1 行ずつ `{data: 行}` に包む）。
const WRAP_ROWS: &str = "function main(i) { return { rows: i.items.map((r) => ({ data: r })) }; }";

/// CSV の列の名前のままの SQL と、列の名前を参照名にそろえた SQL。
const SQL_AS_IS: &str = "SELECT 品番, 品名, 在庫 FROM data";
const SQL_ALIASED: &str = "SELECT 品番 AS sku, 品名 AS name, 在庫 AS stock FROM data";

/// csv.query の結果（tabular の形・rows は文字、items は種類のまま）。SQL で列の名前をそろえたら items のキーも変わる。
fn csv_result(sql: &str) -> Value {
    let columns: [&str; 3] = if sql.contains(" AS sku") {
        ["sku", "name", "stock"]
    } else {
        ["品番", "品名", "在庫"]
    };
    let values = [json!(["A-1", "ボルト", 120]), json!(["B-2", "ナット", 0])];
    let items: Vec<Value> = values
        .iter()
        .map(|v| {
            let row = v.as_array().unwrap();
            Value::Object(
                columns
                    .iter()
                    .zip(row)
                    .map(|(c, v)| ((*c).to_string(), v.clone()))
                    .collect(),
            )
        })
        .collect();
    json!({
        "columns": columns,
        "rows": [["A-1", "ボルト", "120"], ["B-2", "ナット", "0"]],
        "items": items,
        "column_types": ["VARCHAR", "VARCHAR", "BIGINT"],
        "total_rows": 2, "truncated": false, "version": 1
    })
}

/// 書いた行（作成の値・一括の行の data）。
fn written(req: &DataWriteReq) -> Vec<Value> {
    match &req.op {
        DataWriteOp::Create { data } => vec![data.clone()],
        DataWriteOp::BulkUpsert { rows, .. } => rows.iter().map(|r| r.data.clone()).collect(),
        other => panic!("作成か一括の書込が届く: {other:?}"),
    }
}

/// csv.query と data だけを持つポート（テーブルは空・書込を記録する）。
#[derive(Default)]
struct CsvTablePorts {
    sql: Mutex<Vec<String>>,
    writes: Mutex<Vec<DataWriteReq>>,
}

fn unused() -> PortError {
    PortError::unavailable("このテストでは使わない")
}

#[async_trait]
impl NodePorts for CsvTablePorts {
    async fn storage_write(&self, _: &ExecCtx, _: StorageWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn storage_read(&self, _: &ExecCtx, _: Uuid) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn storage_list(
        &self,
        _: &ExecCtx,
        _: workflow_engine::StorageListReq,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn rag_search(&self, _: &ExecCtx, _: &str, _: Option<u32>) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn llm_invoke(&self, _: &ExecCtx, _: LlmInvokeReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn agent_invoke(
        &self,
        _: &ExecCtx,
        _: AgentInvokeReq,
        _: Arc<dyn workflow_engine::AgentToolGateway>,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn http_send(&self, _: &ExecCtx, _: HttpSendReq) -> Result<HttpSendResp, PortError> {
        Err(unused())
    }
    async fn resolve_secret(&self, _: &ExecCtx, _: &str) -> Result<ResolvedSecretView, PortError> {
        Err(unused())
    }
    async fn workflow_start(&self, _: &ExecCtx, _: &str, _: &Value) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn skill_resolve(
        &self,
        _: &ExecCtx,
        _: &str,
        _: &str,
    ) -> Result<workflow_engine::nodes::ResolvedSkillView, PortError> {
        Err(unused())
    }
    async fn csv_query(
        &self,
        _: &ExecCtx,
        req: workflow_engine::CsvQueryReq,
    ) -> Result<Value, PortError> {
        self.sql.lock().unwrap().push(req.sql.clone());
        Ok(csv_result(&req.sql))
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn data_query(
        &self,
        _: &ExecCtx,
        _: Uuid,
        _: &data::DataQuery,
    ) -> Result<Value, PortError> {
        // 同じ値のレコードは無い（追加か更新は追加に、一括の同じレコードを探す列は当たらない）。
        Ok(json!({
            "items": [], "sort_precision": "exact", "sort_degraded": false,
            "shares_truncated": false, "suppressed": false
        }))
    }
    async fn data_write(&self, _: &ExecCtx, req: DataWriteReq) -> Result<Value, PortError> {
        let rows = written(&req);
        let out = match &req.op {
            DataWriteOp::BulkUpsert { .. } => json!({
                "import_id": Uuid::new_v4().to_string(), "status": "done",
                "rows_total": rows.len(), "rows_applied": rows.len(),
                "rows_invalid": 0, "rows_rejected": 0, "errors": []
            }),
            _ => json!({ "id": Uuid::new_v4().to_string(), "rev": 1, "data": rows[0] }),
        };
        self.writes.lock().unwrap().push(req);
        Ok(out)
    }
}

struct NoAudit;
impl CapabilityAudit for NoAudit {
    fn record(&self, _: &str, _: &str, _: bool, _: &Value) {}
}

/// 品番（sku）を重複を許さない列に持つテーブル（保存の照合の材料）。
fn catalog() -> Catalog {
    let table = CatalogTable {
        fields: [("sku", "text"), ("name", "text"), ("stock", "number")]
            .iter()
            .map(|(k, t)| ((*k).to_string(), (*t).to_string()))
            .collect(),
        unique: BTreeSet::from(["sku".to_string()]),
        ..CatalogTable::default()
    };
    Catalog {
        tables: Some(BTreeMap::from([(TABLE.to_string(), table)])),
        ..Catalog::default()
    }
}

/// スコープを補った IR（保存 API と同じ順）。
fn workflow(nodes: Value, edges: Value) -> Value {
    let ir = json!({
        "ir_version": 1, "name": "import-stock", "display_name": "在庫の CSV を取り込む",
        "triggers": [{ "kind": "interactive" }], "nodes": nodes, "edges": edges
    });
    complete_declared_scopes(&ir).0
}

fn read_csv(sql: &str) -> Value {
    json!({ "id": "read", "type": "csv.query", "label": "CSV を読む",
            "params": { "file": FILE, "sql": sql } })
}

/// 勧める書き方: csv.query の items を map で回し、領域の中で 1 行ずつ書く（`write` は領域の中のブロック）。
fn per_row(sql: &str, write: Value) -> Value {
    workflow(
        json!([
            read_csv(sql),
            { "id": "per_row", "type": "control.map", "label": "1 行ずつ",
              "params": { "items": { "$from": "nodes.read.output", "path": "/items" } } },
            write
        ]),
        json!([{ "from": "read", "to": "per_row" }]),
    )
}

/// 列ごとに `/item/<CSV の列の名前>` を書く追加か更新。
fn upsert_by_column() -> Value {
    per_row(
        SQL_AS_IS,
        json!({ "id": "save", "type": "data.record.upsert", "parent": "per_row", "label": "在庫を書く",
                "params": { "table": TABLE, "match_on": "sku", "data": {
                    "sku": { "$from": "each", "path": "/item/品番" },
                    "name": { "$from": "each", "path": "/item/品名" },
                    "stock": { "$from": "each", "path": "/item/在庫" } } } }),
    )
}

/// SQL で列の名前を参照名にそろえ、data に `/item` をそのまま書く追加。
fn create_aliased() -> Value {
    per_row(
        SQL_ALIASED,
        json!({ "id": "save", "type": "data.record.create", "parent": "per_row", "label": "在庫を追加",
                "params": { "table": TABLE, "data": { "$from": "each", "path": "/item" } } }),
    )
}

/// map の上限を超えるとき: script.run で `{data: 行}` に包んでから一括で書く。
fn bulk_wrapped() -> Value {
    workflow(
        json!([
            read_csv(SQL_ALIASED),
            { "id": "wrap", "type": "script.run", "label": "行を包む",
              "params": { "source": { "inline": WRAP_ROWS },
                          "input": { "items": { "$from": "nodes.read.output", "path": "/items" } } } },
            { "id": "save", "type": "data.bulk_upsert", "label": "まとめて登録",
              "params": { "table": TABLE, "match_on": "sku",
                          "rows": { "$from": "nodes.wrap.output", "path": "/rows" } } }
        ]),
        json!([{ "from": "read", "to": "wrap" }, { "from": "wrap", "to": "save" }]),
    )
}

/// 説明文の前の誤り: csv.query の items（平らな行）を rows にそのまま渡す（保存は通り、実行で断られる）。
fn bulk_flat() -> Value {
    workflow(
        json!([
            read_csv(SQL_ALIASED),
            { "id": "save", "type": "data.bulk_upsert", "label": "まとめて登録",
              "params": { "table": TABLE, "match_on": "sku",
                          "rows": { "$from": "nodes.read.output", "path": "/items" } } }
        ]),
        json!([{ "from": "read", "to": "save" }]),
    )
}

/// 説明文の書き方は、保存の検証を通り、警告も実行の前に止める理由も無い。
#[test]
fn the_guided_ways_save_without_errors_or_warnings() {
    for (name, ir) in [
        ("列ごとの追加か更新", upsert_by_column()),
        ("そろえた列の追加", create_aliased()),
        ("包んでから一括", bulk_wrapped()),
        ("平らな行の一括", bulk_flat()),
    ] {
        let parsed = validate(&ir, &catalog()).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert!(
            save_warnings(&parsed, &catalog()).is_empty(),
            "{name}: {:?}",
            save_warnings(&parsed, &catalog())
        );
        assert!(run_blockers(&parsed).is_empty(), "{name}");
    }
}

fn node_ctx(scopes: &[&str]) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        run_started_at: chrono::Utc::now(),
        run_scheduled_at: None,
        step_path: "n1".into(),
        idempotency_key: format!("wf:csv-table:{}", Uuid::new_v4()),
        attempt: 1,
        principal: "alice".into(),
        principal_kind: "user".into(),
        input: json!({}),
        trigger: json!({}),
        node_outputs: Value::Null,
        each: None,
        trace_id: None,
        scope_ceiling: scopes.iter().map(|s| (*s).to_string()).collect(),
        cancel: workflow_engine::run::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
    }
}

fn lazy_journal() -> EffectJournal {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    EffectJournal::new(pool)
}

/// 上限: map は [`MAX_MAP_ITEMS`] 件まで回し、超えると実行で止まる。script.run の戻り値は 256 KB まで（包んだ行が
/// 大きいと script.run が失敗する）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_map_and_script_limits_in_the_guide_hold() {
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let exec = CapabilityNodeExecutor::new(
        Arc::new(CsvTablePorts::default()),
        lazy_journal(),
        Arc::new(NoAudit),
    )
    .with_script_engine(engine, script_runtime::engine::Limits::default());
    let map = |n: usize| json!({ "items": vec![json!({ "sku": "A" }); n] });
    let ok = exec
        .execute("control.map", &map(MAX_MAP_ITEMS), &node_ctx(&[]))
        .await;
    assert!(ok.ok, "{:?}", ok.error);
    let over = exec
        .execute("control.map", &map(MAX_MAP_ITEMS + 1), &node_ctx(&[]))
        .await;
    assert_eq!(over.error.unwrap().code, "fanout_limit_exceeded");

    // 包んだ行が 256 KB に収まれば通り、超えると失敗する（行は 1 つ 100 バイトほど）。
    let rows = |n: usize| {
        json!({ "source": { "inline": WRAP_ROWS }, "input": { "items":
            vec![json!({ "sku": "A-0001", "name": "x".repeat(60), "stock": 1 }); n] } })
    };
    let fits = exec
        .execute("script.run", &rows(1_000), &node_ctx(&[]))
        .await;
    assert!(fits.ok, "{:?}", fits.error);
    assert_eq!(fits.output["rows"].as_array().unwrap().len(), 1_000);
    assert_eq!(fits.output["rows"][0]["data"]["sku"], "A-0001");
    let too_big = exec
        .execute("script.run", &rows(3_000), &node_ctx(&[]))
        .await;
    let err = too_big.error.expect("256 KB を超える戻り値は失敗する");
    assert!(
        err.message.contains(&format!("> {}", 256 * 1024)),
        "{err:?}"
    );
    // 一括の行数の上限は 5,000 行（rows の形と上限は capability_data の単体テスト）。
    assert_eq!(MAX_WORKFLOW_BULK_ROWS, 5_000);
}

async fn setup() -> Option<PgPool> {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
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

/// IR を 1 回、ワーカーで最後まで流す（テナントは毎回新しく作り、最後に run を消す）。`script` は script.run を動かせる
/// ようにする（エンジンの準備に時間がかかるので、使う IR だけ）。
struct Flow {
    status: Option<RunStatus>,
    /// 失敗した step の（step の位置, code, 文言）。
    failures: Vec<(String, String, String)>,
    ports: Arc<CsvTablePorts>,
}

async fn flow(pool: &PgPool, ir: &Value, script: bool) -> Flow {
    let tenant = format!("t-{}", Uuid::new_v4());
    let ports = Arc::new(CsvTablePorts::default());
    let mut exec = CapabilityNodeExecutor::new(
        Arc::clone(&ports) as Arc<dyn NodePorts>,
        EffectJournal::new(pool.clone()),
        Arc::new(NoAudit),
    );
    if script {
        let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
        exec = exec.with_script_engine(engine, script_runtime::engine::Limits::default());
    }
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let parsed = WorkflowIr::from_json(ir).unwrap();
    let run_id = store
        .create_run(
            &tenant,
            "acme",
            Uuid::new_v4(),
            1,
            "interactive",
            None,
            "alice",
            "user",
            &json!({}),
            ir,
            &RunGraph::build(&parsed),
        )
        .await
        .expect("create_run")
        .expect("admitted");
    let worker = WorkflowWorker::new(
        store.clone(),
        Arc::new(exec) as Arc<dyn NodeExecutor>,
        WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant.clone());
    while worker.claim_and_run_once("w1").await.unwrap() {}
    let status = store.run_status(&tenant, run_id).await.unwrap();
    let failures: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT step_path, error->>'code', error->>'message' FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND error IS NOT NULL ORDER BY step_path",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_all(pool)
    .await
    .unwrap();
    for sql in [
        "DELETE FROM effect_journal WHERE tenant_id = $1",
        "DELETE FROM workflow_run WHERE tenant_id = $1",
    ] {
        sqlx::query(sql).bind(&tenant).execute(pool).await.unwrap();
    }
    Flow {
        status,
        failures,
        ports,
    }
}

/// 勧める書き方は最後まで流れ、CSV の 2 行が数を数のまま、列の参照名で 1 行ずつ書かれる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writing_csv_rows_one_by_one_in_a_map_runs() {
    let Some(pool) = setup().await else { return };
    let expected = vec![
        json!({ "sku": "A-1", "name": "ボルト", "stock": 120 }),
        json!({ "sku": "B-2", "name": "ナット", "stock": 0 }),
    ];
    for (name, ir, sql) in [
        ("列ごとの追加か更新", upsert_by_column(), SQL_AS_IS),
        ("そろえた列の追加", create_aliased(), SQL_ALIASED),
    ] {
        let f = flow(&pool, &ir, false).await;
        assert_eq!(
            f.status,
            Some(RunStatus::Succeeded),
            "{name}: {:?}",
            f.failures
        );
        assert_eq!(*f.ports.sql.lock().unwrap(), vec![sql.to_string()]);
        let mut rows: Vec<Value> = f
            .ports
            .writes
            .lock()
            .unwrap()
            .iter()
            .flat_map(written)
            .collect();
        rows.sort_by_key(|r| r["sku"].as_str().unwrap().to_string());
        assert_eq!(rows, expected, "{name}");
        assert!(
            f.ports
                .writes
                .lock()
                .unwrap()
                .iter()
                .all(|w| matches!(w.op, DataWriteOp::Create { .. })),
            "{name}: 1 行ずつの追加になる"
        );
    }
}

/// 包んでから一括で書く書き方は 1 回の一括の書込になり、平らな行をそのまま渡すと、包み方を言って何も書かない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulk_rows_must_be_wrapped_in_data() {
    let Some(pool) = setup().await else { return };
    let f = flow(&pool, &bulk_wrapped(), true).await;
    assert_eq!(f.status, Some(RunStatus::Succeeded), "{:?}", f.failures);
    {
        let writes = f.ports.writes.lock().unwrap();
        assert_eq!(writes.len(), 1, "一括の書込 1 回");
        assert!(matches!(writes[0].op, DataWriteOp::BulkUpsert { .. }));
        assert_eq!(
            written(&writes[0]),
            vec![
                json!({ "sku": "A-1", "name": "ボルト", "stock": 120 }),
                json!({ "sku": "B-2", "name": "ナット", "stock": 0 }),
            ]
        );
    }

    let f = flow(&pool, &bulk_flat(), false).await;
    assert_eq!(f.status, Some(RunStatus::Failed));
    let [(path, code, message)] = f.failures.as_slice() else {
        panic!("一括の書込だけが失敗する: {:?}", f.failures);
    };
    assert_eq!((path.as_str(), code.as_str()), ("save", "invalid"));
    // どのキーを挙げるかは行のキーの並びによる（どれも列の名前）。
    let wrap = |k: &str| {
        format!(
            "data.bulk_upsert: 1 行目に未知のキー '{k}' があります。行は 1 行ずつ {{\"data\": {{列の参照名: 値}}}} の形に包んで渡してください（行に書けるのは id / rev / key / data だけ）。"
        )
    };
    assert!(
        ["sku", "name", "stock"].iter().any(|k| *message == wrap(k)),
        "{message}"
    );
    assert!(f.ports.writes.lock().unwrap().is_empty(), "何も書かない");
}
