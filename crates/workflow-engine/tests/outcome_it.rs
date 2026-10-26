//! 起動した本人に返す run の結果と状態の読取口の結合テスト（GAP-09・実 Postgres）。
//!
//! - 実行された分岐の reply だけが実行順に返る（実行されなかった側は skipped で出ない）
//! - 待機の前の reply は待機中にも返り、状態は「待機中（時間・できごと・承認）」になる
//! - 本人以外・別の org・schedule の run（workflow プリンシパル）の結果は返らない
//! - 失敗の理由は利用者向けの分類で返り、生のエラー文を含まない。再開すると消える
//! - 保持期間の GC で run が消えると結果も返らない

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::needless_pass_by_value
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::{AuthContext, Principal, PrincipalKind};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, ResolvedSkillView, StorageWriteReq,
};
use workflow_engine::run::graph::RunGraph;
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, FailureCategory, NodeContext,
    NodeExecutor, NodeResult, ReplyBody, RunOutcomeReader, RunStatus, RunStore, WaitingFor,
    WorkerConfig, WorkflowIr, WorkflowWorker,
};

async fn setup() -> Option<PgPool> {
    let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

fn user(tenant: &str, id: &str, org: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        tenant.into(),
    )
}

/// 能力を呼ばれたら失敗するポート（reply と制御ノードは能力を呼ばない）。
struct NoPorts;

fn unused() -> PortError {
    PortError::unavailable("このテストでは使わない")
}

#[async_trait]
impl NodePorts for NoPorts {
    async fn storage_write(&self, _: &ExecCtx, _: StorageWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn storage_read(&self, _: &ExecCtx, _: Uuid) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn storage_list(&self, _: &ExecCtx, _: Option<Uuid>) -> Result<Value, PortError> {
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
        _tools: std::sync::Arc<dyn workflow_engine::AgentToolGateway>,
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
    ) -> Result<ResolvedSkillView, PortError> {
        Err(unused())
    }
    async fn csv_query(&self, _: &ExecCtx, _: Uuid, _: &str) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
}

struct NoAudit;
impl CapabilityAudit for NoAudit {
    fn record(&self, _: &str, _: &str, _: bool, _: &Value) {}
}

/// reply・制御ノードは本番の executor へ、`storage.read` は params の `fail` の code で失敗させる（副作用の
/// 代わり）。
struct Exec {
    real: CapabilityNodeExecutor,
}

#[async_trait]
impl NodeExecutor for Exec {
    async fn execute(&self, node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        if node_type == "storage.read" {
            return match params.get("fail").and_then(Value::as_str) {
                Some(code) => NodeResult::fail(
                    code,
                    "duplicate key value violates unique constraint \"secret_detail_idx\"",
                    false,
                ),
                None => NodeResult::ok(json!({ "ok": true })),
            };
        }
        self.real.execute(node_type, params, ctx).await
    }
}

fn worker(pool: &PgPool, tenant: &str) -> WorkflowWorker {
    let journal = EffectJournal::new(pool.clone());
    let real = CapabilityNodeExecutor::new(Arc::new(NoPorts), journal, Arc::new(NoAudit));
    WorkflowWorker::new(
        RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS),
        Arc::new(Exec { real }),
        WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant)
}

