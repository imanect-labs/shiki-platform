//! 承認待ち（human.approval・GAP-08）の engine 層の検証。
//!
//! 純ロジック（DB 不要）: 停止まで（行の状態・rev・FSM で進める状態・担当者の解決・決まらない承認の失敗）。
//! DB 必要（`STORAGE_TEST_DATABASE_URL`）: 停止で台帳・担当者・購読が書かれる／直前の自分の遷移（rev が停止時
//! 以下）では起きない／判断の遷移で出口へ進み、台帳が同じ Tx で確定してコメントが拾われる／判断の外の遷移は
//! 取り下げ／期限切れは timeout 出口と台帳の expired／run の失敗で台帳が cancelled になり受信箱から消える／
//! 受信箱は担当者（本人・所属ロール）だけ／受付の突合の材料と印。
//!
//! 行の見え方（実行主体・承認者本人）と FSM の遷移は data 側で効き、api の `workflow_approval_it` が実 PG ＋
//! 実 OpenFGA で確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::print_stderr
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::run::graph::RunGraph;
use workflow_engine::run::Suspend;
use workflow_engine::{
    ApprovalStore, ApprovalTarget, AssigneeKind, CapabilityAudit, CapabilityNodeExecutor,
    EffectJournal, NodeContext, NodeExecutor, RunStatus, RunStore, StepStatus, WorkerConfig,
    WorkflowWorker,
};

const TABLE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";

/// 承認の停止で読む行だけを返す fake（状態・rev・進める状態・担当者の列）。
struct ApprovalPorts {
    target: Mutex<ApprovalTarget>,
}

impl ApprovalPorts {
    fn new(current: &str, rev: i64, targets: &[&str]) -> Self {
        ApprovalPorts {
            target: Mutex::new(ApprovalTarget {
                current: Some(current.into()),
                rev,
                targets: targets.iter().map(ToString::to_string).collect(),
                refs: BTreeMap::from([
                    (
                        "approver".to_string(),
                        (AssigneeKind::User, Some("bob".to_string())),
                    ),
                    ("deputy".to_string(), (AssigneeKind::User, None)),
                ]),
            }),
        }
    }
}

#[async_trait]
impl NodePorts for ApprovalPorts {
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
    async fn data_approval_target(
        &self,
        _c: &ExecCtx,
        table_id: Uuid,
        _record_id: Uuid,
    ) -> Result<ApprovalTarget, PortError> {
        assert_eq!(table_id.to_string(), TABLE);
        Ok(self.target.lock().unwrap().clone())
    }
}

struct NoopAudit;
impl CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
}

fn lazy_journal() -> EffectJournal {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    EffectJournal::new(pool)
}

fn approval_params(timeout: Option<u32>) -> Value {
    let mut p = json!({
        "title": "経費申請の承認",
        "instructions": "規程どおりか確認してください。",
        "record": { "table": TABLE, "id": { "$from": "trigger", "path": "/record_id" } },
        "assignees": [{ "field": "approver" }, { "field": "deputy" }, { "role": "keiri" }],
        "approve": { "to": "approved" },
        "reject": { "to": "rejected", "comment": "required" },
        "show_fields": ["title", "amount"]
    });
    if let Some(t) = timeout {
        p["timeout_sec"] = json!(t);
        p["on_timeout"] = json!("continue");
    }
    p
}

fn ctx(record: Uuid) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        step_path: "ok".into(),
        idempotency_key: "wf:t1:r:ok".into(),
        attempt: 1,
        principal: Uuid::nil().to_string(),
        principal_kind: "workflow".into(),
        input: json!({ "record_id": record }),
        trigger: json!({ "record_id": record }),
        node_outputs: Value::Null,
        each: None,
        trace_id: None,
        scope_ceiling: vec!["data.read".into(), "notify.send".into()],
        cancel: workflow_engine::run::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
    }
}

