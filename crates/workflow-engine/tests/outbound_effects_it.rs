//! 外への呼び出し（llm.invoke・http.request・Shiki.http.request）の効果の記録の結合テスト（実 Postgres・GAP-28）。
//!
//! リースが切れて step が拾い直されたときに、前の実行が済ませた呼び出しを繰り返さない（二重課金・二重送信）ことを、
//! ワーカーが落ちた形（リースの切れた claim のあとに checkpoint しない）と、リースを失ったまま呼んでいる形で確かめる。
//! 送っている途中で落ちた外部への送信は、送り直さずに「届いたか分からない」で止まり、再開で送り直す。
//! `STORAGE_TEST_DATABASE_URL` が無ければスキップ。各テストは自分のテナントに閉じ、最後に run と記録を消す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::nodes::http::HTTP_DELIVERY_UNKNOWN;
use workflow_engine::nodes::outbound::RECORD_MAX_BYTES;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::run::graph::RunGraph;
use workflow_engine::run::store::ClaimedStep;
use workflow_engine::run::CancelSignal;
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor,
    ResumeOutcome, RunStatus, RunStore, WorkerConfig, WorkflowWorker,
};

/// 呼び出しの途中で止められる口（`entered` で呼び出しに入ったことを知らせ、`release` まで戻らない）。
#[derive(Default)]
struct Gate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

/// LLM の呼び出しと外部への送信を数えるポート。
#[derive(Default)]
struct CountingPorts {
    llm_calls: AtomicUsize,
    sends: Mutex<Vec<String>>,
    /// 置いてあれば、次の呼び出しはここで止まる（1 回だけ）。
    gate: Mutex<Option<Arc<Gate>>>,
    /// 外部の応答の本文（無ければ `{"ok":true}`）。
    http_body: Mutex<Option<Vec<u8>>>,
    /// 外部の応答の状態コード（無ければ 201）。
    http_status: Mutex<Option<u16>>,
    /// 次の送信を一度だけ一時的な誤り（`unavailable`）で失敗させる URL（送ったことは数える）。
    fail_once: Mutex<Vec<String>>,
}

impl CountingPorts {
    async fn maybe_block(&self) {
        let gate = self.gate.lock().unwrap().take();
        if let Some(g) = gate {
            g.entered.notify_one();
            g.release.notified().await;
        }
    }
    fn sends(&self) -> usize {
        self.sends.lock().unwrap().len()
    }
}

fn unused() -> PortError {
    PortError::unavailable("このテストでは使わない")
}