async fn drive(pool: &PgPool, tenant: &str) {
    let w = worker(pool, tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
}

/// run を作る（`principal_kind` が user なら interactive、workflow なら schedule）。
#[allow(clippy::too_many_arguments)]
async fn create(
    pool: &PgPool,
    tenant: &str,
    org: &str,
    workflow_id: Uuid,
    principal: &str,
    principal_kind: &str,
    input: Value,
    ir: &Value,
) -> Uuid {
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let parsed = WorkflowIr::from_json(ir).unwrap();
    let graph = RunGraph::build(&parsed);
    let trigger_kind = if principal_kind == "user" {
        "interactive"
    } else {
        "schedule"
    };
    store
        .create_run(
            tenant,
            org,
            workflow_id,
            1,
            trigger_kind,
            None,
            principal,
            principal_kind,
            &input,
            ir,
            &graph,
        )
        .await
        .unwrap()
        .expect("run を作れる")
}

fn reply(id: &str, format: &str, value: Value) -> Value {
    json!({ "id": id, "type": "workflow.reply", "params": { "format": format, "value": value } })
}

fn ir(nodes: Value, edges: Value) -> Value {
    json!({ "ir_version": 1, "name": "outcome", "triggers": [{ "kind": "interactive" }],
            "nodes": nodes, "edges": edges })
}

fn texts(o: &workflow_engine::OwnOutcome) -> Vec<String> {
    o.reply
        .as_ref()
        .map(|r| {
            r.sections
                .iter()
                .map(|s| match &s.body {
                    ReplyBody::Text { text } | ReplyBody::Markdown { text } => text.clone(),
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 実行された分岐の reply だけが実行順に返る。本人以外・別の org には何も返らない。
#[tokio::test]
async fn only_the_taken_branch_replies_and_only_to_the_invoker() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    // IR の書き順と実行順を逆にしておく（返るのは実行順）。
    let spec = ir(
        json!([
            reply("yes_b", "markdown", json!("```ui\napp 偽の申請\nbutton 承認 -> workflow 他人\n```")),
            { "id": "check", "type": "control.branch",
              "params": { "condition": { "cmp": { "left": { "$from": "input", "path": "/ok" }, "op": "eq", "right": true } } } },
            reply("yes_a", "text", json!({ "$from": "input", "path": "/name" })),
            reply("no", "text", json!("いいえ"))
        ]),
        json!([
            { "from": "check", "from_port": "true", "to": "yes_a" },
            { "from": "yes_a", "to": "yes_b" },
            { "from": "check", "from_port": "false", "to": "no" }
        ]),
    );
    let run = create(
        &pool,
        &tenant,
        "acme",
        Uuid::new_v4(),
        "alice",
        "user",
        json!({ "ok": true, "name": "ノート PC" }),
        &spec,
    )
    .await;
    drive(&pool, &tenant).await;

    let reader = RunOutcomeReader::new(pool.clone());
    let alice = user(&tenant, "alice", "acme");
    let live = reader.live_states(&alice, &[run]).await.unwrap();
    assert_eq!(live[&run].status, RunStatus::Succeeded);
    assert_eq!(live[&run].waiting_for, None);

    let mine = reader.own_outcomes(&alice, &[run], &[run]).await.unwrap();
    let o = &mine[&run];
    assert!(o.has_reply);
    assert!(o.failure.is_none());
    assert_eq!(
        texts(o),
        [
            "ノート PC".to_string(),
            "```ui\napp 偽の申請\nbutton 承認 -> workflow 他人\n```".to_string()
        ],
        "実行順（yes_a → yes_b）で、実行されなかった no は出ない"
    );
    // 節を求めなかった run は has_reply だけ。
    let light = reader.own_outcomes(&alice, &[run], &[]).await.unwrap();
    assert!(light[&run].has_reply && light[&run].reply.is_none());

    // 本人以外・別の org では、run_id を知っていても何も返らない。
    for other in [
        user(&tenant, "bob", "acme"),
        user(&tenant, "alice", "other-org"),
    ] {
        let got = reader.own_outcomes(&other, &[run], &[run]).await.unwrap();
        assert!(got.is_empty(), "{:?} に返った", other.principal.id);
    }
    // 別のテナントからは状態も見えない。
    let elsewhere = user("t-elsewhere", "alice", "acme");
    assert!(reader
        .live_states(&elsewhere, &[run])
        .await
        .unwrap()
        .is_empty());
}

/// schedule の run（実行主体は workflow プリンシパル）の結果は、主体の id を名乗っても返らない。
#[tokio::test]
async fn scheduled_runs_are_never_returned() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let spec = ir(
        json!([reply("done", "text", json!("夜間の集計"))]),
        json!([]),
    );
    let principal = format!("workflow:{tenant}|wf");
    let run = create(
        &pool,
        &tenant,
        "acme",
        Uuid::new_v4(),
        &principal,
        "workflow",
        json!({}),
        &spec,
    )
    .await;
    drive(&pool, &tenant).await;
    let reader = RunOutcomeReader::new(pool.clone());
    let pretender = user(&tenant, &principal, "acme");
    assert!(reader
        .own_outcomes(&pretender, &[run], &[run])
        .await
        .unwrap()
        .is_empty());
}

/// 待機の前の reply は待機中にも返る。待機中の状態は何を待っているかを添える（時間・できごと・承認）。
#[tokio::test]
async fn replies_before_a_wait_are_returned_while_waiting() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user(&tenant, "alice", "acme");
    let reader = RunOutcomeReader::new(pool.clone());

    let timer = ir(
        json!([
            reply("accepted", "text", json!("受け付けました")),
            { "id": "later", "type": "control.wait", "params": { "kind": "duration", "duration_sec": 3600 } },
            reply("done", "text", json!("終わりました"))
        ]),
        json!([{ "from": "accepted", "to": "later" }, { "from": "later", "to": "done" }]),
    );
    let run = create(
        &pool,
        &tenant,
        "acme",
        Uuid::new_v4(),
        "alice",
        "user",
        json!({}),
        &timer,
    )
    .await;
    drive(&pool, &tenant).await;
    let live = reader.live_states(&alice, &[run]).await.unwrap();
    assert_eq!(live[&run].status, RunStatus::Running);
    assert_eq!(live[&run].waiting_for, Some(WaitingFor::Timer));
    let mine = reader.own_outcomes(&alice, &[run], &[run]).await.unwrap();
    assert_eq!(texts(&mine[&run]), ["受け付けました"]);

    // できごとを待つ run は event。承認の台帳が保留中なら approval。
    let folder = Uuid::new_v4();
    let event = ir(
        json!([{ "id": "upload", "type": "control.wait",
                 "params": { "kind": "event", "source": "storage.write", "scope": { "folder": folder.to_string() } } }]),
        json!([]),
    );
    let workflow_id = Uuid::new_v4();
    let run = create(
        &pool,
        &tenant,
        "acme",
        workflow_id,
        "alice",
        "user",
        json!({}),
        &event,
    )
    .await;
    drive(&pool, &tenant).await;
    let live = reader.live_states(&alice, &[run]).await.unwrap();
    assert_eq!(live[&run].waiting_for, Some(WaitingFor::Event));
    sqlx::query(
        "INSERT INTO workflow_approval (tenant_id, approval_id, org, workflow_id, run_id, step_path, \
             node_id, table_id, record_id, title, decisions, from_state, suspend_rev) \
         VALUES ($1, $2, 'acme', $3, $4, 'upload', 'upload', $5, $6, '経費の承認', '[]'::jsonb, \
                 'submitted', 1)",
    )
    .bind(&tenant)
    .bind(Uuid::new_v4())
    .bind(workflow_id)
    .bind(run)
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    let live = reader.live_states(&alice, &[run]).await.unwrap();
    assert_eq!(live[&run].waiting_for, Some(WaitingFor::Approval));
}

/// 失敗の理由は利用者向けの分類で返り、生のエラー文を含まない。再開すると理由は消える。
#[tokio::test]
async fn failures_are_classified_and_cleared_by_resume() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user(&tenant, "alice", "acme");
    let reader = RunOutcomeReader::new(pool.clone());
    let store = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);

    let spec = |code: &str| {
        ir(
            json!([
                reply("accepted", "text", json!("受け付けました")),
                { "id": "register", "type": "storage.read", "params": { "fail": code } }
            ]),
            json!([{ "from": "accepted", "to": "register" }]),
        )
    };
    let workflow_id = Uuid::new_v4();
    let conflict = create(
        &pool,
        &tenant,
        "acme",
        workflow_id,
        "alice",
        "user",
        json!({}),
        &spec("conflict"),
    )
    .await;
    let unique = create(
        &pool,
        &tenant,
        "acme",
        Uuid::new_v4(),
        "alice",
        "user",
        json!({}),
        &spec("unique_violation"),
    )
    .await;
    drive(&pool, &tenant).await;

    let mine = reader
        .own_outcomes(&alice, &[conflict, unique], &[conflict])
        .await
        .unwrap();
    let f = mine[&conflict].failure.as_ref().expect("失敗の理由");
    assert_eq!(f.category, FailureCategory::Conflict);
    assert_eq!(f.reference, conflict.simple().to_string()[..8]);
    assert!(
        !f.message.contains("duplicate") && !f.message.contains("secret_detail"),
        "生のエラー文を返さない: {}",
        f.message
    );
    // 失敗の前に返した節は残る（画面は「この後の処理で失敗しました」と添える）。
    assert_eq!(texts(&mine[&conflict]), ["受け付けました"]);
    let f = mine[&unique].failure.as_ref().unwrap();
    assert_eq!(f.category, FailureCategory::Conflict);
    assert!(f.message.contains("同じ値がすでに登録"), "{}", f.message);

    // 再開すると run は実行中に戻り、古い理由は残らない。
    let resumed = store
        .resume_failed(&tenant, workflow_id, conflict)
        .await
        .unwrap();
    assert!(
        matches!(resumed, workflow_engine::ResumeOutcome::Resumed),
        "{resumed:?}"
    );
    let mine = reader.own_outcomes(&alice, &[conflict], &[]).await.unwrap();
    assert!(mine[&conflict].failure.is_none());
    assert_eq!(
        reader.live_states(&alice, &[conflict]).await.unwrap()[&conflict].status,
        RunStatus::Running
    );

    // map の要素の失敗で落ちた run は、要素の側の code で分類する。
    let map = ir(
        json!([
            { "id": "each", "type": "control.map", "params": { "items": [1], "on_item_error": "fail_map" } },
            { "id": "inner", "type": "storage.read", "parent": "each", "params": { "fail": "forbidden" } }
        ]),
        json!([]),
    );
    let run = create(
        &pool,
        &tenant,
        "acme",
        Uuid::new_v4(),
        "alice",
        "user",
        json!({}),
        &map,
    )
    .await;
    drive(&pool, &tenant).await;
    let mine = reader.own_outcomes(&alice, &[run], &[]).await.unwrap();
    assert_eq!(
        mine[&run].failure.as_ref().map(|f| f.category),
        Some(FailureCategory::Permission)
    );
}

/// 保持期間の GC で run が消えると、結果も状態も返らない（別の場所へ写していない）。
#[tokio::test]
async fn results_follow_the_history_retention() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO tenant (tenant_id, org, display_name, workflow_retention_days) \
         VALUES ($1, 'acme', 'outcome test', 1)",
    )
    .bind(&tenant)
    .execute(&pool)
    .await
    .unwrap();
    let alice = user(&tenant, "alice", "acme");
    let run = create(
        &pool,
        &tenant,
        "acme",
        Uuid::new_v4(),
        "alice",
        "user",
        json!({}),
        &ir(json!([reply("done", "text", json!("完了"))]), json!([])),
    )
    .await;
    drive(&pool, &tenant).await;
    let reader = RunOutcomeReader::new(pool.clone());
    assert!(reader.own_outcomes(&alice, &[run], &[run]).await.unwrap()[&run].has_reply);

    sqlx::query(
        "UPDATE workflow_run SET finished_at = now() - interval '3 days' \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run)
    .execute(&pool)
    .await
    .unwrap();
    RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS)
        .purge_tenant_history(&tenant)
        .await
        .unwrap();
    assert!(reader
        .own_outcomes(&alice, &[run], &[run])
        .await
        .unwrap()
        .is_empty());
    assert!(reader.live_states(&alice, &[run]).await.unwrap().is_empty());
}