/// 停止まで: 行の状態と rev を残し、担当者は列の値（空の列は数えない）とロールで解決する。
#[tokio::test]
async fn approval_suspends_with_the_row_state_and_resolved_assignees() {
    let ports = Arc::new(ApprovalPorts::new(
        "submitted",
        4,
        &["approved", "rejected"],
    ));
    let exec = CapabilityNodeExecutor::new(ports.clone(), lazy_journal(), Arc::new(NoopAudit));
    let record = Uuid::new_v4();
    let res = exec
        .execute("human.approval", &approval_params(None), &ctx(record))
        .await;
    assert!(res.ok, "{:?}", res.error);
    let Some(Suspend::Approval(a)) = res.suspend else {
        panic!("承認で止まる: {res:?}");
    };
    assert_eq!((a.from_state.as_str(), a.suspend_rev), ("submitted", 4));
    assert_eq!(a.record_id, record);
    let who: Vec<(AssigneeKind, &str)> = a
        .assignees
        .iter()
        .map(|x| (x.kind, x.subject.as_str()))
        .collect();
    assert_eq!(
        who,
        [(AssigneeKind::User, "bob"), (AssigneeKind::Role, "keiri")]
    );
    assert_eq!(a.choices[0].label, "承認する", "既定の文言");
    assert_eq!(
        a.approval_id,
        workflow_engine::approval_id_for("t1", "wf:t1:r:ok")
    );

    // FSM にいまの状態から判断の行き先への遷移が無ければ、待っても決まらない（permanent）。
    ports.target.lock().unwrap().targets = vec!["approved".into()];
    let res = exec
        .execute("human.approval", &approval_params(None), &ctx(record))
        .await;
    let e = res.error.unwrap();
    assert_eq!(e.code, "approval_not_decidable");
    assert!(!e.retryable);
    assert!(e.message.contains("rejected"), "{}", e.message);

    // 担当者の列が読めなければ失敗（黙って外さない）。担当者が誰もいなければ失敗。
    let mut p = approval_params(None);
    p["assignees"] = json!([{ "field": "secret_approver" }]);
    *ports.target.lock().unwrap() = ApprovalPorts::new("submitted", 4, &["approved", "rejected"])
        .target
        .into_inner()
        .unwrap();
    let e = exec
        .execute("human.approval", &p, &ctx(record))
        .await
        .error
        .unwrap();
    assert_eq!(e.code, "approval_assignee_unreadable");
    p["assignees"] = json!([{ "field": "deputy" }]);
    let e = exec
        .execute("human.approval", &p, &ctx(record))
        .await
        .error
        .unwrap();
    assert_eq!(e.code, "approval_no_assignee");

    // data.read の天井が無ければ行を読まない。
    let mut c = ctx(record);
    c.scope_ceiling = vec!["notify.send".into()];
    let e = exec
        .execute("human.approval", &approval_params(None), &c)
        .await
        .error
        .unwrap();
    assert_eq!(e.code, "out_of_scope");
}

// ---- DB ----

