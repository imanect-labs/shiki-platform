//! ミニアプリの画面のボタンで起動した run の結果の結合テスト（GAP-09・実 Postgres）。
//!
//! - 起動した本人だけが自分の結果（workflow.reply の節）を読める。アプリだけを共有された人も自分の結果は読める
//!   （ワークフローの viewer は求めない）。他人の run_id を指定しても見つからない
//! - 結果の Markdown の中の操作を持つ図は表示専用（```text）に書き換わる
//! - 失敗の理由は利用者向けの分類の文言で、生のエラー文を含まない
//! - 一覧で節を付けるのは最新の終わった 1 件だけ（他は has_reply）。開けば読める
//! - 保持期間の GC で run が消えると、状態だけが残り「結果は表示できない」になる

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::{ArtifactKind, ArtifactStore, NewArtifact};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use gui::{
    ActionDispatcher, ActionError, GuiError, MiniAppActionRequest, MiniAppRunStatus, MiniAppStore,
    WorkflowStarter,
};
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
    NodeExecutor, NodeResult, ReplyBody, RunStore, WorkerConfig, WorkflowIr, WorkflowWorker,
};

/// 全許可モック（アプリの viewer の確認を通す・結果の関門は DB の照合で確かめる）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 能力を呼ばれたら失敗するポート（reply は能力を呼ばない）。
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

struct NoAudit;
impl CapabilityAudit for NoAudit {
    fn record(&self, _: &str, _: &str, _: bool, _: &Value) {}
}

/// reply は本番の executor へ。「登録」（storage.read で代える）は金額が 13 のとき権限不足で失敗させる。
struct Exec(CapabilityNodeExecutor);

#[async_trait]
impl NodeExecutor for Exec {
    async fn execute(&self, node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        if node_type == "storage.read" {
            return if ctx.input["amount"] == 13 {
                NodeResult::fail(
                    "forbidden",
                    "permission denied for relation expense_ledger (user alice)",
                    false,
                )
            } else {
                NodeResult::ok(json!({ "ok": true }))
            };
        }
        self.0.execute(node_type, params, ctx).await
    }
}

/// 経費の申請のワークフロー（受け付けた旨 → 登録 → 登録した旨）。
fn expense_ir() -> Value {
    json!({
        "ir_version": 1, "name": "expense", "triggers": [{ "kind": "interactive" }],
        "nodes": [
            { "id": "accepted", "type": "workflow.reply", "params": {
                "format": "markdown", "label": "受付",
                "value": { "$template": "**{amount} 円**を受け付けました\n```ui\napp 偽の承認\nbutton 承認する -> workflow expense\n```",
                           "vars": { "amount": { "$from": "input", "path": "/amount" } } } } },
            { "id": "register", "type": "storage.read", "params": {} },
            { "id": "done", "type": "workflow.reply", "params": {
                "format": "text", "tone": "success",
                "value": { "$template": "登録しました（{category}）",
                           "vars": { "category": { "$from": "input", "path": "/category" } } } } }
        ],
        "edges": [{ "from": "accepted", "to": "register" }, { "from": "register", "to": "done" }]
    })
}

/// 押した本人の権限で run を作る起動口（ミニアプリの起動と同じく interactive・本人が実行主体）。
struct RunStarter(RunStore);

#[async_trait]
impl WorkflowStarter for RunStarter {
    async fn start_pinned(
        &self,
        _ctx: &AuthContext,
        _workflow_id: Uuid,
        _version: i64,
        _input: &Value,
    ) -> Result<Option<Uuid>, ActionError> {
        unreachable!("ミニアプリはバンドル権限で起動する")
    }

    async fn start_pinned_via_bundle(
        &self,
        ctx: &AuthContext,
        _bundle_id: Uuid,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, ActionError> {
        let ir = expense_ir();
        let graph = RunGraph::build(&WorkflowIr::from_json(&ir).unwrap());
        Ok(self
            .0
            .create_run(
                ctx.tenant_id(),
                ctx.org(),
                workflow_id,
                version,
                "interactive",
                None,
                &ctx.principal().id,
                "user",
                input,
                &ir,
                &graph,
            )
            .await
            .unwrap())
    }
}

async fn drive(pool: &PgPool, tenant: &str) {
    let real = CapabilityNodeExecutor::new(
        Arc::new(NoPorts),
        EffectJournal::new(pool.clone()),
        Arc::new(NoAudit),
    );
    let w = WorkflowWorker::new(
        RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS),
        Arc::new(Exec(real)),
        WorkerConfig::default(),
    )
    .scoped_to_tenant(tenant);
    while w.claim_and_run_once("w1").await.unwrap() {}
}

