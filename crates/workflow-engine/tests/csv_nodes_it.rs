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
use workflow_engine::ir::params::NameTakenPolicy;
use workflow_engine::nodes::ports::{
    name_taken_error, AgentInvokeReq, CsvPatchReq, CsvQueryReq, CsvWriteReq, ExecCtx, HttpSendReq,
    HttpSendResp, LlmInvokeReq, NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor,
};

/// csv 系ポートのみ意味を持つ最小 fake（他能力は未使用）。patch/write の適用回数を数える。
#[derive(Default)]
struct CsvPorts {
    /// 最後に受けた csv.query の追加テーブル（name, file_id）。
    query_tables: Mutex<Vec<(String, Uuid)>>,
    patch_calls: Mutex<u32>,
    write_calls: Mutex<u32>,
    /// 保存先にすでにある名前（csv.write は `name_taken` で断る・BUG-97）。
    taken_names: Mutex<Vec<String>>,
    /// 番号を付ける扱いで、ポートの先（StorageService）が付けた名前。`None` は上限まで埋まっている（D-124）。
    numbered_as: Mutex<Option<String>>,
    /// 上限まで埋まっていたときに、ポートの先が最後に試した名前（文はポートが組む）。
    last_tried_as: Mutex<Option<String>>,
    /// csv.write に渡った要求の並び。
    writes: Mutex<Vec<CsvWriteTry>>,
    /// 次の書込（csv.patch・csv.write・workflow.start）が返す誤り（返したら消える・BUG-188）。
    refuse_next: Mutex<Option<PortError>>,
    /// workflow.start が子を起動した回数。
    start_calls: Mutex<u32>,
}