#[async_trait]
impl NodePorts for CountingPorts {
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
        let n = self.llm_calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.maybe_block().await;
        Ok(json!({ "text": format!("答え {n}"), "model": "stub" }))
    }
    async fn agent_invoke(
        &self,
        _: &ExecCtx,
        _: AgentInvokeReq,
        _: Arc<dyn workflow_engine::AgentToolGateway>,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn http_send(&self, _: &ExecCtx, req: HttpSendReq) -> Result<HttpSendResp, PortError> {
        self.sends
            .lock()
            .unwrap()
            .push(format!("{} {}", req.method, req.url));
        let n = self.sends();
        {
            let mut fail = self.fail_once.lock().unwrap();
            if let Some(i) = fail.iter().position(|u| *u == req.url) {
                fail.remove(i);
                return Err(PortError::unavailable("送り先が一時的に応答しません"));
            }
        }
        self.maybe_block().await;
        let body = self
            .http_body
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| format!(r#"{{"ok":true,"n":{n}}}"#).into_bytes());
        let status = self.http_status.lock().unwrap().unwrap_or(201);
        Ok(HttpSendResp { status, body })
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
        _: workflow_engine::CsvQueryReq,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
}

/// step の行（状態, attempt, 待っている理由, 誤りの code, 結果）。
type StepRow = (
    String,
    i32,
    Option<String>,
    Option<String>,
    Option<sqlx::types::Json<Value>>,
);

struct NoAudit;
impl CapabilityAudit for NoAudit {
    fn record(&self, _: &str, _: &str, _: bool, _: &Value) {}
}

struct Env {
    pool: PgPool,
    store: RunStore,
    tenant: String,
    ports: Arc<CountingPorts>,
    exec: Arc<CapabilityNodeExecutor>,
}

async fn setup() -> Option<Env> {
    setup_with(false).await
}

/// `script`: script.run を動かせるようにする（エンジンの準備に時間がかかるので、使うテストだけ）。
async fn setup_with(script: bool) -> Option<Env> {
    let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
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
    let ports = Arc::new(CountingPorts::default());
    let mut exec = CapabilityNodeExecutor::new(
        Arc::clone(&ports) as Arc<dyn NodePorts>,
        EffectJournal::new(pool.clone()),
        Arc::new(NoAudit),
    )
    .with_http_allowlist(vec!["api.example.com".into()], 5_000);
    if script {
        let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
        exec = exec.with_script_engine(engine, script_runtime::engine::Limits::default());
    }
    let exec = Arc::new(exec);
    Some(Env {
        store: RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS),
        pool,
        tenant: format!("t-{}", Uuid::new_v4()),
        ports,
        exec,
    })
}

impl Env {
    /// 1 ブロックだけの run のワークフローの id。
    async fn workflow_id(&self) -> Uuid {
        sqlx::query_scalar("SELECT workflow_id FROM workflow_run WHERE tenant_id = $1")
            .bind(&self.tenant)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// 1 ブロックだけの run を作る。
    async fn run(&self, node: Value, scopes: &[&str]) -> Uuid {
        let ir = json!({
            "ir_version": 1, "name": "outbound", "declared_scopes": scopes,
            "nodes": [node], "edges": []
        });
        let parsed = workflow_engine::WorkflowIr::from_json(&ir).unwrap();
        self.store
            .create_run(
                &self.tenant,
                "acme",
                Uuid::new_v4(),
                1,
                "interactive",
                None,
                "alice",
                "user",
                &json!({}),
                &ir,
                &RunGraph::build(&parsed),
            )
            .await
            .expect("create_run")
            .expect("admitted")
    }

    /// リースの切れた claim（このワーカーは落ちたものとする）。
    async fn claim_as_dead_worker(&self) -> ClaimedStep {
        self.store
            .claim_ready_step("w-dead", -1, Some(&self.tenant))
            .await
            .unwrap()
            .expect("claim")
    }

    fn worker(&self) -> WorkflowWorker {
        WorkflowWorker::new(
            self.store.clone(),
            Arc::clone(&self.exec) as Arc<dyn NodeExecutor>,
            WorkerConfig {
                idle_poll: Duration::from_millis(50),
                ..WorkerConfig::default()
            },
        )
        .scoped_to_tenant(self.tenant.clone())
    }

    /// 落ちたワーカーの step を sweeper が戻し、次のワーカーが拾って 1 回動かす。
    async fn reclaim_and_run_once(&self) {
        assert_eq!(
            self.store
                .reclaim_expired_leases(Some(&self.tenant))
                .await
                .unwrap(),
            1
        );
        assert_eq!(self.worker().claim_and_run_once("w-b").await, Ok(true));
    }

    /// [`Self::reclaim_and_run_once`] と同じ。確かめずに（戻した数, 動かした結果）を返す（片付けの後で確かめる）。
    async fn reclaim_and_run_once_unchecked(&self) -> (u64, Result<bool, String>) {
        let reclaimed = self
            .store
            .reclaim_expired_leases(Some(&self.tenant))
            .await
            .unwrap();
        let ran = self.worker().claim_and_run_once("w-b").await;
        (reclaimed, ran)
    }

    /// （状態, attempt, 待っている理由, 誤りの code, 結果）。
    async fn step(&self, run: Uuid) -> (String, i32, Option<String>, Option<String>, Value) {
        let (status, attempt, wait, code, out): StepRow = sqlx::query_as(
            "SELECT status, attempt, wait_code, error->>'code', output FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&self.tenant)
        .bind(run)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (
            status,
            attempt,
            wait,
            code,
            out.map_or(Value::Null, |j| j.0),
        )
    }

    /// 前の実行の予約を回収の猶予より古くし、待っている step をすぐ拾えるようにする（時間を進めるかわり）。
    async fn age_reservations_and_wake(&self) {
        sqlx::query(
            "UPDATE effect_journal SET reserved_at = now() - interval '301 seconds' \
             WHERE tenant_id = $1",
        )
        .bind(&self.tenant)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE step_execution SET next_retry_at = now() \
             WHERE tenant_id = $1 AND status = 'ready'",
        )
        .bind(&self.tenant)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn journal_rows(&self) -> Vec<(String, Option<Value>)> {
        sqlx::query_as::<_, (String, Option<sqlx::types::Json<Value>>)>(
            "SELECT idempotency_key, result_summary FROM effect_journal WHERE tenant_id = $1 \
             ORDER BY idempotency_key",
        )
        .bind(&self.tenant)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(k, v)| (k, v.map(|j| j.0)))
        .collect()
    }

    async fn cleanup(&self) {
        for sql in [
            "DELETE FROM workflow_run WHERE tenant_id = $1",
            "DELETE FROM effect_journal WHERE tenant_id = $1",
        ] {
            sqlx::query(sql)
                .bind(&self.tenant)
                .execute(&self.pool)
                .await
                .unwrap();
        }
    }
}

/// 落ちたワーカーが claim した step を、ワーカーと同じ文脈で実行する（checkpoint はしない）。
fn ctx_of(c: &ClaimedStep, scopes: &[&str]) -> NodeContext {
    NodeContext {
        tenant_id: c.tenant_id.clone(),
        org: c.org.clone(),
        run_id: c.run_id,
        run_started_at: c.run_started_at,
        run_scheduled_at: c.run_scheduled_at,
        step_path: c.step_path.clone(),
        idempotency_key: c.idempotency_key.clone(),
        attempt: c.attempt,
        principal: c.principal.clone(),
        principal_kind: c.principal_kind.clone(),
        input: c.input.0.clone(),
        trigger: c.trigger_or_input(),
        node_outputs: Value::Null,
        each: None,
        trace_id: None,
        scope_ceiling: scopes.iter().map(ToString::to_string).collect(),
        cancel: CancelSignal::default(),
        workflow_id: c.workflow_id,
        cache: None,
    }
}

fn llm_node() -> Value {
    json!({ "id": "ai", "type": "llm.invoke", "params": { "prompt": "要約して" } })
}

fn post_node() -> Value {
    json!({ "id": "post", "type": "http.request", "params": {
        "method": "POST", "url": "https://api.example.com/hook", "body": { "n": 1 } } })
}

/// AI に聞いたあと、checkpoint の前にワーカーが落ちても、拾い直した実行は AI を呼び直さず、記録した答えを使う。
#[tokio::test]
async fn a_reclaimed_llm_step_reuses_the_recorded_answer() {
    let Some(env) = setup().await else { return };
    let run = env.run(llm_node(), &[]).await;
    let dead = env.claim_as_dead_worker().await;
    let first = env
        .exec
        .execute("llm.invoke", &llm_node()["params"], &ctx_of(&dead, &[]))
        .await;
    assert!(first.ok, "{:?}", first.error);
    env.reclaim_and_run_once().await;
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let calls = env.ports.llm_calls.load(Ordering::SeqCst);
    env.cleanup().await;

    assert_eq!(calls, 1, "拾い直しで AI を呼び直した（二重に課金される）");
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(step.0, "succeeded");
    assert_eq!(step.1, 1, "落ちたワーカーの実行は試す回数に数えない");
    assert_eq!(step.4, first.output, "前の実行の答えを次へ渡す");
    assert_eq!(step.4["text"], json!("答え 1"));
}

/// リースを失ったまま AI を呼んでいるあいだに拾い直されても、呼び直さずに待ち（試す回数を使わない）、前の実行が
/// 記録した答えを使う。前の実行の checkpoint は捨てられる（fencing）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_takeover_waits_for_the_running_llm_call_instead_of_calling_again() {
    let Some(env) = setup().await else { return };
    let run = env.run(llm_node(), &[]).await;
    let gate = Arc::new(Gate::default());
    *env.ports.gate.lock().unwrap() = Some(Arc::clone(&gate));
    let dead = env.claim_as_dead_worker().await;
    let ctx = ctx_of(&dead, &[]);
    let exec = Arc::clone(&env.exec);
    let a = tokio::spawn(async move {
        exec.execute("llm.invoke", &llm_node()["params"], &ctx)
            .await
    });
    gate.entered.notified().await;

    // 前の実行が呼んでいるあいだに拾い直した実行は、呼ばずに待つ。
    env.reclaim_and_run_once().await;
    let waiting = env.step(run).await;
    let calls_while_waiting = env.ports.llm_calls.load(Ordering::SeqCst);

    // 前の実行の呼び出しが戻って記録したら、待っていた実行はその答えを使う。
    gate.release.notify_one();
    let a_result = a.await.unwrap();
    sqlx::query("UPDATE step_execution SET next_retry_at = now() WHERE tenant_id = $1")
        .bind(&env.tenant)
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(env.worker().claim_and_run_once("w-b").await, Ok(true));
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let calls = env.ports.llm_calls.load(Ordering::SeqCst);
    env.cleanup().await;

    assert_eq!(
        (waiting.0.as_str(), waiting.1, waiting.2.as_deref()),
        ("ready", 0, Some("effect_in_progress")),
        "試す回数を使わずに待つ"
    );
    assert_eq!(calls_while_waiting, 1);
    assert!(a_result.ok);
    assert_eq!(calls, 1, "AI を 2 回呼んだ");
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(step.4, a_result.output);
}

/// AI を呼んでいる途中でワーカーごと落ちたら（記録が付かない）、拾い直した実行は回収の猶予まで待ってから呼び直す
/// （外には何も起きないので、止めずに続ける）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_llm_call_lost_with_its_worker_is_made_again_after_the_reclaim_window() {
    let Some(env) = setup().await else { return };
    let run = env.run(llm_node(), &[]).await;
    let gate = Arc::new(Gate::default());
    *env.ports.gate.lock().unwrap() = Some(Arc::clone(&gate));
    let dead = env.claim_as_dead_worker().await;
    let ctx = ctx_of(&dead, &[]);
    // ワーカーごと落ちた形: 呼び出しの途中で未来値を捨てずに置き去りにする（落とした後片付けが走らない）。
    let node = llm_node();
    let mut a = Box::pin(env.exec.execute("llm.invoke", &node["params"], &ctx));
    tokio::select! {
        _ = &mut a => panic!("止めた呼び出しが戻った"),
        () = gate.entered.notified() => {}
    }
    std::mem::forget(a);

    env.reclaim_and_run_once().await;
    let waiting = env.step(run).await;
    env.age_reservations_and_wake().await;
    assert_eq!(env.worker().claim_and_run_once("w-b").await, Ok(true));
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let calls = env.ports.llm_calls.load(Ordering::SeqCst);
    env.cleanup().await;

    assert_eq!(waiting.2.as_deref(), Some("effect_in_progress"));
    assert_eq!(calls, 2, "落ちた呼び出しの後に 1 回だけ呼び直す");
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(step.4["text"], json!("答え 2"));
}

