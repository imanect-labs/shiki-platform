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
//!   テストで確かめる。包んだ行が戻り値の上限を超えると script.run は invalid で失敗し、戻り値の大きさから決めた数に
//!   行を分けて包む script.run を置く書き方を言う。上限の 2 倍を超える戻り値でも、言われた数に分けると最後まで流れる
//!   （BUG-264）。
//! - 集計の行（GAP-132）: data.query の groups（data の `AggregateGroup` をそのまま出す）を map で回し、領域の中で
//!   `/item/key/<まとめた列>`・`/item/values/<i>`（`/item/value`）を読んで 1 行ずつ書く。`/item/<列>`・`/item/count` は
//!   解決できない。結合先の列でまとめた行の key の名前は group_by に書いたとおりの `m.<参照名>` で、`/item/key/m.<参照名>`
//!   で読む（`/item/key/<参照名>` は解決できない）。
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
/// 売上（地域ごとに集計する）と、集計の行を書く週次 KPI のテーブル（GAP-132）。
const SALES: &str = "5b0e2f3a-7c41-4d8e-9f60-1a2b3c4d5e6f";
const KPI: &str = "6c1f3a4b-8d52-4e9f-a071-2b3c4d5e6f70";
/// 売上の customer が指す顧客のテーブル（顧客の地域でまとめる・結合先の列の key の名前・GAP-132）。
const CUSTOMERS: &str = "7d2a4b5c-9e63-4fa0-b182-3c4d5e6f7081";
const FILE: &str = "3f1d2c4b-5a69-4e7f-8a1b-2c3d4e5f6a7b";

/// 説明文の script.run の例（chat の guide.rs と同じ文・行を 1 行ずつ `{data: 行}` に包む）。
const WRAP_ROWS: &str = "function main(i) { return { rows: i.items.map((r) => ({ data: r })) }; }";

/// CSV の列の名前のままの SQL と、列の名前を参照名にそろえた SQL。
const SQL_AS_IS: &str = "SELECT 品番, 品名, 在庫 FROM data";
const SQL_ALIASED: &str = "SELECT 品番 AS sku, 品名 AS name, 在庫 AS stock FROM data";

/// 包んだ行が script.run の戻り値の上限を超える CSV（[`MANY_ROWS`] 行・1 行 100 バイトほど・BUG-264）。
const SQL_MANY: &str = "SELECT 品番 AS sku, 品名 AS name, 在庫 AS stock FROM data WHERE 在庫 >= 0";
const MANY_ROWS: usize = 3_000;
/// 包んだ行が戻り値の上限の 2 倍を超える CSV（[`MORE_ROWS`] 行・2 つに分けても収まらない・BUG-264）。
const SQL_MORE: &str = "SELECT 品番 AS sku, 品名 AS name, 在庫 AS stock FROM data WHERE 在庫 >= 1";
const MORE_ROWS: usize = 6_000;

/// 説明文と誤りの文言が勧める分け方（行を `parts` つに分けて包む script.run の `k` 個目・0 から）。
fn wrap_part(k: usize, parts: usize) -> String {
    format!(
        "function main(i) {{ const n = Math.ceil(i.items.length / {parts}); \
         return {{ rows: i.items.slice({k} * n, ({k} + 1) * n).map((r) => ({{ data: r }})) }}; }}"
    )
}

/// csv.query の結果（tabular の形・rows は文字、items は種類のまま）。SQL で列の名前をそろえたら items のキーも変わる。
fn csv_result(sql: &str) -> Value {
    let many = match sql {
        SQL_MANY => Some(MANY_ROWS),
        SQL_MORE => Some(MORE_ROWS),
        _ => None,
    };
    if let Some(n) = many {
        let items: Vec<Value> = (0..n)
            .map(|i| json!({ "sku": format!("A-{i:04}"), "name": "x".repeat(60), "stock": i }))
            .collect();
        return json!({
            "columns": ["sku", "name", "stock"], "items": items,
            "rows": items.iter().map(|r| json!([r["sku"], r["name"], r["stock"].to_string()])).collect::<Vec<_>>(),
            "column_types": ["VARCHAR", "VARCHAR", "BIGINT"],
            "total_rows": n, "truncated": false, "version": 1
        });
    }
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
    /// 集計のクエリ（data.query の aggregate のあるもの）。
    queries: Mutex<Vec<data::DataQuery>>,
    writes: Mutex<Vec<DataWriteReq>>,
}

