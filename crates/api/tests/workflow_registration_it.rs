//! 有効化・同意・トリガ実体化の結合テスト（Task 10.4a 残・engine.md §10）。
//!
//! enable → workflow_trigger 実体化 → tick_schedules 発火 → disable で停止 →
//! 委譲者失権 → check_run_start が suspend → 再 enable で回復 → scope 拡大の軽量切替 409 相当、
//! を実 Postgres＋live OpenFGA で検証する（env 未設定はスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{
    DelegationStore, EnableError, GrantRequest, RegistrationService, RunAdmission, WorkflowIr,
    WorkflowStore,
};
use workflow_engine::{RunLauncher, SchedulerStore};

fn user_ctx(tenant: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

struct Env {
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
}

async fn setup() -> Option<Env> {
    let (Ok(db), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
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
                store_name: format!("shiki-wf-reg-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    ) as Arc<dyn AuthzClient>;
    Some(Env { pool, fga })
}

async fn mk_folder(pool: &sqlx::PgPool, tenant: &str, id: Uuid) {
    sqlx::query(
        "INSERT INTO node (id, org, tenant_id, kind, name, parent_id, created_by, updated_by) \
         VALUES ($1, 'acme', $2, 'folder', $3, NULL, 'alice', 'alice')",
    )
    .bind(id)
    .bind(tenant)
    .bind(id.to_string())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_closure (org, tenant_id, ancestor, descendant, depth) \
         VALUES ('acme', $1, $2, $2, 0)",
    )
    .bind(tenant)
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

/// 起動数を数えるだけの launcher（schedule 発火の観測用）。
struct CountingLauncher {
    launches: AtomicUsize,
}

#[async_trait]
impl RunLauncher for CountingLauncher {
    async fn launch(
        &self,
        _tenant_id: &str,
        _workflow_id: Uuid,
        _trigger_kind: &str,
        _trigger_id: &str,
        _payload: &Value,
    ) -> Option<Uuid> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        Some(Uuid::new_v4())
    }
}

fn ir_v1(folder: Uuid) -> Value {
    json!({
        "ir_version": 1,
        "name": "reg-flow",
        "declared_scopes": ["storage.read"],
        "triggers": [
            { "kind": "schedule", "cron": "* * * * *", "tz": "UTC" },
            { "kind": "event", "source": "storage.write", "scope": { "folder": folder.to_string() } },
            { "kind": "interactive" }
        ],
        "nodes": [
            { "id": "ls", "type": "storage.list", "params": { "folder": folder.to_string() } }
        ],
        "edges": []
    })
}

/// v2: storage.write スコープが増える（scope 拡大）。
fn ir_v2(folder: Uuid) -> Value {
    let mut ir = ir_v1(folder);
    ir["declared_scopes"] = json!(["storage.read", "storage.write"]);
    ir["nodes"].as_array_mut().unwrap().push(json!(
        { "id": "wr", "type": "storage.write",
          "params": { "folder": folder.to_string(), "name": "o", "content": "x" } }
    ));
    ir["edges"] = json!([{ "from": "ls", "to": "wr" }]);
    ir
}

#[tokio::test]
async fn enable_materializes_triggers_and_lifecycle_works() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");
    let folder = Uuid::new_v4();
    mk_folder(&env.pool, &tenant, folder).await;
    // alice にフォルダ viewer を付与（委譲の原資）。
    let folder_obj = alice.ns().folder(&folder.to_string());
    env.fga
        .write_tuple(&alice.subject(), Relation::Viewer, &folder_obj)
        .await
        .unwrap();

    // IR v1/v2 を保存。
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        env.pool.clone(),
        Arc::clone(&env.fga),
    ));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let catalog = workflow_engine::Catalog::default();
    let (wf_id, _) = workflows
        .create(&alice, &ir_v1(folder), &catalog, None)
        .await
        .expect("create v1");
    workflows
        .update(&alice, wf_id, &ir_v2(folder), &catalog, Some(1), None)
        .await
        .expect("update v2");

    let delegation = DelegationStore::new(env.pool.clone(), Arc::clone(&env.fga));
    let service = RegistrationService::new(env.pool.clone(), delegation.clone());
    let ir1 = WorkflowIr::from_json(&ir_v1(folder)).unwrap();
    let ir2 = WorkflowIr::from_json(&ir_v2(folder)).unwrap();
    let grants = [GrantRequest {
        scope: "storage.read".into(),
        object: folder_obj.clone(),
        relation: Relation::Viewer,
    }];

    // --- enable v1: トリガが実体化される。
    service
        .enable(&alice, wf_id, 1, &ir1, &grants)
        .await
        .expect("enable v1");
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT kind, source FROM workflow_trigger \
         WHERE tenant_id = $1 AND workflow_id = $2 AND enabled ORDER BY trigger_id",
    )
    .bind(&tenant)
    .bind(wf_id)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3, "3 トリガが実体化: {rows:?}");
    assert!(rows.iter().any(|(k, _)| k == "schedule"));
    assert!(rows
        .iter()
        .any(|(k, s)| k == "event" && s.as_deref() == Some("storage.write")));

    // --- schedule が発火する（cron 毎分・1 分の窓で 1 occurrence）。
    let sched = SchedulerStore::new(env.pool.clone());
    let launcher = CountingLauncher {
        launches: AtomicUsize::new(0),
    };
    let fired = sched
        .tick_schedules(
            Utc::now() + chrono::Duration::seconds(61),
            Some(&tenant),
            &launcher,
        )
        .await
        .unwrap();
    assert!(fired >= 1, "schedule トリガが発火する（fired={fired}）");

    // --- disable: トリガ停止・以後発火しない。
    service.disable(&tenant, wf_id).await.expect("disable");
    let fired = sched
        .tick_schedules(
            Utc::now() + chrono::Duration::seconds(180),
            Some(&tenant),
            &launcher,
        )
        .await
        .unwrap();
    assert_eq!(fired, 0, "disable 後は発火しない");
    assert_eq!(
        service.view(&tenant, wf_id).await.unwrap().status,
        "disabled"
    );

    // --- 再 enable → 委譲者失権 → check_run_start が suspend（fail-closed・黙って動かない）。
    service
        .enable(&alice, wf_id, 1, &ir1, &grants)
        .await
        .expect("re-enable");
    env.fga
        .delete_tuple(&alice.subject(), Relation::Viewer, &folder_obj)
        .await
        .unwrap();
    let admission = delegation
        .check_run_start(&tenant, wf_id, &ir1.declared_scopes)
        .await
        .unwrap();
    assert!(
        matches!(admission, RunAdmission::DelegationInvalid(_)),
        "失権後は run 開始不可: {admission:?}"
    );
    assert_eq!(
        service.view(&tenant, wf_id).await.unwrap().status,
        "suspended_reconsent",
        "再同意要求状態になる"
    );

    // --- 権限回復＋再 enable で復帰する。
    env.fga
        .write_tuple(&alice.subject(), Relation::Viewer, &folder_obj)
        .await
        .unwrap();
    service
        .enable(&alice, wf_id, 1, &ir1, &grants)
        .await
        .expect("recover");
    assert_eq!(
        delegation
            .check_run_start(&tenant, wf_id, &ir1.declared_scopes)
            .await
            .unwrap(),
        RunAdmission::Ok
    );

    // --- scope 拡大（v2）の軽量切替（grants なし）は拒否（API 層は 409 missing_scopes）。
    let err = service
        .enable(&alice, wf_id, 2, &ir2, &[])
        .await
        .expect_err("scope 拡大の軽量切替は拒否");
    match err {
        EnableError::ScopeExpansion { missing } => {
            assert_eq!(missing, vec!["storage.write".to_string()]);
        }
        other => panic!("ScopeExpansion のはず: {other:?}"),
    }
    // enabled_version は v1 のまま（拒否は無変更）。
    let view = service.view(&tenant, wf_id).await.unwrap();
    assert_eq!(view.enabled_version, Some(1));

    // --- 一覧要約: enabled 状態とトリガ種が単一 SQL 射影で取れる（Task 10.14 一覧 API の材料）。
    let summaries = workflow_engine::WorkflowSummaryStore::new(env.pool.clone())
        .list(&tenant, &[wf_id])
        .await
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].enabled_status, "enabled");
    assert_eq!(summaries[0].enabled_version, Some(1));
    assert_eq!(summaries[0].current_version, 2, "最新版は v2");
    assert_eq!(
        summaries[0].trigger_kinds,
        vec!["schedule", "event", "interactive"],
        "トリガ種が body から射影される"
    );

    // --- 縮小/同一の軽量切替は grants なしで通る（v1 と同一 scope の v3 相当として v1 を再指定）。
    service
        .enable(&alice, wf_id, 1, &ir1, &[])
        .await
        .expect("軽量切替（同一 scope）");
    let view = service.view(&tenant, wf_id).await.unwrap();
    assert_eq!(view.status, "enabled");
    // 既存委譲は維持される（軽量切替は FGA/委譲行に触れない）。
    assert_eq!(view.delegations.len(), 1);
}