/// 外部へ送ったあと、checkpoint の前にワーカーが落ちても、拾い直した実行は送り直さず、記録した応答を使う。
#[tokio::test]
async fn a_reclaimed_post_is_not_sent_twice() {
    let Some(env) = setup().await else { return };
    let run = env.run(post_node(), &["http.egress"]).await;
    let dead = env.claim_as_dead_worker().await;
    let first = env
        .exec
        .execute(
            "http.request",
            &post_node()["params"],
            &ctx_of(&dead, &["http.egress"]),
        )
        .await;
    assert!(first.ok, "{:?}", first.error);
    env.reclaim_and_run_once().await;
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends = env.ports.sends();
    let rows = env.journal_rows().await;
    env.cleanup().await;

    assert_eq!(sends, 1, "拾い直しで送り直した（二重送信）");
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(step.4, first.output);
    assert_eq!(step.4["body"], json!({ "ok": true, "n": 1 }));
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].1.as_ref(),
        Some(&first.output),
        "ブロックの結果を記録する"
    );
}

/// 送っている途中でワーカーごと落ちたら、届いたかが分からないので、拾い直した実行は送り直さずに止まる。再開すると
/// 送る（送り直すと決めたのは再開した人）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_post_lost_with_its_worker_stops_as_unknown_and_resuming_sends_it_again() {
    let Some(env) = setup().await else { return };
    let run = env.run(post_node(), &["http.egress"]).await;
    let gate = Arc::new(Gate::default());
    *env.ports.gate.lock().unwrap() = Some(Arc::clone(&gate));
    let dead = env.claim_as_dead_worker().await;
    let ctx = ctx_of(&dead, &["http.egress"]);
    let node = post_node();
    let mut a = Box::pin(env.exec.execute("http.request", &node["params"], &ctx));
    tokio::select! {
        _ = &mut a => panic!("止めた送信が戻った"),
        () = gate.entered.notified() => {}
    }
    std::mem::forget(a);

    env.reclaim_and_run_once().await;
    let waiting = env.step(run).await;
    env.age_reservations_and_wake().await;
    assert_eq!(env.worker().claim_and_run_once("w-b").await, Ok(true));
    let stopped = env.step(run).await;
    let stopped_status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends_before_resume = env.ports.sends();
    let rows_after_stop = env.journal_rows().await;

    let workflow_id: Uuid =
        sqlx::query_scalar("SELECT workflow_id FROM workflow_run WHERE tenant_id = $1")
            .bind(&env.tenant)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    let resumed = env
        .store
        .resume_failed(&env.tenant, workflow_id, run)
        .await
        .unwrap();
    assert_eq!(env.worker().claim_and_run_once("w-c").await, Ok(true));
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends = env.ports.sends();
    env.cleanup().await;

    assert_eq!(waiting.2.as_deref(), Some("effect_in_progress"));
    assert_eq!(sends_before_resume, 1, "届いたか分からない送信を送り直した");
    assert_eq!(stopped.0, "failed");
    assert_eq!(stopped.3.as_deref(), Some(HTTP_DELIVERY_UNKNOWN));
    assert_eq!(stopped_status, Some(RunStatus::Failed));
    assert!(rows_after_stop.is_empty(), "予約は消す（再開が送れる）");
    assert_eq!(resumed, ResumeOutcome::Resumed);
    assert_eq!(sends, 2, "再開すると 1 回送る");
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(step.4["body"], json!({ "ok": true, "n": 2 }));
}