/// 判断 API が受け付けた承認の判断（`who` が rev `rev` へ approved に進めた）。
fn accepted(who: &str, rev: i64) -> workflow_engine::AcceptedDecision {
    workflow_engine::AcceptedDecision {
        kind: workflow_engine::ir::params::ApprovalDecisionKind::Approve,
        to: "approved".into(),
        by: who.into(),
        rev,
        at: chrono::Utc::now(),
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

/// `ok`(承認) → approved: `after_ok`・rejected: `after_ng`・withdrawn: `after_wd`・timeout: `after_to`。
fn approval_ir(timeout: Option<u32>) -> Value {
    let mut edges = vec![
        json!({ "from": "ok", "from_port": "approved", "to": "after_ok" }),
        json!({ "from": "ok", "from_port": "rejected", "to": "after_ng" }),
        json!({ "from": "ok", "from_port": "withdrawn", "to": "after_wd" }),
    ];
    let mut nodes = vec![
        json!({ "id": "ok", "type": "human.approval", "params": approval_params(timeout) }),
        json!({ "id": "after_ok", "type": "control.join", "params": {} }),
        json!({ "id": "after_ng", "type": "control.join", "params": {} }),
        json!({ "id": "after_wd", "type": "control.join", "params": {} }),
    ];
    if timeout.is_some() {
        nodes.push(json!({ "id": "after_to", "type": "control.join", "params": {} }));
        edges.push(json!({ "from": "ok", "from_port": "timeout", "to": "after_to" }));
    }
    json!({
        "ir_version": 1, "name": "expense",
        "declared_scopes": ["data.read", "notify.send"],
        "nodes": nodes, "edges": edges,
        "policies": { "run_timeout_sec": 1_209_600 }
    })
}

struct Env {
    pool: PgPool,
    runs: RunStore,
    inbox: ApprovalStore,
    tenant: String,
    record: Uuid,
    workflow: Uuid,
}

impl Env {
    async fn new(pool: PgPool) -> Self {
        Env {
            runs: RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS),
            inbox: ApprovalStore::new(pool.clone()),
            pool,
            tenant: format!("t-{}", Uuid::new_v4()),
            record: Uuid::new_v4(),
            workflow: Uuid::new_v4(),
        }
    }

    /// 承認で止まるまで run を進める（停止時の行は rev 4・状態 submitted）。
    async fn start(&self, ir: &Value) -> Uuid {
        self.start_by(ir, &Uuid::nil().to_string(), "workflow", "event")
            .await
    }

    /// [`Self::start`] を、起動した主体ときっかけを決めて行う（本人が手で起動した run の結果を読むテスト用）。
    async fn start_by(&self, ir: &Value, principal: &str, kind: &str, trigger: &str) -> Uuid {
        workflow_engine::WorkflowIr::from_json(ir).unwrap();
        let parsed = workflow_engine::validate(ir, &workflow_engine::Catalog::default()).unwrap();
        let graph = RunGraph::build(&parsed);
        let run_id = self
            .runs
            .create_run(
                &self.tenant,
                "acme",
                self.workflow,
                1,
                trigger,
                None,
                principal,
                kind,
                &json!({ "record_id": self.record }),
                ir,
                &graph,
            )
            .await
            .unwrap()
            .unwrap();
        self.drain().await;
        run_id
    }

    /// ready の step が尽きるまでワーカーを回す（停止時の行は rev 4・状態 submitted）。
    async fn drain(&self) {
        let ports = Arc::new(ApprovalPorts::new(
            "submitted",
            4,
            &["approved", "rejected"],
        ));
        let exec: Arc<dyn NodeExecutor> = Arc::new(CapabilityNodeExecutor::new(
            ports,
            EffectJournal::new(self.pool.clone()),
            Arc::new(NoopAudit),
        ));
        let w = WorkflowWorker::new(self.runs.clone(), exec, WorkerConfig::default())
            .scoped_to_tenant(&self.tenant);
        while w.claim_and_run_once("w1").await.unwrap() {}
    }

    async fn step(&self, run: Uuid, node: &str) -> (StepStatus, Vec<String>, Option<Value>) {
        let (s, ports, out): (String, Vec<String>, Option<sqlx::types::Json<Value>>) =
            sqlx::query_as(
                "SELECT status, taken_ports, output FROM step_execution \
                 WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3",
            )
            .bind(&self.tenant)
            .bind(run)
            .bind(node)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        (StepStatus::parse(&s).unwrap(), ports, out.map(|j| j.0))
    }

    async fn ledger(&self, run: Uuid) -> (String, Option<String>, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT status, decided_by, decided_to, comment FROM workflow_approval \
             WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&self.tenant)
        .bind(run)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    fn table(&self) -> Uuid {
        Uuid::parse_str(TABLE).unwrap()
    }

    /// その行の遷移を待っている承認の候補（relay と同じ口）。
    async fn candidate(&self) -> workflow_engine::DataWaitCandidate {
        let mut c = self
            .runs
            .data_wait_candidates(&self.tenant, &[self.table()])
            .await
            .unwrap();
        assert_eq!(c.len(), 1, "承認の購読が 1 件");
        c.remove(0)
    }

    /// 判断 API と同じ口で、承認のコメント（承認の判断）を置いて確定する。
    async fn comment(&self, approval: Uuid, rev: i64, who: &str, body: &str) {
        let mut d = self.open_decision(approval).await;
        d.put_comment(
            rev,
            who,
            workflow_engine::ir::params::ApprovalDecisionKind::Approve,
            body,
        )
        .await
        .unwrap();
        d.accept(&accepted(who, rev)).await.unwrap();
    }

    /// 判断の受付を始める（台帳の行をロックする）。
    async fn open_decision(&self, approval: Uuid) -> workflow_engine::DecisionTx {
        match self
            .inbox
            .begin_decision(&self.tenant, approval)
            .await
            .unwrap()
        {
            workflow_engine::DecisionStart::Open(d) => d,
            _ => panic!("判断を受け付けられる"),
        }
    }

    /// テストで作った run を消す（台帳・step は run と一緒に消える。購読は外部キーが無いので先に消す）。
    async fn forget_run(&self, run: Uuid) {
        for sql in [
            "DELETE FROM wait_subscription WHERE tenant_id = $1 AND run_id = $2",
            "DELETE FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
        ] {
            sqlx::query(sql)
                .bind(&self.tenant)
                .bind(run)
                .execute(&self.pool)
                .await
                .unwrap();
        }
    }

    async fn comments(&self, approval: Uuid) -> Vec<(i64, String, String)> {
        sqlx::query_as(
            "SELECT rev, author, body FROM workflow_approval_comment \
             WHERE tenant_id = $1 AND approval_id = $2 ORDER BY author",
        )
        .bind(&self.tenant)
        .bind(approval)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    fn transition(&self, to: &str, actor: &str, rev: i64) -> Value {
        json!({
            "source": "data.transition", "table_id": TABLE, "record_id": self.record, "rev": rev,
            "change": "transition", "from": "submitted", "to": to, "changed": ["status"],
            "changes": {}, "actor": actor, "by_workflow": false,
            "occurred_at": "2026-10-26T01:02:03Z", "record": null, "truncated": false
        })
    }
}

/// 停止で台帳・担当者・購読が書かれ、受信箱には担当者（本人・所属ロール）にだけ出る。直前の自分の遷移
/// （停止時の rev 以下）では起きず、判断の遷移で approved へ進み、台帳とコメントが同じ Tx で確定する。
#[tokio::test]
async fn approval_waits_for_the_decision_and_settles_the_ledger() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool).await;
    let run = env.start(&approval_ir(None)).await;
    assert_eq!(env.step(run, "ok").await.0, StepStatus::WaitingEvent);
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Running)
    );
    let (status, ..) = env.ledger(run).await;
    assert_eq!(status, "pending");

    // 受信箱: bob（列の担当者）とロール keiri の人に出る。担当でない人には出ない。
    let bob = env
        .inbox
        .inbox(&env.tenant, "acme", "bob", &[], None, 50)
        .await
        .unwrap();
    assert_eq!(bob.len(), 1);
    let item = &bob[0];
    assert_eq!(
        (item.from_state.as_str(), item.suspend_rev),
        ("submitted", 4)
    );
    assert_eq!(item.show_fields, ["title", "amount"]);
    assert!(item.decidable);
    let keiri = env
        .inbox
        .inbox(&env.tenant, "acme", "carol", &["keiri".into()], None, 50)
        .await
        .unwrap();
    assert_eq!(keiri.len(), 1);
    for (who, roles) in [("carol", vec![]), ("dave", vec!["sales".to_string()])] {
        assert!(env
            .inbox
            .inbox(&env.tenant, "acme", who, &roles, None, 50)
            .await
            .unwrap()
            .is_empty());
        assert!(env
            .inbox
            .get_assigned(&env.tenant, "acme", who, &roles, item.approval_id)
            .await
            .unwrap()
            .is_none());
    }
    assert!(env
        .inbox
        .inbox(&env.tenant, "other-org", "bob", &[], None, 50)
        .await
        .unwrap()
        .is_empty());

    // 受付の突合の材料に出て、印を付けると出ない。
    let todo = env.runs.approvals_to_reconcile(1000).await.unwrap();
    let mine = todo
        .iter()
        .find(|p| p.wait.approval_id == item.approval_id)
        .unwrap();
    assert_eq!(
        (mine.from_state.as_str(), mine.suspend_rev, mine.assignees),
        ("submitted", 4, 2)
    );
    env.runs
        .mark_approval_reconciled(&env.tenant, item.approval_id)
        .await
        .unwrap();
    let todo = env.runs.approvals_to_reconcile(1000).await.unwrap();
    assert!(todo.iter().all(|p| p.wait.approval_id != item.approval_id));

    // 直前の自分の遷移（停止時の rev 以下）では起きない。
    let c = env.candidate().await;
    assert!(c.approval().is_some());
    assert!(!c.wants(
        workflow_engine::vocab::EventSource::DataTransition,
        env.table(),
        env.record,
        4,
        i64::MAX
    ));
    assert!(c.wants(
        workflow_engine::vocab::EventSource::DataTransition,
        env.table(),
        env.record,
        5,
        i64::MAX
    ));

    // bob が受信箱からコメントを添えて承認した（遷移の後の rev 5）。別の人（carol）が同じ rev に置き残した
    // コメントがあっても置ける（鍵に作者を含む・置き残しが別の人の判断を塞がない・BUG-47）。起床で採るのは
    // 遷移した人のものだけ。
    sqlx::query(
        "INSERT INTO workflow_approval_comment (tenant_id, approval_id, rev, author, decision, body) \
         VALUES ($1, $2, 5, 'carol', 'approve', '横入り')",
    )
    .bind(&env.tenant)
    .bind(item.approval_id)
    .execute(&env.pool)
    .await
    .unwrap();
    env.comment(item.approval_id, 5, "bob", "規程どおりです")
        .await;
    assert!(env
        .runs
        .wake_data_wait(&c, &env.transition("approved", "bob", 5))
        .await
        .unwrap());
    // 二度目の起床（再配送）は何もしない。
    assert!(!env
        .runs
        .wake_data_wait(&c, &env.transition("approved", "bob", 5))
        .await
        .unwrap());

    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st, ports),
        (StepStatus::Succeeded, vec!["approved".to_string()])
    );
    let out = out.unwrap();
    assert_eq!(out["decision"], "approve");
    assert_eq!(out["to"], "approved");
    assert_eq!(out["decided_by"], "bob");
    assert_eq!(out["comment"], "規程どおりです");
    assert_eq!(out["rev"], 5);
    assert_eq!(out["record_id"], json!(env.record));
    assert_eq!(
        env.ledger(run).await,
        (
            "approved".into(),
            Some("bob".into()),
            Some("approved".into()),
            Some("規程どおりです".into())
        )
    );
    env.drain().await;
    assert_eq!(env.step(run, "after_ok").await.0, StepStatus::Succeeded);
    assert_eq!(env.step(run, "after_ng").await.0, StepStatus::Skipped);
    assert_eq!(env.step(run, "after_wd").await.0, StepStatus::Skipped);
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    // 判断の後は受信箱に出ない。
    assert!(env
        .inbox
        .inbox(&env.tenant, "acme", "bob", &[], None, 50)
        .await
        .unwrap()
        .is_empty());
    let got = env
        .inbox
        .get_assigned(&env.tenant, "acme", "bob", &[], item.approval_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!got.decidable);
}

