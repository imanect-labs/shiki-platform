//! data ノード・`Shiki.data.*`（Task 15.7）の engine 層の検証。
//!
//! 純ロジック（DB 不要）: data の `DataQuery` がそのままポートへ渡ること（比較値の `$from` は値へ置き換える
//! だけ）・scope 天井（data.read / data.write）の一段目と `out_of_scope` の監査・script から
//! `Shiki.data.query` が呼べること・data を結線しないポートは「未構成」で失敗すること。
//! DB 必要（`STORAGE_TEST_DATABASE_URL`）: 書込が effect_journal で高々 1 回（ステップの再実行・
//! script のホスト呼び出しの再実行）。
//! 行の認可（委譲元ユーザーの行述語・マスク）は data 側で効き、api の `workflow_data_it` が実 PG ＋
//! 実 OpenFGA で確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use data::{CondOp, Condition, DataQuery, SortKey};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, DataWriteOp, DataWriteReq, ExecCtx, HttpSendReq,
    HttpSendResp, LlmInvokeReq, NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor,
};

/// data のポートだけを持つ fake（渡された DataQuery・書込を記録する）。
#[derive(Default)]
struct DataPorts {
    /// data を結線しない（既定実装＝未構成）ふりをする。
    unconfigured: bool,
    queries: Mutex<Vec<(Uuid, DataQuery)>>,
    writes: Mutex<Vec<DataWriteReq>>,
    /// 次の書込を data が拒否した（検証エラー）ことにする。
    reject_next_write: Mutex<bool>,
}