/// 読むだけの送信（GET）は記録しない。拾い直すと送り直す（送り先の状態は変わらない）。
#[tokio::test]
async fn a_reclaimed_get_is_sent_again_without_a_record() {
    let Some(env) = setup().await else { return };
    let get = json!({ "id": "get", "type": "http.request", "params": {
        "method": "GET", "url": "https://api.example.com/status" } });
    let run = env.run(get.clone(), &["http.egress"]).await;
    let dead = env.claim_as_dead_worker().await;
    let first = env
        .exec
        .execute(
            "http.request",
            &get["params"],
            &ctx_of(&dead, &["http.egress"]),
        )
        .await;
    assert!(first.ok, "{:?}", first.error);
    env.reclaim_and_run_once().await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends = env.ports.sends();
    let rows = env.journal_rows().await;
    env.cleanup().await;

    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(sends, 2);
    assert!(rows.is_empty(), "GET は記録しない: {rows:?}");
}

/// 相手が誤りを返した応答（ブロックは成功として次へ渡す）は記録しない。受け付けられなかった送信なので、拾い直しで
/// 送り直す（script が誤りの応答を見て投げ、やり直しで送り直す使い方を保つ）。
#[tokio::test]
async fn an_error_response_is_not_recorded_and_is_sent_again() {
    let Some(env) = setup().await else { return };
    *env.ports.http_status.lock().unwrap() = Some(503);
    let run = env.run(post_node(), &["http.egress"]).await;
    let dead = env.claim_as_dead_worker().await;
    let first = env
        .exec
        .execute(
            "http.request",
            &post_node()["params"],
            &ctx_of(&dead, &["http.egress"]),
        )
        .await;
    let rows_after_send = env.journal_rows().await;
    env.reclaim_and_run_once().await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends = env.ports.sends();
    env.cleanup().await;

    assert!(first.ok, "{:?}", first.error);
    assert_eq!(first.output["status"], json!(503));
    assert!(rows_after_send.is_empty(), "{rows_after_send:?}");
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(sends, 2);
}