/// 判断の外の状態への遷移は取り下げ（コメントは判断の種別が合わないので拾わない）。
#[tokio::test]
async fn transitions_outside_the_decisions_withdraw_the_approval() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool).await;
    let run = env.start(&approval_ir(None)).await;
    let c = env.candidate().await;
    let a = c.approval().unwrap().approval_id;
    env.comment(a, 5, "alice", "x").await;
    assert!(env
        .runs
        .wake_data_wait(&c, &env.transition("draft", "alice", 5))
        .await
        .unwrap());
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st, ports),
        (StepStatus::Succeeded, vec!["withdrawn".to_string()])
    );
    assert_eq!(out.unwrap()["comment"], Value::Null);
    assert_eq!(env.ledger(run).await.0, "withdrawn");
    env.drain().await;
    assert_eq!(env.step(run, "after_wd").await.0, StepStatus::Succeeded);
}

/// 期限切れは timeout 出口へ進み、台帳は expired。確定したものを監査の材料として返す。
#[tokio::test]
async fn expired_approvals_take_the_timeout_port() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool).await;
    let run = env.start(&approval_ir(Some(60))).await;
    let later = chrono::Utc::now() + chrono::Duration::seconds(120);
    let done = env
        .runs
        .expire_due_approvals(later, Some(&env.tenant))
        .await
        .unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].status, "expired");
    assert_eq!(done[0].run_id, run);
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st, ports),
        (StepStatus::Succeeded, vec!["timeout".to_string()])
    );
    assert_eq!(out.unwrap()["decision"], "timeout");
    assert_eq!(env.ledger(run).await.0, "expired");
    // wait の timeout の回収（承認以外の口）は承認を扱わない（確定済みの承認を二度扱うこともない）。
    assert_eq!(
        env.runs
            .expire_due_waits(later, Some(&env.tenant))
            .await
            .unwrap(),
        0
    );
}

