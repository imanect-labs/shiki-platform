//! キーで追加か更新・足す数・まとめて登録の同じレコードを探す列（GAP-16）の組み立ての検証（DB 不要）。
//!
//! data の入口の代わりに、行の rev と探す列の一意を持つ小さな表（[`Fake`]）を使う。読み・書きの間にほかの書込が
//! 割り込む・同じ値で先に追加される・見えない同じ値の行がある、を差し込みで起こし、読み直しで更新を失わないこと、
//! 2 件にならないこと、何度も書かないことを確かめる。実 Postgres・実 OpenFGA での並行は api の
//! `workflow_data_upsert_it` が確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

use std::sync::Mutex;

use async_trait::async_trait;
use data::{BulkUpsertRow, CondOp, Condition, DataQuery};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::*;
use crate::nodes::ports::{
    AgentInvokeReq, AgentToolGateway, CsvPatchReq, CsvWriteReq, HttpSendReq, HttpSendResp,
    LlmInvokeReq, ResolvedSecretView, ResolvedSkillView, StorageListReq, StorageWriteReq,
};

const TABLE: Uuid = Uuid::from_u128(0x8c8a_6f6e_2ab7_4a44_a815_9a2b_53c4_e9a1);

#[derive(Clone)]
struct Row {
    id: Uuid,
    rev: i64,
    data: Map<String, Value>,
    /// 実行主体に見えない行（行の決まりで隠れている）。
    hidden: bool,
}

/// 書込の直前に 1 回だけ起こすこと（ほかの人の書込の割り込み）。
type Interleave = Box<dyn FnOnce(&mut Vec<Row>) + Send>;

#[derive(Default)]
struct State {
    rows: Vec<Row>,
    /// 重複を許さない列。
    unique: Option<String>,
    /// 書込の直前の割り込み（先頭から 1 つずつ使う）。
    interleave: Vec<Interleave>,
    /// 成功した書込の数・断られた書込の数・変更の無い更新（書き込む人の見え方での読み）の数。
    wrote: usize,
    refused: usize,
    reads: usize,
    /// まとめて登録で受け取った行。
    bulk: Vec<BulkUpsertRow>,
    /// 読む口（data_query・data_get）でだけ見えない列（読む権限を委譲した人に読めない列）。
    reader_masked: Vec<String>,
}

#[derive(Default)]
struct Fake {
    s: Mutex<State>,
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => data::number_key(x) == data::number_key(y),
        _ => a == b,
    }
}

impl Fake {
    fn with_unique(field: &str) -> Fake {
        let f = Fake::default();
        f.s.lock().unwrap().unique = Some(field.into());
        f
    }

    fn insert(&self, data: Value, hidden: bool) -> Uuid {
        let id = Uuid::new_v4();
        self.s.lock().unwrap().rows.push(Row {
            id,
            rev: 1,
            data: data.as_object().unwrap().clone(),
            hidden,
        });
        id
    }

    fn interleave(&self, f: impl FnOnce(&mut Vec<Row>) + Send + 'static) {
        self.s.lock().unwrap().interleave.push(Box::new(f));
    }

    fn rows(&self) -> Vec<Row> {
        self.s.lock().unwrap().rows.clone()
    }

    fn record(r: &Row) -> Value {
        json!({ "id": r.id, "table_id": TABLE, "rev": r.rev, "data": r.data,
                "owner": "alice", "created_at": "2026-11-02T00:00:00Z",
                "updated_at": "2026-11-02T00:00:00Z" })
    }

    /// 読む口が返すレコード（読む人に読めない列を落とす）。
    fn read(s: &State, r: &Row) -> Value {
        let mut v = Fake::record(r);
        let data = v["data"].as_object_mut().unwrap();
        for k in &s.reader_masked {
            data.remove(k);
        }
        v
    }

    fn duplicate(s: &State, data: &Map<String, Value>, except: Option<Uuid>) -> bool {
        let Some(u) = &s.unique else { return false };
        let Some(v) = data.get(u) else { return false };
        s.rows
            .iter()
            .any(|r| Some(r.id) != except && r.data.get(u).is_some_and(|x| same(x, v)))
    }
}

fn unique_violation() -> PortError {
    PortError::new("unique_violation", "「SKU」に同じ値は入れられません", false)
}