/// 地域ごとの件数と金額の合計（data の `run_query` が返す型をそのまま JSON にする・api の data のポートと同じ）。
/// 北は件数が少なくて伏せた行（value が null で、values は null の並び）。key の名前はまとめた列の group_by に書いた
/// 名前（data の `col_key` と同じく、結合先の列は `別名.参照名`）。
fn region_groups(key: &str) -> Value {
    let group = |region: &str, values: Vec<Value>| data::AggregateGroup {
        key: serde_json::Map::from_iter([(key.to_string(), json!(region))]),
        value: values[0].clone(),
        values,
    };
    serde_json::to_value(data::QueryResult {
        items: None,
        groups: Some(vec![
            group("東", vec![json!(5), json!(350_000.0)]),
            group("北", vec![Value::Null, Value::Null]),
        ]),
        next_cursor: None,
        count: None,
        sort_precision: data::SortPrecision::Exact,
        sort_degraded: false,
        shares_truncated: false,
        suppressed: true,
        snapshot_seq: None,
    })
    .unwrap()
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
        q: &data::DataQuery,
    ) -> Result<Value, PortError> {
        if !q.aggregate.is_empty() {
            self.queries.lock().unwrap().push(q.clone());
            return Ok(region_groups(&q.group_by[0]));
        }
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
    // 売上は地域と金額に索引がある（まとめる列・集計の列は保存で照らす）。
    let fields = |kinds: &[(&str, &str)]| {
        kinds
            .iter()
            .map(|(k, t)| ((*k).to_string(), (*t).to_string()))
            .collect()
    };
    let schema_of = |v: Value| {
        let mut schema: data::TableSchema = serde_json::from_value(v).unwrap();
        for (i, f) in schema.fields.iter_mut().enumerate() {
            f.id = 10 + u16::try_from(i).unwrap();
        }
        schema
    };
    let schema = schema_of(json!({ "fields": [
        { "key": "sold_on", "type": "date", "indexed": true },
        { "key": "region", "type": "select", "options": ["東", "西", "北"], "indexed": true },
        { "key": "amount", "type": "number", "indexed": true },
        { "key": "customer", "type": "record_ref", "ref_table": CUSTOMERS }
    ] }));
    let sales = CatalogTable {
        fields: fields(&[
            ("sold_on", "date"),
            ("region", "select"),
            ("amount", "number"),
            ("customer", "record_ref"),
        ]),
        schema: Some(schema),
        ..CatalogTable::default()
    };
    // 顧客は地域に索引がある（結合先の列でまとめる・GAP-132）。
    let customers = CatalogTable {
        fields: fields(&[("area", "select")]),
        schema: Some(schema_of(json!({ "fields": [
            { "key": "area", "type": "select", "options": ["東", "西", "北"], "indexed": true }
        ] }))),
        ..CatalogTable::default()
    };
    let kpi = CatalogTable {
        fields: fields(&[("region", "text"), ("deals", "number"), ("total", "number")]),
        ..CatalogTable::default()
    };
    Catalog {
        tables: Some(BTreeMap::from([
            (TABLE.to_string(), table),
            (SALES.to_string(), sales),
            (KPI.to_string(), kpi),
            (CUSTOMERS.to_string(), customers),
        ])),
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

/// 包んだ行が戻り値の上限を超える（`sql` の行を 1 つの script.run で包む）。
fn bulk_wrapped_many(sql: &str) -> Value {
    workflow(
        json!([
            read_csv(sql),
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

/// 誤りの文言と説明文の分け方: 行を `parts` つに分けて包む script.run を置き、それぞれの結果を別の一括で登録に渡す。
fn bulk_split(sql: &str, parts: usize) -> Value {
    let mut nodes = vec![read_csv(sql)];
    let mut edges = Vec::new();
    for k in 0..parts {
        nodes.push(json!({ "id": format!("wrap_{k}"), "type": "script.run", "label": "行を分けて包む",
                "params": { "source": { "inline": wrap_part(k, parts) },
                            "input": { "items": { "$from": "nodes.read.output", "path": "/items" } } } }));
        nodes.push(json!({ "id": format!("save_{k}"), "type": "data.bulk_upsert", "label": "まとめて登録",
                "params": { "table": TABLE, "match_on": "sku",
                            "rows": { "$from": format!("nodes.wrap_{k}.output"), "path": "/rows" } } }));
        edges.push(json!({ "from": "read", "to": format!("wrap_{k}") }));
        edges.push(json!({ "from": format!("wrap_{k}"), "to": format!("save_{k}") }));
    }
    workflow(Value::Array(nodes), Value::Array(edges))
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

/// 集計の行を地域ごとに 1 行ずつ書く（`write` は領域の中の追加の data・GAP-132）。
fn per_group(data: Value) -> Value {
    per_group_of(
        json!({
            "group_by": ["region"],
            "aggregate": [{ "metric": "count" }, { "metric": "sum", "field": "amount" }]
        }),
        data,
    )
}

/// 集計の行を 1 行ずつ書く（`query` は data.query の query・`data` は領域の中の追加の data）。
fn per_group_of(query: Value, data: Value) -> Value {
    workflow(
        json!([
            { "id": "sum_up", "type": "data.query", "label": "地域ごとに集計",
              "params": { "table": SALES, "query": query } },
            { "id": "per_region", "type": "control.map", "label": "地域ごと",
              "params": { "items": { "$from": "nodes.sum_up.output", "path": "/groups" } } },
            { "id": "save", "type": "data.record.create", "parent": "per_region", "label": "KPI を追加",
              "params": { "table": KPI, "data": data } }
        ]),
        json!([{ "from": "sum_up", "to": "per_region" }]),
    )
}

/// 説明文の指し方: まとめた列は `/item/key/<参照名>`、指標は `/item/values/<aggregate の位置>`（先頭は `/item/value`）。
fn groups_by_key_and_values() -> Value {
    per_group(json!({
        "region": { "$from": "each", "path": "/item/key/region" },
        "deals": { "$from": "each", "path": "/item/values/0" },
        "total": { "$from": "each", "path": "/item/values/1" }
    }))
}

fn groups_first_value() -> Value {
    per_group(json!({
        "region": { "$from": "each", "path": "/item/key/region" },
        "deals": { "$from": "each", "path": "/item/value" }
    }))
}

/// E1 の app08 の誤り: まとめた列と指標を行の直下の名前で読む（保存は通り、実行で解決できない）。
fn groups_by_flat_names() -> Value {
    per_group(json!({
        "region": { "$from": "each", "path": "/item/region" },
        "deals": { "$from": "each", "path": "/item/count" },
        "total": { "$from": "each", "path": "/item/sum_amount" }
    }))
}

/// 顧客の地域（結合先の列）でまとめた行を書く。説明文のとおり、key の名前は group_by に書いた `m.area` のまま読む。
fn groups_by_joined_key(path: &str) -> Value {
    per_group_of(
        json!({
            "joins": [{ "alias": "m", "field": "customer" }],
            "group_by": ["m.area"],
            "aggregate": [{ "metric": "count" }]
        }),
        json!({
            "region": { "$from": "each", "path": path },
            "deals": { "$from": "each", "path": "/item/value" }
        }),
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
        ("分けて包んでから一括", bulk_split(SQL_MANY, 2)),
        ("3 つに分けて包んでから一括", bulk_split(SQL_MORE, 3)),
        (
            "集計の行を key と values で読む",
            groups_by_key_and_values(),
        ),
        ("集計の行の先頭を value で読む", groups_first_value()),
        ("集計の行を直下の名前で読む", groups_by_flat_names()),
        (
            "結合先の列でまとめた行を m.<参照名> で読む",
            groups_by_joined_key("/item/key/m.area"),
        ),
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
    assert_too_large(&err.code, &err.message, 2);
    // 一括の行数の上限は 5,000 行（rows の形と上限は capability_data の単体テスト）。
    assert_eq!(MAX_WORKFLOW_BULK_ROWS, 5_000);
}

/// 戻り値が大きすぎる script.run の誤り: 作成者が直す誤り（invalid）で、大きさ・上限と、行を `parts` つに分けて包む
/// 書き方を言う（BUG-264。以前は internal・英語の「result too large: … > 262144」）。
fn assert_too_large(code: &str, message: &str, parts: usize) {
    assert_eq!(code, "invalid", "{message}");
    let kb: u32 = message
        .strip_prefix("スクリプトの戻り値が大きすぎます（")
        .and_then(|m| m.split_once(" KB。"))
        .and_then(|(kb, _)| kb.parse().ok())
        .unwrap_or_else(|| panic!("{message}"));
    assert!(kb > 256, "{message}");
    // 文の中の source は、そのまま k を書けば動く書き方（wrap_part と同じ）。
    assert!(
        message.ends_with(&format!(
            " KB。{} KB まで）。返す項目を必要なものだけにするか、行を分けて返してください。\
             テーブルに書く行を包んで返しているときは、行を {parts} つに分けて包む script.run を {parts} 個置き\
             （k 個目の source は {} で、k に 0 から {} を書く）、\
             それぞれの結果を別の data.bulk_upsert に渡してください（1 つの data.bulk_upsert には {MAX_WORKFLOW_BULK_ROWS} 行まで）。",
            workflow_engine::nodes::script::MAX_RESULT_BYTES / 1024,
            wrap_part(0, parts).replace("0 * n, (0 + 1) * n", "k * n, (k + 1) * n"),
            parts - 1
        )),
        "{message}"
    );
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
    /// 成功した step の出力（step の位置 → step に残った JSON）。
    outputs: BTreeMap<String, Value>,
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
    let outputs: Vec<(String, sqlx::types::Json<Value>)> = sqlx::query_as(
        "SELECT step_path, output FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND output IS NOT NULL",
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
        outputs: outputs.into_iter().map(|(p, o)| (p, o.0)).collect(),
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

/// 集計の行（data の `AggregateGroup` をそのまま出した groups）を map で回し、`/item/key/<列>`・`/item/values/<i>`・
/// `/item/value` で読むと最後まで流れる。step に残る data.query の出力は data の型の JSON のまま（ノードで詰め替え
/// ない）。行の直下の名前（`/item/region`・`/item/count`）は解決できない（GAP-132）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aggregate_rows_are_read_by_key_and_values_in_a_map() {
    let Some(pool) = setup().await else { return };
    let f = flow(&pool, &groups_by_key_and_values(), false).await;
    assert_eq!(f.status, Some(RunStatus::Succeeded), "{:?}", f.failures);
    let q = f.ports.queries.lock().unwrap()[0].clone();
    assert_eq!(q.group_by, vec!["region".to_string()]);
    assert_eq!(q.aggregate.len(), 2);
    let mut rows: Vec<Value> = f
        .ports
        .writes
        .lock()
        .unwrap()
        .iter()
        .flat_map(written)
        .collect();
    rows.sort_by_key(|r| r["region"].as_str().unwrap().to_string());
    assert_eq!(
        rows,
        vec![
            // 件数は整数、合計は数。伏せた行は null のまま書く。
            json!({ "region": "北", "deals": null, "total": null }),
            json!({ "region": "東", "deals": 5, "total": 350_000.0 }),
        ]
    );
    // step に残る出力は data の型の JSON（key・value・values）。
    let groups = &f.outputs["sum_up"]["groups"];
    assert_eq!(
        groups[0],
        json!({ "key": { "region": "東" }, "value": 5, "values": [5, 350_000.0] })
    );
    assert_eq!(
        groups[1],
        json!({ "key": { "region": "北" }, "value": null, "values": [null, null] })
    );
    assert!(
        f.outputs["sum_up"].get("items").is_none(),
        "集計では items は無い"
    );

    let f = flow(&pool, &groups_first_value(), false).await;
    assert_eq!(f.status, Some(RunStatus::Succeeded), "{:?}", f.failures);
    let mut deals: Vec<Value> = f
        .ports
        .writes
        .lock()
        .unwrap()
        .iter()
        .flat_map(written)
        .map(|r| r["deals"].clone())
        .collect();
    deals.sort_by_key(Value::to_string);
    assert_eq!(deals, vec![json!(5), Value::Null]);

    let f = flow(&pool, &groups_by_flat_names(), false).await;
    assert_eq!(f.status, Some(RunStatus::Failed));
    assert!(
        f.failures
            .iter()
            .any(|(path, code, _)| path.starts_with("per_region[") && code == "expr_resolve_error"),
        "{:?}",
        f.failures
    );
    assert!(f.ports.writes.lock().unwrap().is_empty(), "何も書かない");
}

/// 結合先の列（`m.area`）でまとめた行の key の名前は、group_by に書いたとおりの `m.area`。説明文のとおり
/// `/item/key/m.area` で読むと最後まで流れ、参照名だけの `/item/key/area` は解決できない（GAP-132）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_grouped_by_a_joined_column_are_read_by_the_alias_key() {
    let Some(pool) = setup().await else { return };
    let f = flow(&pool, &groups_by_joined_key("/item/key/m.area"), false).await;
    assert_eq!(f.status, Some(RunStatus::Succeeded), "{:?}", f.failures);
    assert_eq!(
        f.ports.queries.lock().unwrap()[0].group_by,
        vec!["m.area".to_string()]
    );
    assert_eq!(
        f.outputs["sum_up"]["groups"][0]["key"],
        json!({ "m.area": "東" })
    );
    let mut rows: Vec<Value> = f
        .ports
        .writes
        .lock()
        .unwrap()
        .iter()
        .flat_map(written)
        .collect();
    rows.sort_by_key(|r| r["region"].as_str().unwrap().to_string());
    assert_eq!(
        rows,
        vec![
            json!({ "region": "北", "deals": null }),
            json!({ "region": "東", "deals": 5 }),
        ]
    );

    let f = flow(&pool, &groups_by_joined_key("/item/key/area"), false).await;
    assert_eq!(f.status, Some(RunStatus::Failed));
    assert!(
        f.failures
            .iter()
            .any(|(path, code, _)| path.starts_with("per_region[") && code == "expr_resolve_error"),
        "{:?}",
        f.failures
    );
    assert!(f.ports.writes.lock().unwrap().is_empty(), "何も書かない");
}

/// 包んだ行が script.run の戻り値の上限を超えると、包む step が invalid で失敗し、戻り値の大きさから決めた数の分け方を
/// 言って何も書かない。言われた数に行を分けて包む script.run を置くと、最後まで流れて全部の行が書かれる（BUG-264）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrapped_rows_over_the_script_limit_are_split_as_told() {
    let Some(pool) = setup().await else { return };
    // 上限を少し超える 3,000 行は 2 つ、上限の 2 倍を超える 6,000 行は 3 つに分けるよう言い、言われたとおりに分けると
    // 重ならずに全部の行が書かれる（2 つに分けよと同じ文を繰り返さない）。
    for (sql, rows, parts) in [(SQL_MANY, MANY_ROWS, 2), (SQL_MORE, MORE_ROWS, 3)] {
        let f = flow(&pool, &bulk_wrapped_many(sql), true).await;
        assert_eq!(f.status, Some(RunStatus::Failed));
        let [(path, code, message)] = f.failures.as_slice() else {
            panic!("包む step だけが失敗する: {:?}", f.failures);
        };
        assert_eq!(path, "wrap");
        assert_too_large(code, message, parts);
        assert!(f.ports.writes.lock().unwrap().is_empty(), "何も書かない");

        let f = flow(&pool, &bulk_split(sql, parts), true).await;
        assert_eq!(f.status, Some(RunStatus::Succeeded), "{:?}", f.failures);
        let writes = f.ports.writes.lock().unwrap();
        let sizes: Vec<usize> = writes.iter().map(|w| written(w).len()).collect();
        assert_eq!(sizes.len(), parts);
        assert!(
            sizes.iter().all(|n| *n <= MAX_WORKFLOW_BULK_ROWS),
            "{sizes:?}"
        );
        let skus: BTreeSet<String> = writes
            .iter()
            .flat_map(written)
            .map(|r| r["sku"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(skus.len(), rows, "重ならずに全部の行");
    }
}