/// 期限切れで on_timeout=fail なら、出口へ進めずに run を時間切れの理由で失敗させる。step の `error.code` と run の
/// `fail_reason` に理由が残り、画面に返す失敗の理由（GAP-09）も「時間切れ」の分類になる（BUG-70）。
#[tokio::test]
async fn expired_approvals_that_fail_keep_the_timeout_reason() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool.clone()).await;
    let mut ir = approval_ir(None);
    ir["nodes"][0]["params"]["timeout_sec"] = json!(60);
    ir["nodes"][0]["params"]["on_timeout"] = json!("fail");
    let run = env.start_by(&ir, "alice", "user", "interactive").await;
    let later = chrono::Utc::now() + chrono::Duration::seconds(120);
    let done = env
        .runs
        .expire_due_approvals(later, Some(&env.tenant))
        .await
        .unwrap();
    assert_eq!(
        done.iter().map(|d| d.status.as_str()).collect::<Vec<_>>(),
        ["expired"]
    );
    let (st, ports, _) = env.step(run, "ok").await;
    assert_eq!((st, ports), (StepStatus::Failed, Vec::<String>::new()));
    let (code, retryable, reason, status): (Option<String>, Option<bool>, Option<String>, String) =
        sqlx::query_as(
            "SELECT s.error->>'code', (s.error->>'retryable')::bool, r.fail_reason, r.status \
             FROM step_execution s JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id \
             WHERE s.tenant_id = $1 AND s.run_id = $2 AND s.node_id = 'ok'",
        )
        .bind(&env.tenant)
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
    let want = workflow_engine::approval::APPROVAL_TIMEOUT;
    assert_eq!(
        (
            code.as_deref(),
            retryable,
            reason.as_deref(),
            status.as_str()
        ),
        (Some(want), Some(false), Some(want), "failed")
    );
    assert_eq!(env.ledger(run).await.0, "expired");

    let alice = authz::AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(env.tenant.clone()),
        },
        "acme".into(),
        env.tenant.clone(),
    );
    let outcomes = workflow_engine::RunOutcomeReader::new(pool)
        .own_outcomes(&alice, &[run], &[])
        .await
        .unwrap();
    let failure = outcomes[&run].failure.as_ref().expect("失敗の理由");
    assert_eq!(failure.category, workflow_engine::FailureCategory::Timeout);
    assert!(
        failure.message.contains("時間内に終わらなかった"),
        "{}",
        failure.message
    );
    env.forget_run(run).await;
}