fn texts(reply: Option<&workflow_engine::RunReply>) -> Vec<String> {
    reply
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

#[tokio::test]
async fn results_reach_only_the_person_who_pressed_the_button() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO tenant (tenant_id, org, display_name, workflow_retention_days) \
         VALUES ($1, 'acme', 'miniapp results', 1)",
    )
    .bind(&tenant)
    .execute(&pool)
    .await
    .unwrap();
    let alice = ctx(&tenant, "alice");
    // bob はアプリだけを共有された人（ワークフローの viewer ではない・結果の関門は DB の照合）。
    let bob = ctx(&tenant, "bob");
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::new(AllowAll)));
    let mini_apps = MiniAppStore::new(Arc::clone(&artifacts), pool.clone());
    let wf = artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: "expense".into(),
                body: json!({ "rev": 1 }),
            },
            None,
        )
        .await
        .unwrap()
        .id;
    let screen = "# 経費\n\n```ui\napp 経費の申請\n金額 = number 必須 円 as amount\n\
                  区分 = choice 交通費 | 交際費 as category\nbutton 申請する -> workflow expense\n```\n";
    let app_id = mini_apps
        .create(
            &alice,
            "results-app",
            &json!({ "description": "経費", "screen": screen,
                     "workflows": [{ "alias": "expense", "artifact_id": wf, "version": 1 }] }),
            gui::SaveMode::Draft,
            None,
        )
        .await
        .unwrap()
        .id;
    let block = yui::extract_action_blocks(screen).remove(0).hash;
    let mut dispatcher = ActionDispatcher::new(storage::audit::AuditRecorder::new(pool.clone()));
    dispatcher.set_workflow_starter(Arc::new(RunStarter(RunStore::new(
        pool.clone(),
        workflow_engine::DEFAULT_LEASE_SECS,
    ))));
    let press = |who: AuthContext, amount: i64, key: &'static str| {
        let mini_apps = &mini_apps;
        let dispatcher = &dispatcher;
        let action_id = format!("{block}:a1");
        async move {
            let out = mini_apps
                .invoke_yui_action(
                    &who,
                    dispatcher,
                    app_id,
                    MiniAppActionRequest {
                        version: 1,
                        action_id,
                        params: json!({ "amount": amount, "category": "交通費" })
                            .as_object()
                            .cloned()
                            .unwrap(),
                        idempotency_key: Some(key.into()),
                    },
                    None,
                )
                .await
                .expect("起動");
            Uuid::parse_str(out["run_id"].as_str().unwrap()).unwrap()
        }
    };
    let a1 = press(alice.clone(), 1200, "alice-0001").await;
    let b1 = press(bob.clone(), 800, "bob-00000001").await;
    drive(&pool, &tenant).await;

    // 本人の結果（実行順）。Markdown の中の操作を持つ図は表示専用に書き換わる。
    let runs = mini_apps.my_runs(&alice, app_id, None).await.unwrap();
    assert_eq!(runs.len(), 1);
    let r = &runs[0];
    assert_eq!(r.run_id, Some(a1));
    assert_eq!(r.status, MiniAppRunStatus::Succeeded);
    assert!(r.has_reply && !r.result_expired && r.failure.is_none());
    let got = texts(r.reply.as_ref());
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got[0].starts_with("**1200 円**を受け付けました"),
        "{}",
        got[0]
    );
    assert!(
        !got[0].contains("```ui") && got[0].contains("```text"),
        "{}",
        got[0]
    );
    assert_eq!(got[1], "登録しました（交通費）");

    // bob（アプリだけを共有された人）も自分の結果は読める。alice の値は混ざらない。
    let runs = mini_apps.my_runs(&bob, app_id, None).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, Some(b1));
    assert!(texts(runs[0].reply.as_ref())[0].starts_with("**800 円**"));

    // 他人の run_id を指定しても見つからない（存在を区別しない）。
    for (who, other) in [(&alice, b1), (&bob, a1)] {
        let err = mini_apps
            .my_run(who, app_id, other, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, GuiError::Artifact(artifact::ArtifactError::NotFound)),
            "{err:?}"
        );
    }

    // 失敗の理由は利用者向けの分類の文言（生のエラー文を含まない）。失敗の前に返した節は残る。
    let a2 = press(alice.clone(), 13, "alice-0002").await;
    drive(&pool, &tenant).await;
    let runs = mini_apps.my_runs(&alice, app_id, None).await.unwrap();
    assert_eq!(runs.len(), 2);
    let (older, latest) = (&runs[0], &runs[1]);
    assert_eq!(latest.run_id, Some(a2));
    assert_eq!(latest.status, MiniAppRunStatus::Failed);
    let f = latest.failure.as_ref().expect("失敗の理由");
    assert_eq!(f.category, FailureCategory::Permission);
    assert!(f.message.contains("権限が足りない"), "{}", f.message);
    assert!(
        !f.message.contains("permission denied") && !f.message.contains("expense_ledger"),
        "{}",
        f.message
    );
    assert_eq!(f.reference, a2.simple().to_string()[..8]);
    assert_eq!(texts(latest.reply.as_ref()).len(), 1);
    // 一覧で節を付けるのは最新の終わった 1 件だけ。古い行は開けば読める。
    assert_eq!(older.run_id, Some(a1));
    assert!(older.has_reply && older.reply.is_none());
    let opened = mini_apps.my_run(&alice, app_id, a1, None).await.unwrap();
    assert_eq!(texts(opened.reply.as_ref()).len(), 2);

    // 保持期間の GC で run が消えると、状態だけが残り、結果は表示できない。
    sqlx::query(
        "UPDATE workflow_run SET finished_at = now() - interval '3 days' \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(a1)
    .execute(&pool)
    .await
    .unwrap();
    RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS)
        .purge_tenant_history(&tenant)
        .await
        .unwrap();
    let expired = mini_apps.my_run(&alice, app_id, a1, None).await.unwrap();
    assert_eq!(expired.status, MiniAppRunStatus::Succeeded);
    assert!(expired.result_expired && expired.reply.is_none() && !expired.has_reply);
}