#[async_trait]
impl NodePorts for DataPorts {
    async fn storage_write(&self, _c: &ExecCtx, _r: StorageWriteReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn storage_read(&self, _c: &ExecCtx, _f: Uuid) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn storage_list(&self, _c: &ExecCtx, _p: Option<Uuid>) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn rag_search(
        &self,
        _c: &ExecCtx,
        _q: &str,
        _k: Option<u32>,
    ) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn llm_invoke(&self, _c: &ExecCtx, _r: LlmInvokeReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn agent_invoke(&self, _c: &ExecCtx, _r: AgentInvokeReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn http_send(&self, _c: &ExecCtx, _r: HttpSendReq) -> Result<HttpSendResp, PortError> {
        unreachable!()
    }
    async fn resolve_secret(
        &self,
        _c: &ExecCtx,
        _n: &str,
    ) -> Result<ResolvedSecretView, PortError> {
        unreachable!()
    }
    async fn skill_resolve(
        &self,
        _ctx: &ExecCtx,
        _name: &str,
        _version: &str,
    ) -> Result<workflow_engine::nodes::ResolvedSkillView, PortError> {
        Err(PortError::forbidden("skill 未解決"))
    }
    async fn workflow_start(&self, _c: &ExecCtx, _n: &str, _i: &Value) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_query(&self, _c: &ExecCtx, _f: Uuid, _sql: &str) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_patch(&self, _c: &ExecCtx, _r: CsvPatchReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_write(&self, _c: &ExecCtx, _r: CsvWriteReq) -> Result<Value, PortError> {
        unreachable!()
    }

    async fn data_query(
        &self,
        _c: &ExecCtx,
        table_id: Uuid,
        query: &DataQuery,
    ) -> Result<Value, PortError> {
        if self.unconfigured {
            return Err(PortError::new("unsupported", "未構成", false));
        }
        self.queries.lock().unwrap().push((table_id, query.clone()));
        Ok(json!({
            "items": [ { "id": Uuid::nil().to_string(), "rev": 1, "data": { "status": "pending" } } ],
            "sort_precision": "exact", "sort_degraded": false, "shares_truncated": false,
            "suppressed": false
        }))
    }

    async fn data_write(&self, _c: &ExecCtx, req: DataWriteReq) -> Result<Value, PortError> {
        if std::mem::take(&mut *self.reject_next_write.lock().unwrap()) {
            return Err(PortError::invalid("'title' は必須です"));
        }
        let n = {
            let mut w = self.writes.lock().unwrap();
            w.push(req);
            w.len()
        };
        Ok(json!({ "id": Uuid::nil().to_string(), "rev": n + 1 }))
    }
}

#[derive(Default)]
struct CapturingAudit {
    records: Mutex<Vec<(String, bool, Value)>>,
}
impl CapabilityAudit for CapturingAudit {
    fn record(&self, _t: &str, api: &str, allowed: bool, meta: &Value) {
        self.records
            .lock()
            .unwrap()
            .push((api.to_string(), allowed, meta.clone()));
    }
}

fn lazy_journal() -> EffectJournal {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    EffectJournal::new(pool)
}

fn ctx(input: Value, scopes: &[&str], key: &str) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        step_path: "n1".into(),
        idempotency_key: key.into(),
        attempt: 1,
        principal: Uuid::nil().to_string(),
        principal_kind: "workflow".into(),
        input: input.clone(),
        trigger: input,
        node_outputs: Value::Null,
        each: None,
        trace_id: Some("trace-1".into()),
        scope_ceiling: scopes.iter().map(|s| (*s).to_string()).collect(),
    }
}

const TABLE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";

/// 受け入れ条件: 全面で同一の `DataQuery` 型（ワークフローのノードは data の型をそのまま渡す）。
#[tokio::test]
async fn data_query_node_passes_the_data_query_type_itself() {
    let ports = Arc::new(DataPorts::default());
    let audit = Arc::new(CapturingAudit::default());
    let exec = CapabilityNodeExecutor::new(ports.clone(), lazy_journal(), audit.clone());
    let res = exec
        .execute(
            "data.query",
            &json!({ "table": TABLE, "query": {
                "filter": { "all": [
                    { "cmp": { "field": "status", "op": "eq", "value": "pending" } },
                    { "cmp": { "field": "amount", "op": "gte",
                               "value": { "$from": "input", "path": "/min" } } }
                ] },
                "sort": [ { "field": "amount", "descending": true } ],
                "limit": 20 } }),
            &ctx(json!({ "min": 1000 }), &["data.read"], "k1"),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["items"][0]["data"]["status"], "pending");
    let (table, q) = ports.queries.lock().unwrap().pop().unwrap();
    assert_eq!(table.to_string(), TABLE);
    assert_eq!(
        q,
        DataQuery {
            filter: Some(Condition::All(vec![
                Condition::cmp("status", CondOp::Eq, Some(json!("pending"))),
                Condition::cmp("amount", CondOp::Gte, Some(json!(1000))),
            ])),
            sort: vec![SortKey {
                field: "amount".into(),
                descending: true,
            }],
            limit: Some(20),
            ..DataQuery::default()
        },
        "同じ DataQuery 型（比較値の $from だけが値になる）"
    );
    let rec = audit.records.lock().unwrap();
    assert!(rec
        .iter()
        .any(|(api, ok, meta)| api == "data.query" && *ok && meta["items"] == 1));
}

#[tokio::test]
async fn data_nodes_are_gated_by_the_scope_ceiling() {
    let ports = Arc::new(DataPorts::default());
    let audit = Arc::new(CapturingAudit::default());
    let exec = CapabilityNodeExecutor::new(ports.clone(), lazy_journal(), audit.clone());
    // data.read だけでは書込できない（scope 天井の一段目・ポートへ届かない）。
    for (nt, params) in [
        (
            "data.record.update",
            json!({ "table": TABLE, "record": TABLE, "patch": { "a": 1 } }),
        ),
        (
            "data.transition",
            json!({ "table": TABLE, "record": TABLE, "to": "approved" }),
        ),
        (
            "data.bulk_upsert",
            json!({ "table": TABLE, "rows": [ { "data": {} } ] }),
        ),
    ] {
        let res = exec
            .execute(nt, &params, &ctx(json!({}), &["data.read"], "k"))
            .await;
        assert!(!res.ok, "{nt}");
        assert_eq!(res.error.as_ref().unwrap().code, "out_of_scope", "{nt}");
    }
    // 読取も data.read が無ければ拒否。
    let res = exec
        .execute(
            "data.query",
            &json!({ "table": TABLE }),
            &ctx(json!({}), &["data.write"], "k"),
        )
        .await;
    assert_eq!(res.error.as_ref().unwrap().code, "out_of_scope");
    assert!(ports.writes.lock().unwrap().is_empty());
    assert!(ports.queries.lock().unwrap().is_empty());
    let rec = audit.records.lock().unwrap();
    assert_eq!(
        rec.iter()
            .filter(|(_, ok, meta)| !*ok && meta["reason"] == "out_of_scope")
            .count(),
        4
    );
}

#[tokio::test]
async fn unconfigured_data_port_fails_closed() {
    let ports = Arc::new(DataPorts {
        unconfigured: true,
        ..DataPorts::default()
    });
    let exec =
        CapabilityNodeExecutor::new(ports, lazy_journal(), Arc::new(CapturingAudit::default()));
    let res = exec
        .execute(
            "data.query",
            &json!({ "table": TABLE }),
            &ctx(json!({}), &["data.read"], "k"),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.unwrap().code, "unsupported");
}

#[tokio::test]
async fn unresolvable_operand_does_not_compare_against_null() {
    let ports = Arc::new(DataPorts::default());
    let exec = CapabilityNodeExecutor::new(
        ports.clone(),
        lazy_journal(),
        Arc::new(CapturingAudit::default()),
    );
    let res = exec
        .execute(
            "data.query",
            &json!({ "table": TABLE, "query": { "filter": { "cmp": { "field": "owner", "op": "eq",
                "value": { "$from": "input", "path": "/missing" } } } } }),
            &ctx(json!({}), &["data.read"], "k"),
        )
        .await;
    assert_eq!(res.error.unwrap().code, "expr_resolve_error");
    assert!(ports.queries.lock().unwrap().is_empty());
}

/// 受け入れ条件: shiki script から `Shiki.data.query` が呼べ、スコープ天井外の呼び出しが `out_of_scope` で監査される。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn script_shiki_data_query_and_out_of_scope_audit() {
    const SCRIPT: &str = r#"function main(input) {
  const res = Shiki.data.query(input.table, {
    filter: { cmp: { field: "status", op: "eq", value: "pending" } },
    limit: 5,
  });
  let denied = null;
  try {
    Shiki.data.create(input.table, { title: "x" });
  } catch (e) {
    denied = e.code;
  }
  return { n: res.items.length, first: res.items[0].data.status, denied: denied };
}"#;
    let ports = Arc::new(DataPorts::default());
    let audit = Arc::new(CapturingAudit::default());
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let exec = CapabilityNodeExecutor::new(ports.clone(), lazy_journal(), audit.clone())
        .with_script_engine(engine, script_runtime::engine::Limits::default());
    let res = exec
        .execute(
            "script.run",
            &json!({ "source": { "inline": SCRIPT } }),
            &ctx(json!({ "table": TABLE }), &["data.read"], "wf:t1:s:n1"),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["n"], 1);
    assert_eq!(res.output["first"], "pending");
    assert_eq!(res.output["denied"], "out_of_scope", "data.write が無い");
    // script の query も同じ DataQuery 型でポートへ届く。
    let (table, q) = ports.queries.lock().unwrap().pop().unwrap();
    assert_eq!(table.to_string(), TABLE);
    assert_eq!(
        q.filter,
        Some(Condition::cmp("status", CondOp::Eq, Some(json!("pending"))))
    );
    assert!(
        ports.writes.lock().unwrap().is_empty(),
        "天井外の書込は届かない"
    );
    let rec = audit.records.lock().unwrap();
    assert!(
        rec.iter().any(|(api, ok, meta)| api == "data.create"
            && !*ok
            && meta["reason"] == "out_of_scope"
            && meta["via"] == "script"),
        "{rec:?}"
    );
    assert!(rec
        .iter()
        .any(|(api, ok, meta)| api == "data.query" && *ok && meta["via"] == "script"));
}

