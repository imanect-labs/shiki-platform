//! ミニアプリのボタンの入力の誤りを、画面の部品の名前で言う結合テスト（GAP-41）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）と本物のワークフローの起動口
//! （`WorkflowRunLauncher`・api の配線と同じ誤りの写し `gui::from_launcher_error`）で次を確かめる。
//!
//! - 画面の部品は必須でないが、ワークフローの入力項目では必須の入力を空で押すと、入力のキー（`amount`）でなく
//!   部品の名前（「金額: 必須です」）で断る。
//! - 画面に部品が無い必須の入力（画面と入力項目の食い違い）は、キーを出さずに作った人に伝えるよう言う。
//! - 断った送信は run を作らず、同じ冪等キーでそのまま押し直せる（確保を残さない）。
//!
//! テストで作った行（テナントごと）と OpenFGA のストアは、落ちても最後に消す。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

mod common;

use std::sync::Arc;

use artifact::ArtifactStore;
use async_trait::async_trait;
use authz::{AuthContext, AuthzClient, Principal};
use gui::{ActionError, MiniAppActionRequest, MiniAppStore, SaveMode, WorkflowStarter};
use serde_json::{json, Map, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::{Catalog, DelegationStore, RunStore, WorkflowRunLauncher, WorkflowStore};

async fn pool() -> Option<PgPool> {
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

/// api の LauncherWorkflowStarter と同じ結線（private のため同型を組む・誤りの写しは同じ関数）。
struct LauncherStarter(WorkflowRunLauncher);

#[async_trait]
impl WorkflowStarter for LauncherStarter {
    async fn start_pinned(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, ActionError> {
        self.0
            .start_interactive_version(ctx, workflow_id, version, input)
            .await
            .map_err(gui::from_launcher_error)
    }

    async fn start_pinned_via_bundle(
        &self,
        ctx: &AuthContext,
        bundle_id: Uuid,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, ActionError> {
        self.0
            .start_interactive_via_bundle(ctx, bundle_id, workflow_id, version, input)
            .await
            .map_err(gui::from_launcher_error)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn button_input_errors_name_the_screen_fields() {
    let Some(pool) = pool().await else { return };
    let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    use authz::client::{OpenFgaClient, OpenFgaConfig};
    let client = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: base_url.clone(),
            store_name: format!("shiki-miniapp-button-errors-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA 接続");
    let store_id = client.store_id().to_string();
    let fga: Arc<dyn AuthzClient> = Arc::new(client);
    let tenant = format!("t-btn-errors-{}", Uuid::new_v4().simple());

    let outcome = tokio::spawn(body(pool.clone(), Arc::clone(&fga), tenant.clone())).await;
    common::purge_tenant_rows(&pool, &tenant).await;
    let resp = reqwest::Client::new()
        .delete(format!("{base_url}/stores/{store_id}"))
        .send()
        .await
        .expect("OpenFGA delete store");
    assert!(
        resp.status().is_success(),
        "delete store: {}",
        resp.status()
    );
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

/// 入力項目の金額（amount）と件名（title）が必須のワークフロー。
fn expense_ir() -> Value {
    json!({
        "ir_version": 1,
        "name": "expense",
        "declared_scopes": [],
        "triggers": [{ "kind": "interactive" }],
        "input_schema": {
            "type": "object",
            "required": ["amount", "title"],
            "properties": {
                "amount": { "type": "integer", "minimum": 1 },
                "title": { "type": "string", "title": "件名" }
            }
        },
        "nodes": [{
            "id": "compute",
            "type": "script.run",
            "params": { "source": { "inline": "function main(i){ return { ok: true }; }" } }
        }],
        "edges": []
    })
}

async fn body(pool: PgPool, fga: Arc<dyn AuthzClient>, tenant: String) {
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let delegation = DelegationStore::new(pool.clone(), Arc::clone(&fga));
    let launcher = WorkflowRunLauncher::new(delegation, workflows.clone(), runs);
    let apps = MiniAppStore::new(Arc::clone(&artifacts), pool.clone());
    let mut dispatcher =
        gui::ActionDispatcher::new(storage::audit::AuditRecorder::new(pool.clone()));
    dispatcher.set_workflow_starter(Arc::new(LauncherStarter(launcher)));
    let alice = ctx(&tenant, "alice");

    let (wf, _) = workflows
        .create(&alice, &expense_ir(), &Catalog::default(), None)
        .await
        .expect("ワークフロー");
    // 画面の金額は必須にしていない（入力項目では必須）。件名の部品は画面に無い。
    let screen = "```ui\napp 経費の申請\n金額 = number 円 as amount\nbutton 申請する -> workflow expense\n```\n";
    let app = apps
        .create(
            &alice,
            "button-errors-app",
            &json!({
                "description": "経費の申請",
                "screen": screen,
                "workflows": [{ "alias": "expense", "artifact_id": wf, "version": 1 }],
            }),
            SaveMode::Draft,
            None,
        )
        .await
        .expect("アプリ");
    let block = yui::extract_action_blocks(screen).remove(0);
    let press = |params: Value, key: &str| MiniAppActionRequest {
        version: app.version,
        action_id: yui::action_key(&block.hash, "a1"),
        params: params.as_object().cloned().unwrap_or_else(Map::new),
        idempotency_key: Some(key.into()),
    };

    // 金額を空で押す: 部品の名前で言い、キーは出さない。件名は画面に無いので、作った人に伝えるよう言う。
    let err = apps
        .invoke_yui_action(
            &alice,
            &dispatcher,
            app.id,
            press(json!({}), "btn-errors-0001"),
            None,
        )
        .await
        .expect_err("入力項目に合わない");
    let ActionError::Invalid(msg) = err else {
        panic!("{err:?}")
    };
    assert_eq!(
        msg,
        "金額: 必須です\n画面の入力が、ワークフローの入力項目と合っていないため、実行できませんでした。アプリを作成した人に伝えてください。"
    );
    assert!(!msg.contains("amount") && !msg.contains("title"), "{msg}");

    // 範囲の誤りも部品の名前で言う。断った送信は確保を残さないので、同じ冪等キーで押し直せる。
    let err = apps
        .invoke_yui_action(
            &alice,
            &dispatcher,
            app.id,
            press(json!({ "amount": 0 }), "btn-errors-0001"),
            None,
        )
        .await
        .expect_err("下限を下回る");
    let ActionError::Invalid(msg) = err else {
        panic!("{err:?}")
    };
    assert!(msg.starts_with("金額: "), "{msg}");
    assert!(!msg.contains("amount"), "{msg}");

    let made: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_run WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(made, 0, "断った送信は run を作らない");
}
