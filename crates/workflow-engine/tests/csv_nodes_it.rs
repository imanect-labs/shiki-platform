//! csv.query / csv.patch / csv.write ノードの検証（Task 11P.9）。
//!
//! 純ロジック（DB 不要）: scope ceiling ゲート（操作別 relation の一段目）・dispatch・params 検証。
//! DB 必要（`STORAGE_TEST_DATABASE_URL`）: cross-TX effect_journal による書込の冪等（PIT-31・
//! ステップ再試行で csv.patch/csv.write が高々 1 回しか適用されない）。認可（OpenFGA の viewer/
//! editor/作成権限）は `TabularService` 側で強制され、api の tabular_http_it / tabular の
//! runner_adversarial_it が担保する（ここは engine 層の scope 天井と冪等に集中する）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor,
};

/// csv 系ポートのみ意味を持つ最小 fake（他能力は未使用）。patch/write の適用回数を数える。
#[derive(Default)]
struct CsvPorts {
    patch_calls: Mutex<u32>,
    write_calls: Mutex<u32>,
}

#[async_trait]
impl NodePorts for CsvPorts {
    async fn storage_write(&self, _c: &ExecCtx, _r: StorageWriteReq) -> Result<Value, PortError> {
        unreachable!("csv テストでは未使用")
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
    async fn workflow_start(&self, _c: &ExecCtx, _n: &str, _i: &Value) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_query(&self, _c: &ExecCtx, file_id: Uuid, _sql: &str) -> Result<Value, PortError> {
        Ok(json!({
            "columns": ["n"],
            "rows": [["1"], ["2"], ["3"]],
            "total_rows": 3,
            "file_id": file_id.to_string(),
        }))
    }
    async fn csv_patch(&self, _c: &ExecCtx, req: CsvPatchReq) -> Result<Value, PortError> {
        *self.patch_calls.lock().unwrap() += 1;
        Ok(
            json!({ "node_id": req.file_id.to_string(), "version": req.base_rev + 1, "rows": 3, "cols": 1 }),
        )
    }
    async fn csv_write(&self, _c: &ExecCtx, req: CsvWriteReq) -> Result<Value, PortError> {
        *self.write_calls.lock().unwrap() += 1;
        Ok(json!({ "node_id": Uuid::nil().to_string(), "name": req.name, "version": 0 }))
    }
}

#[derive(Default)]
struct SilentAudit;
impl CapabilityAudit for SilentAudit {
    fn record(&self, _t: &str, _api: &str, _allowed: bool, _meta: &Value) {}
}

fn lazy_journal() -> EffectJournal {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    EffectJournal::new(pool)
}

fn ctx(scopes: Vec<String>) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        step_path: "n1".into(),
        idempotency_key: "wf:t1:0:n1".into(),
        attempt: 1,
        principal: "wf".into(),
        principal_kind: "workflow".into(),
        input: json!({}),
        trigger: json!({}),
        node_outputs: Value::Null,
        each: None,
        trace_id: Some("trace-1".into()),
        scope_ceiling: scopes,
    }
}

#[tokio::test]
async fn csv_query_dispatches_with_csv_read_scope() {
    let exec = CapabilityNodeExecutor::new(
        Arc::new(CsvPorts::default()),
        lazy_journal(),
        Arc::new(SilentAudit),
    );
    let res = exec
        .execute(
            "csv.query",
            &json!({ "file": Uuid::nil().to_string(), "sql": "SELECT * FROM data" }),
            &ctx(vec!["csv.read".into()]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["columns"], json!(["n"]));
}

#[tokio::test]
async fn csv_query_denied_without_scope() {
    let exec = CapabilityNodeExecutor::new(
        Arc::new(CsvPorts::default()),
        lazy_journal(),
        Arc::new(SilentAudit),
    );
    // scope_ceiling に csv.read が無い → 拒否（journal を叩かず一段目で止まる）。
    let res = exec
        .execute(
            "csv.query",
            &json!({ "file": Uuid::nil().to_string(), "sql": "SELECT 1" }),
            &ctx(vec![]),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.unwrap().code, "out_of_scope");
}

#[tokio::test]
async fn csv_patch_denied_without_csv_write_scope() {
    // 操作別 relation の一段目: csv.patch は csv.write スコープを要する（csv.read だけでは不可）。
    let ports = Arc::new(CsvPorts::default());
    let exec = CapabilityNodeExecutor::new(ports.clone(), lazy_journal(), Arc::new(SilentAudit));
    let res = exec
        .execute(
            "csv.patch",
            &json!({
                "file": Uuid::nil().to_string(),
                "base_rev": 0,
                "ops": [{ "op": "cell_update", "row": 0, "col": "n", "value": "9" }]
            }),
            &ctx(vec!["csv.read".into()]),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.unwrap().code, "out_of_scope");
    // 拒否は副作用に到達しない。
    assert_eq!(*ports.patch_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn csv_patch_rejects_non_array_ops() {
    let exec = CapabilityNodeExecutor::new(
        Arc::new(CsvPorts::default()),
        lazy_journal(),
        Arc::new(SilentAudit),
    );
    let res = exec
        .execute(
            "csv.patch",
            &json!({ "file": Uuid::nil().to_string(), "base_rev": 0, "ops": "nope" }),
            &ctx(vec!["csv.write".into()]),
        )
        .await;
    assert!(!res.ok);
    // params 契約違反（ops は配列）。node_csv_patch が PortError::invalid で弾く。
    assert_eq!(res.error.unwrap().code, "invalid");
}

/// DB 必須: 同一冪等キーでの再試行が csv.patch を高々 1 回しか適用しない（PIT-31）。
#[tokio::test]
async fn csv_patch_is_idempotent_across_retry() {
    let Ok(db) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db)
        .await
        .expect("test DB 接続");
    // effect_journal テーブルは storage/workflow の migration で作られる。冪等キーを一意にする。
    let key = format!("wf:csvtest:{}", Uuid::new_v4());
    let ports = Arc::new(CsvPorts::default());
    let exec = CapabilityNodeExecutor::new(
        ports.clone(),
        EffectJournal::new(pool.clone()),
        Arc::new(SilentAudit),
    );

    let mut c = ctx(vec!["csv.write".into()]);
    c.idempotency_key = key.clone();
    let params = json!({
        "file": Uuid::nil().to_string(),
        "base_rev": 0,
        "ops": [{ "op": "cell_update", "row": 0, "col": "n", "value": "9" }]
    });

    let r1 = exec.execute("csv.patch", &params, &c).await;
    assert!(r1.ok, "{:?}", r1.error);
    // 同一キーで再試行（ワーカーのクラッシュ後の再実行を模す）。
    let r2 = exec.execute("csv.patch", &params, &c).await;
    assert!(r2.ok, "{:?}", r2.error);

    // 副作用（ポート適用）は高々 1 回。2 回目は journal の AlreadyDone が畳む。
    assert_eq!(
        *ports.patch_calls.lock().unwrap(),
        1,
        "csv.patch が重複適用された"
    );
    // 冪等記録の掃除（他テストへ影響させない）。
    let _ = sqlx::query("DELETE FROM effect_journal WHERE idempotency_key = $1")
        .bind(&key)
        .execute(&pool)
        .await;
}