/// wait の timeout の回収（承認以外の口）は承認を期限切れにしない（実行主体を確かめる api の回収だけが決める・
/// D-62）。期限の来た承認は期限・承認 id の順に、続きの位置から読める（決められずに残った承認を、同じ回収の
/// 次のページで読み直さない）。
#[tokio::test]
async fn due_approvals_are_left_to_the_checked_sweep_and_page_by_keyset() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool).await;
    let first = env.start(&approval_ir(Some(60))).await;
    let second = env.start(&approval_ir(Some(60))).await;
    let later = chrono::Utc::now() + chrono::Duration::seconds(120);
    assert_eq!(
        env.runs
            .expire_due_waits(later, Some(&env.tenant))
            .await
            .unwrap(),
        0,
        "承認の購読は wait の timeout の回収で扱わない"
    );
    for run in [first, second] {
        assert_eq!(env.step(run, "ok").await.0, StepStatus::WaitingEvent);
        assert_eq!(env.ledger(run).await.0, "pending");
    }

    let page = env
        .runs
        .due_approvals(later, Some(&env.tenant), None, 1)
        .await
        .unwrap();
    assert_eq!(page.approvals.len(), 1);
    let next = page.next.expect("ページが埋まったので続きがある");
    let rest = env
        .runs
        .due_approvals(later, Some(&env.tenant), Some(next), 1)
        .await
        .unwrap();
    assert_eq!(rest.approvals.len(), 1);
    assert_ne!(
        rest.approvals[0].wait.approval_id, page.approvals[0].wait.approval_id,
        "読んだ承認を続きで読み直さない"
    );
    let tail = env
        .runs
        .due_approvals(later, Some(&env.tenant), rest.next, 1)
        .await
        .unwrap();
    assert!(tail.approvals.is_empty());
    assert!(tail.next.is_none());
    let all = env
        .runs
        .due_approvals(later, Some(&env.tenant), None, 10)
        .await
        .unwrap();
    assert_eq!(all.approvals.len(), 2);
    assert!(all.next.is_none(), "ページが埋まらなければ続きは無い");
    env.forget_run(first).await;
    env.forget_run(second).await;
}