/// 応答が大きすぎて本文を記録しなかった送信は、拾い直したら送り直さずに止まる（届いたことは分かるが、続きに渡す本文が
/// 無い）。記録は消すので、再開すれば送る。
#[tokio::test]
async fn a_reclaimed_post_with_a_large_response_stops_without_sending_again() {
    let Some(env) = setup().await else { return };
    *env.ports.http_body.lock().unwrap() = Some(vec![b'x'; RECORD_MAX_BYTES + 1]);
    let run = env.run(post_node(), &["http.egress"]).await;
    let dead = env.claim_as_dead_worker().await;
    let first = env
        .exec
        .execute(
            "http.request",
            &post_node()["params"],
            &ctx_of(&dead, &["http.egress"]),
        )
        .await;
    assert!(first.ok, "{:?}", first.error);
    let rows_after_send = env.journal_rows().await;
    env.reclaim_and_run_once().await;
    let step = env.step(run).await;
    let sends = env.ports.sends();
    let rows = env.journal_rows().await;
    env.cleanup().await;

    assert_eq!(
        rows_after_send[0].1,
        Some(json!({ "status": 201, "host": "api.example.com", "body_omitted": true })),
        "本文を落とした印だけを記録する"
    );
    assert_eq!(sends, 1);
    assert_eq!(
        (step.0.as_str(), step.3.as_deref()),
        ("failed", Some(HTTP_DELIVERY_UNKNOWN))
    );
    assert!(rows.is_empty());
}