#[async_trait]
impl NodePorts for Fake {
    async fn storage_write(&self, _: &ExecCtx, _: StorageWriteReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn storage_read(&self, _: &ExecCtx, _: Uuid) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn storage_list(&self, _: &ExecCtx, _: StorageListReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn rag_search(&self, _: &ExecCtx, _: &str, _: Option<u32>) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn llm_invoke(&self, _: &ExecCtx, _: LlmInvokeReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn agent_invoke(
        &self,
        _: &ExecCtx,
        _: AgentInvokeReq,
        _: std::sync::Arc<dyn AgentToolGateway>,
    ) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn http_send(&self, _: &ExecCtx, _: HttpSendReq) -> Result<HttpSendResp, PortError> {
        unreachable!()
    }
    async fn resolve_secret(&self, _: &ExecCtx, _: &str) -> Result<ResolvedSecretView, PortError> {
        unreachable!()
    }
    async fn workflow_start(&self, _: &ExecCtx, _: &str, _: &Value) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn skill_resolve(
        &self,
        _: &ExecCtx,
        _: &str,
        _: &str,
    ) -> Result<ResolvedSkillView, PortError> {
        unreachable!()
    }
    async fn csv_query(&self, _: &ExecCtx, _: Uuid, _: &str) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        unreachable!()
    }

    async fn data_query(&self, _: &ExecCtx, _: Uuid, q: &DataQuery) -> Result<Value, PortError> {
        let Some(Condition::Cmp(c)) = &q.filter else {
            panic!("探すのは 1 つの比べだけ: {q:?}")
        };
        let wanted: Vec<Value> = match (c.op, &c.value) {
            (CondOp::Eq, Some(v)) => vec![v.clone()],
            (CondOp::In, Some(Value::Array(vs))) => vs.clone(),
            other => panic!("想定しない比べ: {other:?}"),
        };
        let s = self.s.lock().unwrap();
        let limit = q.limit.unwrap_or(50) as usize;
        let items: Vec<Value> = s
            .rows
            .iter()
            .filter(|r| !r.hidden)
            .filter(|r| {
                r.data
                    .get(&c.field)
                    .is_some_and(|x| wanted.iter().any(|w| same(x, w)))
            })
            .take(limit)
            .map(|r| Fake::read(&s, r))
            .collect();
        Ok(json!({ "items": items }))
    }

    async fn data_get(&self, _: &ExecCtx, _: Uuid, id: Uuid) -> Result<Value, PortError> {
        let s = self.s.lock().unwrap();
        s.rows
            .iter()
            .find(|r| r.id == id && !r.hidden)
            .map(|r| Fake::read(&s, r))
            .ok_or_else(|| PortError::new("not_found", "対象が見つかりません", false))
    }

    async fn data_write(&self, _: &ExecCtx, req: DataWriteReq) -> Result<Value, PortError> {
        let mut s = self.s.lock().unwrap();
        if !s.interleave.is_empty() {
            let f = s.interleave.remove(0);
            f(&mut s.rows);
        }
        match req.op {
            DataWriteOp::Create { data } => {
                let data = data.as_object().unwrap().clone();
                if Fake::duplicate(&s, &data, None) {
                    s.refused += 1;
                    return Err(unique_violation());
                }
                let row = Row {
                    id: Uuid::new_v4(),
                    rev: 1,
                    data,
                    hidden: false,
                };
                let out = Fake::record(&row);
                s.rows.push(row);
                s.wrote += 1;
                Ok(out)
            }
            DataWriteOp::Update {
                record_id,
                patch,
                expected_rev,
            } => {
                let Some(i) = s.rows.iter().position(|r| r.id == record_id && !r.hidden) else {
                    s.refused += 1;
                    return Err(PortError::new("not_found", "対象が見つかりません", false));
                };
                let rev = s.rows[i].rev;
                if expected_rev.is_some_and(|e| e != rev) {
                    s.refused += 1;
                    return Err(PortError::new(
                        "conflict",
                        format!("rev が一致しません（現在 {rev}）"),
                        false,
                    ));
                }
                let mut merged = s.rows[i].data.clone();
                for (k, v) in patch.as_object().unwrap() {
                    merged.insert(k.clone(), v.clone());
                }
                // 変更の無い更新は rev を進めず、いまのレコードを返す（data の update_record と同じ）。
                if merged == s.rows[i].data {
                    s.reads += 1;
                    return Ok(Fake::record(&s.rows[i]));
                }
                if Fake::duplicate(&s, &merged, Some(record_id)) {
                    s.refused += 1;
                    return Err(unique_violation());
                }
                let row = &mut s.rows[i];
                row.data = merged;
                row.rev += 1;
                let out = Fake::record(row);
                s.wrote += 1;
                Ok(out)
            }
            DataWriteOp::BulkUpsert { rows, .. } => {
                s.bulk = rows;
                s.wrote += 1;
                Ok(json!({ "status": "done", "rows_total": s.bulk.len() }))
            }
            other => panic!("想定しない書込: {other:?}"),
        }
    }
}

fn ec() -> ExecCtx {
    ExecCtx {
        tenant_id: "t1".into(),
        org: "acme".into(),
        principal: "alice".into(),
        principal_kind: "user".into(),
        trace_id: None,
        run_id: None,
        step_path: None,
    }
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn upsert_req(data: Value, add: Value) -> UpsertReq {
    UpsertReq {
        table_id: TABLE,
        match_on: "sku".into(),
        data: obj(data),
        add: obj(add),
        idempotency_key: "wf:t1:r:n1".into(),
    }
}

/// 無ければ追加（足す数は 0 から）、あれば書き換え（いまの値に足す・探す列は書かない）。
#[tokio::test]
async fn upsert_creates_then_updates_the_same_record() {
    let f = Fake::with_unique("sku");
    let req = upsert_req(
        json!({ "sku": "A-1", "name": "ねじ" }),
        json!({ "stock": 5 }),
    );
    let first = upsert(&f, &ec(), &req).await.unwrap();
    assert_eq!(first["created"], true);
    assert_eq!(first["data"]["stock"], 5, "追加は 0 に足す");
    let second = upsert(
        &f,
        &ec(),
        &upsert_req(
            json!({ "sku": "A-1", "name": "六角ねじ" }),
            json!({ "stock": -2 }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(second["created"], false);
    assert_eq!(second["id"], first["id"]);
    assert_eq!(second["data"]["stock"], 3);
    assert_eq!(second["data"]["name"], "六角ねじ");
    assert_eq!(f.rows().len(), 1, "同じ値は 1 件のまま");
    assert_eq!(f.s.lock().unwrap().wrote, 2);
}

/// 足す数は 10 進で正確に足す（0.1 + 0.2 は 0.3）。
#[tokio::test]
async fn add_is_exact_decimal() {
    let f = Fake::with_unique("sku");
    let id = f.insert(json!({ "sku": "A", "total": 0.1 }), false);
    let out = update_adding(
        &f,
        &ec(),
        TABLE,
        id,
        Map::new(),
        &obj(json!({ "total": 0.2 })),
        None,
        "k",
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["total"], json!(0.3));
}

/// 読み・書きの間にほかの人が足しても、読み直して足し直すので、どちらの足し分も残る（更新を失わない）。修正前の
/// 組み立て（読んだ値に足して rev を照らさずに書く）だと、割り込んだ 10 が消えて 101 になる。
#[tokio::test]
async fn add_does_not_lose_a_concurrent_update() {
    let f = Fake::with_unique("sku");
    let id = f.insert(json!({ "sku": "A", "stock": 100 }), false);
    // いまの値を読んだ後・1 回目の書込の直前に、ほかの人が 10 足す（rev が進む）。
    f.interleave(|_| {});
    f.interleave(move |rows| {
        let r = rows.iter_mut().find(|r| r.id == id).unwrap();
        r.data.insert("stock".into(), json!(110));
        r.rev += 1;
    });
    let out = update_adding(
        &f,
        &ec(),
        TABLE,
        id,
        Map::new(),
        &obj(json!({ "stock": 1 })),
        None,
        "k",
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["stock"], 111);
    let s = f.s.lock().unwrap();
    assert_eq!(
        (s.wrote, s.refused),
        (1, 1),
        "断られた 1 回の後に 1 回だけ書く"
    );
}

/// いまの値は書き込む人の見え方で読む。読む口（読む権限を委譲した人の見え方）に読めない列でも、書き込む人に読める
/// なら、その値に足す（読む口で読むと、読めない列を 0 から足して上書きしてしまう）。
#[tokio::test]
async fn add_reads_the_current_value_as_the_writer() {
    let f = Fake::with_unique("sku");
    let id = f.insert(json!({ "sku": "A", "stock": 40 }), false);
    f.s.lock().unwrap().reader_masked = vec!["stock".into()];
    let out = update_adding(
        &f,
        &ec(),
        TABLE,
        id,
        Map::new(),
        &obj(json!({ "stock": 2 })),
        None,
        "k",
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["stock"], 42);
    let out = upsert(
        &f,
        &ec(),
        &upsert_req(json!({ "sku": "A" }), json!({ "stock": 1 })),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["stock"], 43);
    assert_eq!(
        f.s.lock().unwrap().reads,
        2,
        "どちらも書き込む人の見え方で読み直す"
    );
}

/// 版を書いたときは読み直さない（その版でなければ断られたまま返す）。
#[tokio::test]
async fn add_with_expected_rev_does_not_retry() {
    let f = Fake::with_unique("sku");
    let id = f.insert(json!({ "sku": "A", "stock": 100 }), false);
    let e = update_adding(
        &f,
        &ec(),
        TABLE,
        id,
        Map::new(),
        &obj(json!({ "stock": 1 })),
        Some(7),
        "k",
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "conflict");
    assert_eq!(f.s.lock().unwrap().refused, 1);
}

/// 重なり続けたら、上限の回数で書かずに返す（やり直せる誤り）。
#[tokio::test]
async fn add_gives_up_after_the_attempt_limit() {
    let f = Fake::with_unique("sku");
    let id = f.insert(json!({ "sku": "A", "stock": 1 }), false);
    // 読むたびと書くたびの直前に、ほかの人が書き換える。
    for _ in 0..2 * MAX_ATTEMPTS {
        f.interleave(move |rows| rows.iter_mut().find(|r| r.id == id).unwrap().rev += 1);
    }
    let e = update_adding(
        &f,
        &ec(),
        TABLE,
        id,
        Map::new(),
        &obj(json!({ "stock": 1 })),
        None,
        "k",
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "conflict");
    assert!(e.retryable);
    let s = f.s.lock().unwrap();
    assert_eq!((s.wrote, s.refused), (0, MAX_ATTEMPTS as usize));
}

/// 同じ値で先に追加された（探した後・追加の前に割り込まれた）ら、2 件にせず、読み直して書き換えに回る。
#[tokio::test]
async fn upsert_turns_a_concurrent_create_into_an_update() {
    let f = Fake::with_unique("sku");
    f.interleave(|rows| {
        rows.push(Row {
            id: Uuid::new_v4(),
            rev: 1,
            data: obj(json!({ "sku": "A", "stock": 10 })),
            hidden: false,
        });
    });
    let out = upsert(
        &f,
        &ec(),
        &upsert_req(json!({ "sku": "A" }), json!({ "stock": 3 })),
    )
    .await
    .unwrap();
    assert_eq!(out["created"], false);
    assert_eq!(out["data"]["stock"], 13, "先に追加された 10 に足す");
    assert_eq!(f.rows().len(), 1);
}

/// 見えない同じ値の行があると、1 件の追加と同じく一意の違反を返す（探し続けない・明かすことは増えない）。
#[tokio::test]
async fn upsert_returns_the_unique_violation_for_an_invisible_row() {
    let f = Fake::with_unique("sku");
    f.insert(json!({ "sku": "A", "stock": 10 }), true);
    let e = upsert(&f, &ec(), &upsert_req(json!({ "sku": "A" }), json!({})))
        .await
        .unwrap_err();
    assert_eq!(e.code, "unique_violation");
    let s = f.s.lock().unwrap();
    assert_eq!((s.wrote, s.refused), (0, 1), "追加を 1 回だけ試す");
}

/// 書き換える前に行が消えたら、探し直して追加に回る。
#[tokio::test]
async fn upsert_recreates_when_the_found_record_vanishes() {
    let f = Fake::with_unique("sku");
    let id = f.insert(json!({ "sku": "A", "stock": 10 }), false);
    f.interleave(move |rows| rows.retain(|r| r.id != id));
    let out = upsert(
        &f,
        &ec(),
        &upsert_req(json!({ "sku": "A" }), json!({ "stock": 1 })),
    )
    .await
    .unwrap();
    assert_eq!(out["created"], true);
    assert_eq!(out["data"]["stock"], 1);
}

/// 探す列が重複を許していないと 2 件以上当たることがある。どれかを選んで書かずに断る。
#[tokio::test]
async fn upsert_refuses_ambiguous_matches() {
    let f = Fake::default();
    f.insert(json!({ "sku": "A" }), false);
    f.insert(json!({ "sku": "A" }), false);
    let e = upsert(&f, &ec(), &upsert_req(json!({ "sku": "A" }), json!({})))
        .await
        .unwrap_err();
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("2 件以上"), "{}", e.message);
    assert_eq!(f.s.lock().unwrap().wrote, 0);
}

/// 入れる値に探す列の値が無い・空・文字か数でないなら書かない。足す数と入れる値・探す列は重ねられない。
#[tokio::test]
async fn upsert_checks_the_key_and_add() {
    let f = Fake::with_unique("sku");
    for (data, add) in [
        (json!({ "name": "x" }), json!({})),
        (json!({ "sku": " " }), json!({})),
        (json!({ "sku": ["A"] }), json!({})),
        (json!({ "sku": "A", "stock": 1 }), json!({ "stock": 1 })),
        (json!({ "sku": "A" }), json!({ "sku": 1 })),
    ] {
        let e = upsert(&f, &ec(), &upsert_req(data.clone(), add))
            .await
            .unwrap_err();
        assert_eq!(e.code, "invalid", "{data}");
    }
    // いまの値が数でない列には足せない。
    f.insert(json!({ "sku": "B", "stock": "多い" }), false);
    let e = upsert(
        &f,
        &ec(),
        &upsert_req(json!({ "sku": "B" }), json!({ "stock": 1 })),
    )
    .await
    .unwrap_err();
    assert!(e.message.contains("数ではない"), "{}", e.message);
    assert_eq!(f.s.lock().unwrap().wrote, 0);
    assert!(parse_add(Some(json!({ "stock": "1" }))).is_err());
    assert!(parse_add(Some(json!([1]))).is_err());
    assert!(parse_add(Some(Value::Null)).unwrap().is_empty());
    // 置いたばかりの値の欄（空の文字）は、足す数だけで書くときの空の値。
    assert!(parse_values(json!(""), "変更する値").unwrap().is_empty());
    assert!(parse_values(json!("x"), "変更する値").is_err());
}

fn bulk_row(data: Value) -> BulkUpsertRow {
    BulkUpsertRow {
        key: None,
        record_id: None,
        expected_rev: None,
        data,
    }
}

/// まとめて登録: id の無い行を同じ値のレコードに当てる（数は 1 と 1.0 を同じに見る）。当たらない行は追加のまま。
#[tokio::test]
async fn bulk_rows_are_matched_by_the_key() {
    let f = Fake::with_unique("no");
    let a = f.insert(json!({ "no": "R-1", "nps": 3 }), false);
    let b = f.insert(json!({ "no": 2 }), false);
    let given = Uuid::new_v4();
    let mut rows = vec![
        bulk_row(json!({ "no": "R-1", "nps": 9 })),
        bulk_row(json!({ "no": 2.0 })),
        bulk_row(json!({ "no": "R-3" })),
        BulkUpsertRow {
            record_id: Some(given),
            ..bulk_row(json!({ "no": "R-1" }))
        },
    ];
    let n = match_bulk_rows(&f, &ec(), TABLE, "no", &mut rows)
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(rows[0].record_id, Some(a));
    assert_eq!(rows[1].record_id, Some(b));
    assert_eq!(rows[2].record_id, None, "当たらない行は追加");
    assert_eq!(rows[3].record_id, Some(given), "id を書いた行はそのまま");
}

/// まとめて登録: 探す列の値が無い・同じ値の行が 2 つあるなら、何も登録しない。
#[tokio::test]
async fn bulk_rows_need_distinct_key_values() {
    let f = Fake::with_unique("no");
    let mut missing = vec![bulk_row(json!({ "no": "A" })), bulk_row(json!({ "x": 1 }))];
    let e = match_bulk_rows(&f, &ec(), TABLE, "no", &mut missing)
        .await
        .unwrap_err();
    assert!(e.message.starts_with("2 件目に"), "{}", e.message);
    let mut dup = vec![
        bulk_row(json!({ "no": 1 })),
        bulk_row(json!({ "no": "B" })),
        bulk_row(json!({ "no": 1.0 })),
    ];
    let e = match_bulk_rows(&f, &ec(), TABLE, "no", &mut dup)
        .await
        .unwrap_err();
    assert!(e.message.starts_with("1 件目と 3 件目"), "{}", e.message);
}

/// まとめて登録: 探す値は 200 件ずつ問い合わせる（data の 1 ページの上限を超えて当て漏らさない）。
#[tokio::test]
async fn bulk_rows_are_matched_in_pages() {
    let f = Fake::with_unique("no");
    let ids: Vec<Uuid> = (0..450)
        .map(|i| f.insert(json!({ "no": format!("R-{i}") }), false))
        .collect();
    let mut rows: Vec<BulkUpsertRow> = (0..450)
        .map(|i| bulk_row(json!({ "no": format!("R-{i}") })))
        .collect();
    let n = match_bulk_rows(&f, &ec(), TABLE, "no", &mut rows)
        .await
        .unwrap();
    assert_eq!(n, 450);
    assert!(rows
        .iter()
        .zip(&ids)
        .all(|(r, id)| r.record_id == Some(*id)));
}