/// run の取消で台帳は cancelled になり、受信箱から消える。
#[tokio::test]
async fn cancelling_the_run_cancels_the_pending_approval() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool).await;
    let run = env.start(&approval_ir(None)).await;
    assert_eq!(
        env.inbox
            .inbox(&env.tenant, "acme", "bob", &[], None, 50)
            .await
            .unwrap()
            .len(),
        1
    );
    env.runs
        .request_cancel(&env.tenant, env.workflow, run)
        .await
        .unwrap();
    env.runs
        .drain_cancel_requested(Some(&env.tenant))
        .await
        .unwrap();
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Cancelled)
    );
    assert_eq!(env.ledger(run).await.0, "cancelled");
    assert!(env
        .inbox
        .inbox(&env.tenant, "acme", "bob", &[], None, 50)
        .await
        .unwrap()
        .is_empty());
}

/// 判断の受付（置く → 遷移 → 失敗なら外す）は台帳の行のロックを持った 1 つの Tx（BUG-47）。取り消した・落とした
/// 受付のコメントは残らず、同じ人が同じ rev に置き直せる。受付の間に起きた起床（relay）は、その commit を待って
/// 置いたコメントを拾う。受付が行を持っている間の別の受付は「判断中」になる。
#[tokio::test]
async fn decision_comments_live_and_die_with_the_decision_tx() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool).await;
    let run = env.start(&approval_ir(None)).await;
    let c = env.candidate().await;
    let a = c.approval().unwrap().approval_id;
    let approve = workflow_engine::ir::params::ApprovalDecisionKind::Approve;

    // 取り消した受付・落とした受付（要求の取り消し）のコメントは残らない。
    let mut d = env.open_decision(a).await;
    d.put_comment(5, "bob", approve, "1 回目").await.unwrap();
    d.rollback().await.unwrap();
    {
        let mut d = env.open_decision(a).await;
        d.put_comment(5, "bob", approve, "2 回目").await.unwrap();
    }
    assert!(env.comments(a).await.is_empty());

    // 受付が行を持っている間の別の受付は、待たずに「判断中」（同じプロセスなら枠も接続も取らない・別のプロセス
    // なら台帳の行の NOWAIT で・接続を持ったまま行のロックを待たない）。
    let mut d = env.open_decision(a).await;
    d.put_comment(5, "bob", approve, "規程どおりです")
        .await
        .unwrap();
    assert!(matches!(
        env.inbox.begin_decision(&env.tenant, a).await.unwrap(),
        workflow_engine::DecisionStart::Busy
    ));
    let other_process = ApprovalStore::new(env.pool.clone());
    let started = std::time::Instant::now();
    assert!(matches!(
        other_process.begin_decision(&env.tenant, a).await.unwrap(),
        workflow_engine::DecisionStart::Busy
    ));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "行のロックを待たない"
    );

    // 受付の間に遷移が配送されても、起床は受付の commit を待ってコメントを拾う。
    let runs = env.runs.clone();
    let woke = tokio::spawn({
        let c = c.clone();
        let ev = env.transition("approved", "bob", 5);
        async move { runs.wake_data_wait(&c, &ev).await.unwrap() }
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!woke.is_finished(), "起床は受付の commit を待つ");
    d.accept(&accepted("bob", 5)).await.unwrap();
    assert!(woke.await.unwrap());
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st, ports),
        (StepStatus::Succeeded, vec!["approved".to_string()])
    );
    assert_eq!(out.unwrap()["comment"], "規程どおりです");
    assert_eq!(
        env.comments(a).await,
        vec![(5, "bob".to_string(), "規程どおりです".to_string())]
    );
    env.forget_run(run).await;
}