/// 時間の上限で打ち切って呼び出しを捨てたら、予約を外す（やり直しは待たずに送り直す・作成者がやり直しを選んだ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_cut_by_the_time_limit_leaves_no_reservation() {
    let Some(env) = setup().await else { return };
    let run = env.run(post_node(), &["http.egress"]).await;
    let gate = Arc::new(Gate::default());
    *env.ports.gate.lock().unwrap() = Some(Arc::clone(&gate));
    let dead = env.claim_as_dead_worker().await;
    let ctx = ctx_of(&dead, &["http.egress"]);
    let cut = tokio::time::timeout(
        Duration::from_millis(300),
        env.exec
            .execute("http.request", &post_node()["params"], &ctx),
    )
    .await;
    assert!(cut.is_err(), "打ち切られる");
    // 予約を外す処理は別のタスクで走る。
    let mut rows = env.journal_rows().await;
    for _ in 0..50 {
        if rows.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        rows = env.journal_rows().await;
    }
    gate.release.notify_one();
    let _ = run;
    env.cleanup().await;

    assert!(
        rows.is_empty(),
        "打ち切った呼び出しの予約が残った: {rows:?}"
    );
}

/// 送り先の状態を変える `Shiki.http.request` を 2 回送る script。
const POST_TWICE: &str = r#"function main(input) {
  const a = Shiki.http.request({ method: "POST", url: "https://api.example.com/a", body: { n: 1 } });
  const b = Shiki.http.request({ method: "POST", url: "https://api.example.com/b", body: { n: 2 } });
  return { a: a.body, b: b.body };
}"#;

/// script の `Shiki.http.request` も、呼び出しごとに記録し、script を動かし直したときに送り直さない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reclaimed_script_does_not_send_its_posts_twice() {
    let Some(env) = setup_with(true).await else {
        return;
    };
    let node = json!({ "id": "sc", "type": "script.run", "params": { "source": { "inline": POST_TWICE } } });
    let run = env.run(node.clone(), &["http.egress"]).await;
    let dead = env.claim_as_dead_worker().await;
    let first = env
        .exec
        .execute(
            "script.run",
            &node["params"],
            &ctx_of(&dead, &["http.egress"]),
        )
        .await;
    assert!(first.ok, "{:?}", first.error);
    env.reclaim_and_run_once().await;
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends = env.ports.sends.lock().unwrap().clone();
    let rows = env.journal_rows().await;
    env.cleanup().await;

    assert_eq!(
        sends,
        [
            "POST https://api.example.com/a",
            "POST https://api.example.com/b"
        ],
        "script を動かし直して送り直した"
    );
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(step.4, first.output);
    assert_eq!(rows.len(), 2, "呼び出しごとに記録する: {rows:?}");
    assert!(rows.iter().all(|(k, _)| k.contains("#c")), "{rows:?}");
}

/// 記録したキーが、試す回数ごとに分かれていること（script の外部への送信・`#a<attempt>#c<seq>`）。
fn attempts_of(rows: &[(String, Option<Value>)]) -> Vec<String> {
    rows.iter()
        .map(|(k, _)| {
            k.rsplit_once('#')
                .and_then(|(head, _)| head.rsplit_once('#'))
                .map_or_else(String::new, |(_, a)| a.to_string())
        })
        .collect()
}