#[tokio::test]
async fn enable_rejects_event_trigger_on_unreadable_folder() {
    // 有効化者が viewer を持たないフォルダへのイベント購読は全体拒否（fail-closed・Codex P1）。
    // これを許すと editor が任意フォルダの書込イベント（ファイル名/id 等のメタデータ）を購読できる。
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");
    let folder = Uuid::new_v4();
    mk_folder(&env.pool, &tenant, folder).await;
    // フォルダ viewer は付与しない（読めないフォルダ）。
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        env.pool.clone(),
        Arc::clone(&env.fga),
    ));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let (wf_id, _) = workflows
        .create(
            &alice,
            &ir_v1(folder),
            &workflow_engine::Catalog::default(),
            None,
        )
        .await
        .expect("create");
    let delegation = DelegationStore::new(env.pool.clone(), Arc::clone(&env.fga));
    let service = RegistrationService::new(env.pool.clone(), delegation);
    let ir1 = WorkflowIr::from_json(&ir_v1(folder)).unwrap();
    let err = service
        .enable(&alice, wf_id, 1, &ir1, &[])
        .await
        .expect_err("読めないフォルダのイベント購読は拒否");
    assert!(
        matches!(
            err,
            EnableError::Delegation(workflow_engine::DelegationError::OutOfScope(_))
        ),
        "{err:?}"
    );
    // トリガは実体化されない（fail-closed）。
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_trigger WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&tenant)
    .bind(wf_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn enable_rejects_out_of_scope_grants() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");
    let folder = Uuid::new_v4();
    mk_folder(&env.pool, &tenant, folder).await;
    // alice はフォルダ viewer を**持たない**。
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        env.pool.clone(),
        Arc::clone(&env.fga),
    ));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let (wf_id, _) = workflows
        .create(
            &alice,
            &ir_v1(folder),
            &workflow_engine::Catalog::default(),
            None,
        )
        .await
        .expect("create");

    let delegation = DelegationStore::new(env.pool.clone(), Arc::clone(&env.fga));
    let service = RegistrationService::new(env.pool.clone(), delegation);
    let ir1 = WorkflowIr::from_json(&ir_v1(folder)).unwrap();
    let err = service
        .enable(
            &alice,
            wf_id,
            1,
            &ir1,
            &[GrantRequest {
                scope: "storage.read".into(),
                object: alice.ns().folder(&folder.to_string()),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect_err("権限外の委譲は全体拒否");
    assert!(
        matches!(
            err,
            EnableError::Delegation(workflow_engine::DelegationError::OutOfScope(_))
        ),
        "{err:?}"
    );
    // fail-closed: registration は作られない（enabled にならない）。
    let view = service.view(&tenant, wf_id).await.unwrap();
    assert_eq!(view.status, "none");
}