impl CsvPorts {
    /// 仕込んだ誤りがあれば返す（1 回きり）。
    fn refused(&self) -> Result<(), PortError> {
        match self.refuse_next.lock().unwrap().take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// csv.write に渡った（名前・扱い・文字コードを含む中身）。
type CsvWriteTry = (String, NameTakenPolicy, Vec<u8>);

#[async_trait]
impl NodePorts for CsvPorts {
    async fn storage_write(&self, _c: &ExecCtx, _r: StorageWriteReq) -> Result<Value, PortError> {
        unreachable!("csv テストでは未使用")
    }
    async fn storage_read(&self, _c: &ExecCtx, _f: Uuid) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn storage_list(
        &self,
        _c: &ExecCtx,
        _: workflow_engine::StorageListReq,
    ) -> Result<Value, PortError> {
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
    async fn agent_invoke(
        &self,
        _c: &ExecCtx,
        _r: AgentInvokeReq,
        _tools: std::sync::Arc<dyn workflow_engine::AgentToolGateway>,
    ) -> Result<Value, PortError> {
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
        Err(PortError::forbidden(
            "skill 未解決（テスト既定・fail-closed）",
        ))
    }

    async fn workflow_start(&self, _c: &ExecCtx, _n: &str, _i: &Value) -> Result<Value, PortError> {
        self.refused()?;
        *self.start_calls.lock().unwrap() += 1;
        Ok(json!({ "run_id": Uuid::new_v4().to_string() }))
    }
    async fn csv_query(&self, _c: &ExecCtx, req: CsvQueryReq) -> Result<Value, PortError> {
        let file_id = req.file_id;
        *self.query_tables.lock().unwrap() = req
            .tables
            .iter()
            .map(|t| (t.name.clone(), t.file_id))
            .collect();
        Ok(json!({
            "columns": ["n"],
            "rows": [["1"], ["2"], ["3"]],
            "total_rows": 3,
            "file_id": file_id.to_string(),
        }))
    }
    async fn csv_patch(&self, _c: &ExecCtx, req: CsvPatchReq) -> Result<Value, PortError> {
        self.refused()?;
        *self.patch_calls.lock().unwrap() += 1;
        Ok(
            json!({ "node_id": req.file_id.to_string(), "version": req.base_rev + 1, "rows": 3, "cols": 1 }),
        )
    }
    async fn csv_write(&self, _c: &ExecCtx, req: CsvWriteReq) -> Result<Value, PortError> {
        self.refused()?;
        self.writes.lock().unwrap().push((
            req.name.clone(),
            req.on_name_taken,
            req.csv_bytes.clone(),
        ));
        let mut saved = req.name.clone();
        if self.taken_names.lock().unwrap().contains(&req.name) {
            // 番号はポートの先が付ける（ここでは付けた名前を仕込んでおく・規則を写さない）。
            match (req.on_name_taken, self.numbered_as.lock().unwrap().clone()) {
                (NameTakenPolicy::AddNumber, Some(n)) => saved = n,
                (NameTakenPolicy::AddNumber, None) => {
                    let last = self.last_tried_as.lock().unwrap().clone().unwrap();
                    return Err(name_taken_error(&req.name, Some(&last)));
                }
                (NameTakenPolicy::Fail, _) => {
                    return Err(PortError::new(
                        workflow_engine::nodes::ports::NAME_TAKEN,
                        "同じ名前のファイルがすでにあります",
                        false,
                    ))
                }
            }
        }
        *self.write_calls.lock().unwrap() += 1;
        Ok(json!({ "node_id": Uuid::nil().to_string(), "name": saved, "version": 0 }))
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

/// テスト用の DB に繋ぎ、migration を流す（effect_journal は migration が作るので、このファイルだけを新しい DB で
/// 流しても通す）。接続先が未設定なら `None`（スキップ・CI は `SHIKI_REQUIRE_IT=1` で失敗に変える）。
async fn test_pool() -> Option<sqlx::PgPool> {
    let Ok(db) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
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
    Some(pool)
}

fn ctx(scopes: Vec<String>) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        run_started_at: chrono::Utc::now(),
        run_scheduled_at: None,
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
        cancel: workflow_engine::run::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
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

/// #1037: tables（JOIN 用の追加 CSV）が解決されてポートへ渡る（$from 参照も可）。
#[tokio::test]
async fn csv_query_passes_extra_tables_to_port() {
    let ports = Arc::new(CsvPorts::default());
    let exec = CapabilityNodeExecutor::new(ports.clone(), lazy_journal(), Arc::new(SilentAudit));
    let other = Uuid::new_v4();
    let mut c = ctx(vec!["csv.read".into()]);
    c.input = json!({ "customers": other.to_string() });
    let res = exec
        .execute(
            "csv.query",
            &json!({
                "file": Uuid::nil().to_string(),
                "sql": "SELECT * FROM data JOIN customers USING (id)",
                "tables": [{ "name": "customers", "file": { "$from": "input", "path": "/customers" } }]
            }),
            &c,
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(
        *ports.query_tables.lock().unwrap(),
        vec![("customers".to_string(), other)]
    );
}

/// #1037: tables の形が不正なら permanent の invalid、値式が解決できない（null 含む）なら
/// expr_resolve_error（ポートは呼ばない・黙って単一 CSV に落とさない）。
#[tokio::test]
async fn csv_query_rejects_malformed_tables() {
    let exec = CapabilityNodeExecutor::new(
        Arc::new(CsvPorts::default()),
        lazy_journal(),
        Arc::new(SilentAudit),
    );
    let mut c = ctx(vec!["csv.read".into()]);
    c.input = json!({ "nothing": null });
    for (tables, code) in [
        (json!("customers"), "invalid"),
        (json!([{ "name": "customers" }]), "invalid"),
        (
            json!([{ "name": "customers", "file": "not-a-uuid" }]),
            "invalid",
        ),
        (json!([{ "file": Uuid::nil().to_string() }]), "invalid"),
        (
            json!({ "$from": "input", "path": "/missing" }),
            "expr_resolve_error",
        ),
        (
            json!({ "$from": "input", "path": "/nothing" }),
            "expr_resolve_error",
        ),
        (
            json!([{ "name": "c", "file": { "$from": "input", "path": "/missing" } }]),
            "expr_resolve_error",
        ),
    ] {
        let res = exec
            .execute(
                "csv.query",
                &json!({ "file": Uuid::nil().to_string(), "sql": "SELECT 1", "tables": tables }),
                &c,
            )
            .await;
        assert!(!res.ok, "拒否されるべき tables: {tables}");
        assert_eq!(res.error.unwrap().code, code, "tables: {tables}");
    }
}

/// 実行時に解決された tables の中身は値式として**再評価しない**（トリガ payload 等に仕込んだ
/// `$from` で、配線されていない値を引き込ませない）。
#[tokio::test]
async fn csv_query_does_not_reevaluate_runtime_tables() {
    let ports = Arc::new(CsvPorts::default());
    let exec = CapabilityNodeExecutor::new(ports.clone(), lazy_journal(), Arc::new(SilentAudit));
    let other = Uuid::new_v4();
    let mut c = ctx(vec!["csv.read".into()]);
    c.input = json!({
        "secret": "leaked",
        "tables": [{ "name": { "$from": "input", "path": "/secret" }, "file": other.to_string() }],
        "ok": [{ "name": "customers", "file": other.to_string() }]
    });
    let res = exec
        .execute(
            "csv.query",
            &json!({ "file": Uuid::nil().to_string(), "sql": "SELECT 1",
                     "tables": { "$from": "input", "path": "/tables" } }),
            &c,
        )
        .await;
    assert!(
        !res.ok,
        "埋め込まれた値式は文字列として扱われず name 不正になる"
    );
    assert_eq!(res.error.unwrap().code, "invalid");
    assert!(
        ports.query_tables.lock().unwrap().is_empty(),
        "ポートは呼ばれない"
    );

    // データとして正しい形なら、そのまま使える。
    let res = exec
        .execute(
            "csv.query",
            &json!({ "file": Uuid::nil().to_string(), "sql": "SELECT 1",
                     "tables": { "$from": "input", "path": "/ok" } }),
            &c,
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(
        *ports.query_tables.lock().unwrap(),
        vec![("customers".to_string(), other)]
    );
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
    let Some(pool) = test_pool().await else {
        return;
    };
    // 冪等キーを一意にする（ほかのテストと記録を共有しない）。
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

/// DB 必須: csv.write は同じ名前があるとき、省略なら失敗にし、番号を付ける扱いなら `集計 (2).csv` で保存する。
/// エンジンは頼まれた名前（`.csv` を付けた名前）と扱いをポートへ 1 回で渡し、番号はポートの先（StorageService）が
/// 付ける（名前を変えて呼び直さない・D-124）。journal の書込は 1 つで、再試行は保存し直さない（BUG-97・PIT-31）。
/// 上限まで埋まっていれば、ポートが最後に試した名前で組んだ文のまま失敗にする。
#[tokio::test]
async fn csv_write_numbers_taken_names_once_across_retry() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let ports = Arc::new(CsvPorts::default());
    *ports.taken_names.lock().unwrap() = vec!["集計.csv".into()];
    *ports.numbered_as.lock().unwrap() = Some("集計 (2).csv".into());
    let exec = CapabilityNodeExecutor::new(
        ports.clone(),
        EffectJournal::new(pool.clone()),
        Arc::new(SilentAudit),
    );
    let keys: Vec<String> = (0..3)
        .map(|_| format!("wf:csvname:{}", Uuid::new_v4()))
        .collect();

    // 省略（既存の定義）は失敗にする。.csv は付けてから頼む。
    let mut c = ctx(vec!["csv.write".into()]);
    c.idempotency_key = keys[0].clone();
    let res = exec
        .execute(
            "csv.write",
            &json!({ "name": "集計", "content": "a\n1" }),
            &c,
        )
        .await;
    assert!(!res.ok);
    let err = res.error.unwrap();
    assert_eq!(err.code, "name_taken");
    assert!(!err.retryable);
    assert!(
        err.message.contains("同じ名前のファイル「集計.csv」"),
        "{}",
        err.message
    );

    // 番号を付ける: 頼まれた名前と扱いを 1 回で渡し、同じキーの再試行は journal が畳む。
    let mut c = ctx(vec!["csv.write".into()]);
    c.idempotency_key = keys[1].clone();
    let params = json!({
        "name": "集計", "content": "a\n1", "on_name_taken": "add_number", "encoding": "utf8_bom"
    });
    let r1 = exec.execute("csv.write", &params, &c).await;
    assert!(r1.ok, "{:?}", r1.error);
    assert_eq!(r1.output["name"], "集計 (2).csv");
    assert_eq!(r1.output["file_id"], r1.output["node_id"]);
    let r2 = exec.execute("csv.write", &params, &c).await;
    assert!(r2.ok, "{:?}", r2.error);
    assert_eq!(r2.output["name"], "集計 (2).csv");
    assert_eq!(r2.output["file_id"], r2.output["node_id"]);
    assert_eq!(
        *ports.write_calls.lock().unwrap(),
        1,
        "再試行で保存し直した"
    );
    let writes = ports.writes.lock().unwrap().clone();
    let asked: Vec<(&str, NameTakenPolicy)> =
        writes.iter().map(|(n, p, _)| (n.as_str(), *p)).collect();
    assert_eq!(
        asked,
        [
            ("集計.csv", NameTakenPolicy::Fail),
            ("集計.csv", NameTakenPolicy::AddNumber)
        ],
        "エンジンが名前を変えて呼び直した"
    );
    // 選んだ文字コード（BOM 付きの UTF-8）のまま、ポートへ 1 回で渡す（D-120）。
    assert_eq!(writes[1].2, b"\xEF\xBB\xBFa\n1");

    // 上限まで埋まっていれば、ポートが組んだ文（最後に試した名前）のまま失敗にし、呼び直さない。
    *ports.numbered_as.lock().unwrap() = None;
    *ports.last_tried_as.lock().unwrap() = Some("集計 (50).csv".into());
    let mut c = ctx(vec!["csv.write".into()]);
    c.idempotency_key = keys[2].clone();
    let res = exec.execute("csv.write", &params, &c).await;
    assert!(!res.ok);
    let err = res.error.unwrap();
    assert_eq!(err.code, "name_taken");
    assert!(!err.retryable);
    assert!(
        err.message
            .contains("「集計.csv」と、番号を付けた「集計 (50).csv」まで"),
        "{}",
        err.message
    );
    assert_eq!(ports.writes.lock().unwrap().len(), 3);

    for key in &keys {
        let _ = sqlx::query("DELETE FROM effect_journal WHERE idempotency_key = $1")
            .bind(key)
            .execute(&pool)
            .await;
    }
}

/// CSV にできない値（数・表でない項目のまとまり・まとまりでないもののリスト）は、JSON の文字のまま保存せずに
/// `invalid` で断る（保存の前・journal にも触れない・BUG-178）。
#[tokio::test]
async fn csv_write_refuses_values_that_cannot_be_csv() {
    let ports = Arc::new(CsvPorts::default());
    let exec = CapabilityNodeExecutor::new(ports.clone(), lazy_journal(), Arc::new(SilentAudit));
    for (content, starts) in [
        (
            json!(12),
            "「CSV の内容」に数が入っているため、CSV にできません。",
        ),
        (
            json!({ "total": 3 }),
            "「CSV の内容」に表でない項目のまとまりが入っているため、CSV にできません。",
        ),
        (
            json!(["a", "b"]),
            "「CSV の内容」のリストの 1 件目が項目のまとまりではありません。",
        ),
    ] {
        let mut c = ctx(vec!["csv.write".into()]);
        c.node_outputs = json!({ "calc": { "value": content } });
        let res = exec
            .execute(
                "csv.write",
                &json!({ "name": "集計", "content": { "$from": "nodes.calc.output", "path": "/value" } }),
                &c,
            )
            .await;
        assert!(!res.ok);
        let err = res.error.unwrap();
        assert_eq!(err.code, "invalid");
        assert!(!err.retryable);
        assert!(err.message.starts_with(starts), "{}", err.message);
    }
    assert!(
        ports.writes.lock().unwrap().is_empty(),
        "断った内容を保存した"
    );
}

/// DB 必須: 「CSV を集計」の結果（表）と項目のまとまりのリストは、CSV に組み立ててから選んだ文字コードで保存する
/// （JSON の文字のまま .csv にしない・BUG-178）。文字の内容はこれまでどおりそのまま保存する。
#[tokio::test]
async fn csv_write_builds_csv_from_tables_and_lists() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let ports = Arc::new(CsvPorts::default());
    let exec = CapabilityNodeExecutor::new(
        ports.clone(),
        EffectJournal::new(pool.clone()),
        Arc::new(SilentAudit),
    );
    let outputs = json!({
        "q": {
            "columns": ["品目", "金額"], "column_types": ["VARCHAR", "BIGINT"],
            "rows": [["交通費", "1200"], ["=HYPERLINK(\"x\")", "-300"]],
            "total_rows": 2, "truncated": false, "version": 1
        },
        "s": [{ "name": "佐藤", "amount": 1200 }, { "name": "鈴木, 一郎" }],
        "t": "a,b\n1,2\n"
    });
    let cases = [
        (
            "q",
            "utf8_bom",
            "\u{FEFF}品目,金額\r\n交通費,1200\r\n\"'=HYPERLINK(\"\"x\"\")\",-300\r\n",
        ),
        (
            "s",
            "utf8",
            "amount,name\r\n1200,佐藤\r\n,\"鈴木, 一郎\"\r\n",
        ),
        ("t", "utf8", "a,b\n1,2\n"),
    ];
    let mut keys = Vec::new();
    for (source, encoding, want) in cases {
        let key = format!("wf:csvcompose:{}", Uuid::new_v4());
        keys.push(key.clone());
        let mut c = ctx(vec!["csv.write".into()]);
        c.idempotency_key = key;
        c.node_outputs = outputs.clone();
        let res = exec
            .execute(
                "csv.write",
                &json!({
                    "name": format!("集計-{source}"),
                    "content": { "$from": format!("nodes.{source}.output") },
                    "encoding": encoding
                }),
                &c,
            )
            .await;
        assert!(res.ok, "{source}: {:?}", res.error);
        let writes = ports.writes.lock().unwrap().clone();
        let (name, _, bytes) = writes.last().unwrap();
        assert_eq!(name, &format!("集計-{source}.csv"));
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), want, "{source}");
    }
    for key in &keys {
        let _ = sqlx::query("DELETE FROM effect_journal WHERE idempotency_key = $1")
            .bind(key)
            .execute(&pool)
            .await;
    }
}

/// DB 必須: 書込のポートが副作用の無い誤り（unavailable 以外）を返したら、effect_journal の予約を外す（data.* と同じ・
/// BUG-188）。原因を直して同じステップを再開すると、回収の猶予（5 分）を待たずにすぐ書ける。書込が済んだか分からない
/// 一時障害（unavailable）では予約を残し、同じステップの再実行は二重に書かずに待つ。csv.patch・csv.write・
/// workflow.start と、script の `Shiki.workflow.start` を確かめる。
#[tokio::test]
async fn refused_writes_release_the_journal_reservation() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let ports = Arc::new(CsvPorts::default());
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let exec = CapabilityNodeExecutor::new(
        ports.clone(),
        EffectJournal::new(pool.clone()),
        Arc::new(SilentAudit),
    )
    .with_script_engine(engine, script_runtime::engine::Limits::default());
    let script = json!({ "source": { "inline":
        "function main(input) { return Shiki.workflow.start(\"子\", { n: 1 }); }" } });
    let cases: [(&str, Value, &str); 4] = [
        (
            "csv.patch",
            json!({ "file": Uuid::nil().to_string(), "base_rev": 0,
                    "ops": [{ "op": "cell_update", "row": 0, "col": "n", "value": "9" }] }),
            "csv.write",
        ),
        (
            "csv.write",
            json!({ "name": "集計", "content": "a\n1" }),
            "csv.write",
        ),
        (
            "workflow.start",
            json!({ "name": "子", "input": { "n": 1 } }),
            "workflow.start",
        ),
        ("script.run", script, "workflow.start"),
    ];
    let calls = |ports: &CsvPorts| {
        *ports.patch_calls.lock().unwrap()
            + *ports.write_calls.lock().unwrap()
            + *ports.start_calls.lock().unwrap()
    };
    let mut keys = Vec::new();
    for (node_type, params, scope) in &cases {
        // 副作用の無い誤り: 権限・名前の衝突・見つからない・入力の誤り。
        for refusal in [
            PortError::forbidden("権限なし"),
            PortError::new(workflow_engine::nodes::ports::NAME_TAKEN, "同じ名前", false),
            PortError::new("not_found", "見つかりません", false),
            PortError::invalid("入力が正しくありません"),
        ] {
            let key = format!("wf:release:{}", Uuid::new_v4());
            keys.push(key.clone());
            let mut c = ctx(vec![(*scope).into()]);
            c.idempotency_key = key.clone();
            let code = refusal.code.clone();
            *ports.refuse_next.lock().unwrap() = Some(refusal);
            let first = exec.execute(node_type, params, &c).await;
            assert!(!first.ok, "{node_type}: {code}");
            assert_eq!(first.error.unwrap().code, code, "{node_type}");
            let before = calls(&ports);
            let again = exec.execute(node_type, params, &c).await;
            assert!(
                again.ok,
                "{node_type}: {code} の後の再開が待たされた: {:?}",
                again.error
            );
            assert_eq!(calls(&ports), before + 1, "{node_type}: {code}");
        }
        // 書込が済んだか分からない一時障害は予約を残す（同じステップの再実行は書かずに待つ）。
        let key = format!("wf:release:{}", Uuid::new_v4());
        keys.push(key.clone());
        let mut c = ctx(vec![(*scope).into()]);
        c.idempotency_key = key.clone();
        *ports.refuse_next.lock().unwrap() = Some(PortError::unavailable("db"));
        let first = exec.execute(node_type, params, &c).await;
        assert_eq!(first.error.unwrap().code, "unavailable", "{node_type}");
        let before = calls(&ports);
        let again = exec.execute(node_type, params, &c).await;
        assert_eq!(
            again.error.map(|e| e.code),
            Some("effect_in_progress".to_string()),
            "{node_type}: 一時障害の後に二重に書いた"
        );
        assert_eq!(calls(&ports), before, "{node_type}");
    }
    // script の書込はホスト呼び出しごとの冪等キー（ステップのキー + #c<seq>）。
    for key in &keys {
        let _ = sqlx::query(
            "DELETE FROM effect_journal WHERE idempotency_key = $1 OR idempotency_key LIKE $1 || '#%'",
        )
        .bind(key)
        .execute(&pool)
        .await;
    }
}