/// 1 つ目の送信は通り、2 つ目の送信が一時的な誤りで失敗する script（`/down` は一度だけ失敗させる）。
fn post_then_down(first_body: &str) -> Value {
    let source = format!(
        r#"function main(input) {{
  const a = Shiki.http.request({{ method: "POST", url: "https://api.example.com/a", body: {first_body} }});
  const b = Shiki.http.request({{ method: "POST", url: "https://api.example.com/down", body: {{ n: 2 }} }});
  return {{ a: a.body, b: b.body }};
}}"#
    );
    json!({ "id": "sc", "type": "script.run", "params": { "source": { "inline": source } } })
}

/// 2xx の応答の後に script が一時的な誤りで失敗してから再開すると、前の試行の送信も送り直し、新しい応答を使う
/// （記録は拾い直しの間だけ。応答の中身を見て失敗を判断する script が、原因を直した再開で通る）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resuming_a_script_sends_again_and_uses_the_new_response() {
    let Some(env) = setup_with(true).await else {
        return;
    };
    let node = post_then_down("{ n: 1 }");
    env.ports
        .fail_once
        .lock()
        .unwrap()
        .push("https://api.example.com/down".into());
    let run = env.run(node, &["http.egress"]).await;
    let first_run = env.worker().claim_and_run_once("w-a").await;
    let failed = env.step(run).await;
    let resumed = env
        .store
        .resume_failed(&env.tenant, env.workflow_id().await, run)
        .await
        .unwrap();
    let ran = [first_run, env.worker().claim_and_run_once("w-b").await];
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends = env.ports.sends.lock().unwrap().clone();
    let rows = env.journal_rows().await;
    env.cleanup().await;

    assert_eq!(ran, [Ok(true), Ok(true)]);
    assert_eq!(
        (failed.0.as_str(), failed.3.as_deref()),
        ("failed", Some("unavailable"))
    );
    assert_eq!(resumed, ResumeOutcome::Resumed);
    assert_eq!(
        sends,
        [
            "POST https://api.example.com/a",
            "POST https://api.example.com/down",
            "POST https://api.example.com/a",
            "POST https://api.example.com/down",
        ],
        "再開で前の試行の送信を送り直していない（記録した古い応答を返した）"
    );
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(
        step.4["a"],
        json!({ "ok": true, "n": 3 }),
        "再開した試行の応答を使う"
    );
    assert_eq!(attempts_of(&rows), ["a1", "a2", "a2"], "{rows:?}");
}

/// 本文に時刻を入れる script も、やり直しで effect_conflict にならずに送り直して通る（記録は試す回数ごと）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrying_a_script_with_a_timestamp_in_the_body_does_not_conflict() {
    let Some(env) = setup_with(true).await else {
        return;
    };
    let mut node = post_then_down("{ at: Date.now() + Math.random() }");
    node["retry"] = json!({ "max_attempts": 2 });
    env.ports
        .fail_once
        .lock()
        .unwrap()
        .push("https://api.example.com/down".into());
    let run = env.run(node, &["http.egress"]).await;
    let first_run = env.worker().claim_and_run_once("w-a").await;
    let retrying = env.step(run).await;
    env.age_reservations_and_wake().await;
    let ran = [first_run, env.worker().claim_and_run_once("w-b").await];
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends = env.ports.sends();
    env.cleanup().await;

    assert_eq!(ran, [Ok(true), Ok(true)]);
    assert_eq!((retrying.0.as_str(), retrying.1), ("ready", 1));
    assert_eq!(
        (step.0.as_str(), step.3.as_deref()),
        ("succeeded", None),
        "やり直しで前と違う本文を effect_conflict にした"
    );
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(sends, 4);
}

