//! UI アクション → workflow 対話トリガ起動の結合テスト（Task 6.5 受け入れ条件④）。
//!
//! 実 Postgres＋実 OpenFGA で、①検証時にピンした版で run が作られ、実行主体が
//! **呼び出しユーザー本人**（principal_kind=user）であること、②workflow を読めない
//! ユーザーは UI アクション越しでも起動できないこと、を検証する。
//! （ノード実行時の scope_ceiling ∩ 本人 ReBAC は engine 側のテストが担保する。）

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{Catalog, DelegationStore, RunStore, WorkflowRunLauncher, WorkflowStore};

fn user_ctx(tenant: &str, org: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        tenant.into(),
    )
}

/// 最小の妥当 IR（script.run のみ・実行はしない）。
fn minimal_ir(name: &str) -> Value {
    json!({
        "ir_version": 1,
        "name": name,
        "declared_scopes": [],
        "nodes": [
            {
                "id": "compute",
                "type": "script.run",
                "params": {
                    "source": { "inline": "function main(i){ return { ok: true }; }" },
                    "input": { "$from": "input" }
                }
            }
        ],
        "edges": [],
        "triggers": [{ "kind": "interactive" }]
    })
}

/// api::wiring の LauncherWorkflowStarter と同じ結線（テストから private のため同型を組む）。
struct TestStarter(WorkflowRunLauncher);

#[async_trait::async_trait]
impl gui::WorkflowStarter for TestStarter {
    async fn start_pinned(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, gui::ActionError> {
        self.0
            .start_interactive_version(ctx, workflow_id, version, input)
            .await
            .map_err(|e| match e {
                workflow_engine::run::LauncherError::Ir(_) => gui::ActionError::NotFound,
                other => gui::ActionError::Internal(format!("run 起動: {other}")),
            })
    }
}

#[tokio::test]
async fn workflow_action_runs_pinned_version_as_caller() {
    let (Ok(db), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let fga = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: fga_url,
                store_name: format!("shiki-uiwf-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    ) as Arc<dyn AuthzClient>;

    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let org = tenant.clone();
    let alice = user_ctx(&tenant, &org, "alice");
    let bob = user_ctx(&tenant, &org, "bob");

    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(pool.clone());
    let delegation = DelegationStore::new(pool.clone(), Arc::clone(&fga));
    let launcher = WorkflowRunLauncher::new(delegation, workflows.clone(), runs.clone());

    // alice がワークフローを保存（v1）→ 改訂（v2）。
    let (wf_id, _) = workflows
        .create(&alice, &minimal_ir("wf-ui-pin"), &Catalog::default(), None)
        .await
        .expect("save ir v1");
    workflows
        .update(
            &alice,
            wf_id,
            &minimal_ir("wf-ui-pin"),
            &Catalog::default(),
            Some(1),
            None,
        )
        .await
        .expect("save ir v2");

    // v1 を明示ピンした UI スペックを検証・解決する（発話ユーザー＝alice の権限で解決）。
    let validator = gui::SpecValidator::new(Arc::clone(&artifacts), pool.clone());
    let spec = json!({
        "version": 1,
        "actions": [
            { "type": "workflow", "id": "run",
              "workflow": { "name": "wf-ui-pin", "version": 1 } }
        ],
        "root": { "component": "button", "label": "実行", "on_click": { "action": "run" } }
    });
    let resolved = validator
        .validate(&alice, &spec, "test", None)
        .await
        .expect("resolve");

    // ディスパッチ（本人権限で v1 起動）。
    let mut dispatcher =
        gui::ActionDispatcher::new(storage::audit::AuditRecorder::new(pool.clone()));
    dispatcher.set_workflow_starter(Arc::new(TestStarter(launcher.clone())));
    let source = gui::ActionSource::MiniApp {
        artifact_id: wf_id,
        version: 1,
    };
    let output = dispatcher
        .dispatch(
            &alice,
            &source,
            &resolved.doc,
            "run",
            json!({ "name": "world" }),
            None,
        )
        .await
        .expect("dispatch");
    let run_id: Uuid = serde_json::from_value(output["run_id"].clone()).expect("run id");

    // run はピン版（v1）・トリガ interactive・実行主体は呼び出しユーザー本人。
    let (version, trigger_kind, principal): (i64, String, String) = sqlx::query_as(
        "SELECT version, trigger_kind, principal FROM workflow_run \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("run row");
    assert_eq!(version, 1, "検証時にピンした版で起動する（再現性）");
    assert_eq!(trigger_kind, "interactive");
    assert_eq!(principal, "alice", "実行主体は呼び出しユーザー本人");

    // bob（workflow の viewer でない）は同じ束縛でも起動できない（存在秘匿の NotFound）。
    let err = dispatcher
        .dispatch(&bob, &source, &resolved.doc, "run", json!({}), None)
        .await
        .expect_err("bob は起動できない");
    assert!(matches!(err, gui::ActionError::NotFound));

    // 認可拒否も監査に残る（6.12）。
    let denies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'ui_action.invoke' AND decision = 'deny' AND actor = 'bob'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(denies, 1);
}