/// DB 必要: 書込は effect_journal で高々 1 回（同じステップの再実行・script のホスト呼び出しの再実行）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_writes_are_applied_at_most_once() {
    let Ok(db) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db)
        .await
        .expect("test DB 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let ports = Arc::new(DataPorts::default());
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let exec = CapabilityNodeExecutor::new(
        ports.clone(),
        EffectJournal::new(pool.clone()),
        Arc::new(CapturingAudit::default()),
    )
    .with_script_engine(engine, script_runtime::engine::Limits::default());

    // ノード: 同じ冪等キーで 2 回（ワーカー kill 後の再実行相当）→ 適用は 1 回・記録済みの結果を返す。
    let key = format!("wf:datatest:{}", Uuid::new_v4());
    let params = json!({ "table": TABLE, "record": TABLE, "patch": { "note": "checked" },
                         "expected_rev": 3 });
    let c = ctx(json!({}), &["data.write"], &key);
    let a = exec.execute("data.record.update", &params, &c).await;
    let b = exec.execute("data.record.update", &params, &c).await;
    assert!(a.ok && b.ok, "{:?} {:?}", a.error, b.error);
    assert_eq!(a.output, b.output);
    {
        let w = ports.writes.lock().unwrap();
        assert_eq!(w.len(), 1);
        assert!(matches!(
            &w[0].op,
            DataWriteOp::Update {
                expected_rev: Some(3),
                ..
            }
        ));
        assert_eq!(w[0].idempotency_key, key);
    }
    // 同じキーで別の書込は permanent（静かに別操作の結果を返さない）。
    let other = exec
        .execute(
            "data.record.update",
            &json!({ "table": TABLE, "record": TABLE, "patch": { "note": "other" } }),
            &c,
        )
        .await;
    assert_eq!(other.error.unwrap().code, "effect_conflict");

    // data が拒否した書込（何も書かれていない）は予約を外し、同じステップの再実行がすぐ進める。
    let rkey = format!("wf:datatest:{}", Uuid::new_v4());
    let rc = ctx(json!({}), &["data.write"], &rkey);
    let create = json!({ "table": TABLE, "data": { "title": "t" } });
    *ports.reject_next_write.lock().unwrap() = true;
    let first = exec.execute("data.record.create", &create, &rc).await;
    assert_eq!(first.error.unwrap().code, "invalid");
    let again = exec.execute("data.record.create", &create, &rc).await;
    assert!(
        again.ok,
        "拒否の後の再実行は InProgress で待たされない: {:?}",
        again.error
    );

    // script: 1 ステップ内の 2 つの書込は別キー（#c<seq>）で両方適用、ステップの再実行では増えない。
    const SCRIPT: &str = r#"function main(input) {
  const a = Shiki.data.create(input.table, { title: "a" });
  const b = Shiki.data.transition(input.table, input.table, "approved", 1);
  return { a: a.rev, b: b.rev };
}"#;
    let skey = format!("wf:datatest:{}", Uuid::new_v4());
    let sc = ctx(json!({ "table": TABLE }), &["data.write"], &skey);
    let before = ports.writes.lock().unwrap().len();
    let r1 = exec
        .execute(
            "script.run",
            &json!({ "source": { "inline": SCRIPT } }),
            &sc,
        )
        .await;
    let r2 = exec
        .execute(
            "script.run",
            &json!({ "source": { "inline": SCRIPT } }),
            &sc,
        )
        .await;
    assert!(r1.ok && r2.ok, "{:?} {:?}", r1.error, r2.error);
    assert_eq!(r1.output, r2.output, "再実行は記録済みの結果を返す");
    let w = ports.writes.lock().unwrap();
    assert_eq!(w.len() - before, 2);
    assert!(matches!(&w[before].op, DataWriteOp::Create { .. }));
    assert!(matches!(
        &w[before + 1].op,
        DataWriteOp::Transition { to, expected_rev: Some(1), .. } if to == "approved"
    ));
    assert_ne!(w[before].idempotency_key, w[before + 1].idempotency_key);
}