/// 応答が大きくて本文を落として記録した送信は、試す回数が残っていても、拾い直しの後にやり直さない（届いたと
/// 分かっているので、すぐにもう一度届く）。再開すれば送る。
#[tokio::test]
async fn a_large_response_is_not_sent_again_by_the_remaining_attempts() {
    let Some(env) = setup().await else { return };
    *env.ports.http_body.lock().unwrap() = Some(vec![b'x'; RECORD_MAX_BYTES + 1]);
    let mut node = post_node();
    node["retry"] = json!({ "max_attempts": 3 });
    let run = env.run(node.clone(), &["http.egress"]).await;
    let dead = env.claim_as_dead_worker().await;
    let first = env
        .exec
        .execute(
            "http.request",
            &node["params"],
            &ctx_of(&dead, &["http.egress"]),
        )
        .await;
    let reclaimed = env.reclaim_and_run_once_unchecked().await;
    let stopped = env.step(run).await;
    // やり直しが待っていれば、ここで拾われて送り直す。
    env.age_reservations_and_wake().await;
    let ran_again = env.worker().claim_and_run_once("w-c").await;
    let sends = env.ports.sends();
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    env.cleanup().await;

    assert!(first.ok, "{:?}", first.error);
    assert_eq!(reclaimed, (1, Ok(true)));
    assert_eq!(
        (stopped.0.as_str(), stopped.3.as_deref()),
        ("failed", Some(HTTP_DELIVERY_UNKNOWN)),
        "試す回数が残っているのでやり直しに回した"
    );
    assert_eq!(ran_again, Ok(false));
    assert_eq!(sends, 1, "届いた送信をやり直しでもう一度送った");
    assert_eq!(status, Some(RunStatus::Failed));
}

/// 送信を try/catch で包む script。前の実行の予約が残っていて誤りを受けたら、別の送り先へ送って成功で終わろうとする。
const CATCHES_IN_PROGRESS: &str = r#"function main(input) {
  try {
    const r = Shiki.http.request({ method: "POST", url: "https://api.example.com/a", body: { n: 1 } });
    return { sent: r.body };
  } catch (e) {
    try { Shiki.http.request({ method: "POST", url: "https://api.example.com/b", body: { n: 2 } }); } catch (_) {}
    return { skipped: true };
  }
}"#;

/// 前の実行が送っている途中の送信の誤り（effect_in_progress）を script が捕まえても、step は成功せずに待ち、後の
/// `Shiki.*` も送らない。前の実行が記録したら、その応答で続ける（送信は 1 回だけ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_script_that_catches_an_in_progress_send_still_waits() {
    let Some(env) = setup_with(true).await else {
        return;
    };
    let node = json!({ "id": "sc", "type": "script.run",
        "params": { "source": { "inline": CATCHES_IN_PROGRESS } } });
    let run = env.run(node.clone(), &["http.egress"]).await;
    let gate = Arc::new(Gate::default());
    *env.ports.gate.lock().unwrap() = Some(Arc::clone(&gate));
    let dead = env.claim_as_dead_worker().await;
    let ctx = ctx_of(&dead, &["http.egress"]);
    let exec = Arc::clone(&env.exec);
    let params = node["params"].clone();
    let a = tokio::spawn(async move { exec.execute("script.run", &params, &ctx).await });
    gate.entered.notified().await;

    let reclaimed = env.reclaim_and_run_once_unchecked().await;
    let waiting = env.step(run).await;
    let sends_while_waiting = env.ports.sends.lock().unwrap().clone();

    gate.release.notify_one();
    let a_result = a.await.unwrap();
    sqlx::query("UPDATE step_execution SET next_retry_at = now() WHERE tenant_id = $1")
        .bind(&env.tenant)
        .execute(&env.pool)
        .await
        .unwrap();
    let ran_after = env.worker().claim_and_run_once("w-c").await;
    let step = env.step(run).await;
    let status = env.store.run_status(&env.tenant, run).await.unwrap();
    let sends = env.ports.sends();
    env.cleanup().await;

    assert_eq!(reclaimed, (1, Ok(true)));
    assert_eq!(
        (waiting.0.as_str(), waiting.1, waiting.2.as_deref()),
        ("ready", 0, Some("effect_in_progress")),
        "捕まえた script が待たずに終わった"
    );
    assert_eq!(ran_after, Ok(true));
    assert_eq!(
        sends_while_waiting,
        ["POST https://api.example.com/a"],
        "誤りを受けた後の送信を通した"
    );
    assert!(a_result.ok, "{:?}", a_result.error);
    assert_eq!(status, Some(RunStatus::Succeeded));
    assert_eq!(step.4, json!({ "sent": { "ok": true, "n": 1 } }));
    assert_eq!(sends, 1);
}