/// 判断の受付は同時実行の枠（プールの上限の 4 分の 1・このテストのプール 5 本なら 1 つ）に収まり、枠が空くのを
/// 接続を持たずに待つ。待ち切れなければ「混み合っている」。受付を返せば、待っていた別の承認の受付が始まる
/// （受付の 2 本目の接続を待つ間に、判断の受付でプールを使い切らない）。
#[tokio::test]
async fn decisions_wait_for_a_slot_without_holding_a_connection() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool).await;
    let first = env.start(&approval_ir(None)).await;
    let second = env.start(&approval_ir(None)).await;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT approval_id FROM workflow_approval WHERE tenant_id = $1 ORDER BY created_at",
    )
    .bind(&env.tenant)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(ids.len(), 2);
    let d = env.open_decision(ids[0]).await;

    // 枠が空かないまま待ち切れない。
    let started = std::time::Instant::now();
    assert!(matches!(
        env.inbox.begin_decision(&env.tenant, ids[1]).await.unwrap(),
        workflow_engine::DecisionStart::Crowded
    ));
    assert!(started.elapsed() >= std::time::Duration::from_secs(2));

    // 枠を待つ間は接続を持たない（プールの残りの 4 本をすべて取れる）。受付を返すと、待っていた受付が始まる。
    let waiting = tokio::spawn({
        let (inbox, tenant, id) = (env.inbox.clone(), env.tenant.clone(), ids[1]);
        async move {
            matches!(
                inbox.begin_decision(&tenant, id).await.unwrap(),
                workflow_engine::DecisionStart::Open(_)
            )
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(
            tokio::time::timeout(std::time::Duration::from_secs(2), env.pool.acquire())
                .await
                .expect("枠を待つ受付は接続を持たない")
                .unwrap(),
        );
    }
    assert!(!waiting.is_finished(), "枠が空くまで待つ");
    drop(held);
    d.rollback().await.unwrap();
    assert!(waiting.await.unwrap(), "枠が空いたら受け付ける");
    env.forget_run(first).await;
    env.forget_run(second).await;
}

/// 判断 API が期限内に受け付けた判断（遷移は済み・relay の起床の前）は、期限の回収で期限切れにされず、その判断で
/// 決まる（行は判断の状態・run は時間切れ、と分かれない・BUG-45）。期限を過ぎてから受付が台帳の行を取ったら、
/// 遷移する前に断る。受け付け済みの承認には、ほかの受付を通さない。
#[tokio::test]
async fn accepted_decisions_win_over_the_deadline() {
    let Some(pool) = setup().await else { return };
    let env = Env::new(pool).await;
    let run = env.start(&approval_ir(Some(60))).await;
    let c = env.candidate().await;
    let a = c.approval().unwrap().approval_id;

    // bob の判断を受け付けた（遷移は済み）。relay が遷移を配る前に、期限の回収が先に来た。
    let mut d = env.open_decision(a).await;
    d.put_comment(
        5,
        "bob",
        workflow_engine::ir::params::ApprovalDecisionKind::Approve,
        "規程どおりです",
    )
    .await
    .unwrap();
    d.accept(&accepted("bob", 5)).await.unwrap();
    assert!(matches!(
        env.inbox.begin_decision(&env.tenant, a).await.unwrap(),
        workflow_engine::DecisionStart::Closed { expired: false }
    ));
    let later = chrono::Utc::now() + chrono::Duration::seconds(120);
    let done = env
        .runs
        .expire_due_approvals(later, Some(&env.tenant))
        .await
        .unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(
        done[0].status, "approved",
        "期限切れにせず受け付けた判断で決める"
    );
    let (st, ports, out) = env.step(run, "ok").await;
    assert_eq!(
        (st, ports),
        (StepStatus::Succeeded, vec!["approved".to_string()])
    );
    let out = out.unwrap();
    assert_eq!(out["decision"], "approve");
    assert_eq!(out["decided_by"], "bob");
    assert_eq!(out["comment"], "規程どおりです");
    assert_eq!(out["rev"], 5);
    assert_eq!(env.ledger(run).await.0, "approved");
    // 遅れて届いた遷移の配送は何もしない（冪等）。
    assert!(!env
        .runs
        .wake_data_wait(&c, &env.transition("approved", "bob", 5))
        .await
        .unwrap());
    env.forget_run(run).await;

    // 期限を過ぎてから受付が台帳の行を取ったら、遷移の前に断る（期限はいまの時刻で決める）。
    let run = env.start(&approval_ir(Some(60))).await;
    let a = env.candidate().await.approval().unwrap().approval_id;
    sqlx::query(
        "UPDATE workflow_approval SET deadline = now() - interval '1 second' \
         WHERE tenant_id = $1 AND approval_id = $2",
    )
    .bind(&env.tenant)
    .bind(a)
    .execute(&env.pool)
    .await
    .unwrap();
    assert!(matches!(
        env.inbox.begin_decision(&env.tenant, a).await.unwrap(),
        workflow_engine::DecisionStart::Closed { expired: true }
    ));
    env.forget_run(run).await;
}
