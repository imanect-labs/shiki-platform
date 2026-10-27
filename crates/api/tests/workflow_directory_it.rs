//! `search_workflows` の検索源（`ApiWorkflowDirectory`）の結合テスト。
//!
//! 実 Postgres＋実 OpenFGA で、①他人の共有されていないワークフローは見えない ②共有された
//! ものは見える ③別テナントのものは同じ利用者 id でも見えない ④語・対話トリガ・件数の絞り込みは
//! SQL で効く ⑤返した name は図のボタンの解決（`WorkflowResolver`）でそのまま引ける、を確かめる。
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が無ければスキップする。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use api::workflow_directory::ApiWorkflowDirectory;
use artifact::ArtifactRole;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind};
use chat::WorkflowDirectory;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{Catalog, WorkflowStore, WorkflowSummaryStore};

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
        tenant.into(),
        tenant.into(),
    )
}

/// 最小の妥当 IR（実行はしない）。
fn ir(name: &str, display: &str, description: &str, trigger: Value) -> Value {
    json!({
        "ir_version": 1,
        "name": name,
        "display_name": display,
        "description": description,
        "declared_scopes": [],
        "input_schema": {
            "type": "object",
            "required": ["amount"],
            "properties": { "amount": { "type": "number" } }
        },
        "nodes": [{
            "id": "compute",
            "type": "script.run",
            "params": {
                "source": { "inline": "function main(i){ return { ok: true }; }" },
                "input": { "$from": "input" }
            }
        }],
        "edges": [],
        "triggers": [trigger]
    })
}

fn interactive() -> Value {
    json!({ "kind": "interactive" })
}

fn names(hits: &[workflow_engine::WorkflowSearchHit]) -> Vec<&str> {
    let mut n: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    n.sort_unstable();
    n
}

#[tokio::test]
async fn search_sees_owned_and_shared_only_within_the_tenant() {
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
                store_name: format!("shiki-wfdir-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    ) as Arc<dyn AuthzClient>;

    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let other_tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user_ctx(&tenant, "alice");
    let bob = user_ctx(&tenant, "bob");
    // 別テナントの同じ利用者 id（tenant_id が落ちると混ざる）。
    let alice_elsewhere = user_ctx(&other_tenant, "alice");

    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let save = |ctx: &AuthContext, body: Value| {
        let (workflows, ctx) = (workflows.clone(), ctx.clone());
        async move {
            workflows
                .create(&ctx, &body, &Catalog::default(), None)
                .await
                .expect("save")
                .0
        }
    };

    save(
        &alice,
        ir(
            "expense-approval",
            "経費承認",
            "経費の申請を承認する",
            interactive(),
        ),
    )
    .await;
    save(
        &alice,
        ir(
            "nightly-report",
            "夜間レポート",
            "経費を毎晩集計する",
            json!({ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }),
        ),
    )
    .await;
    save(
        &bob,
        ir(
            "bob-private",
            "経費 下書き",
            "bob だけのもの",
            interactive(),
        ),
    )
    .await;
    let shared_id = save(
        &bob,
        ir("bob-shared", "経費 共有", "alice に共有する", interactive()),
    )
    .await;
    save(
        &alice_elsewhere,
        ir(
            "elsewhere",
            "経費 別テナント",
            "別テナントのもの",
            interactive(),
        ),
    )
    .await;
    artifacts
        .share(
            &bob,
            shared_id,
            &storage::ShareTarget::User { id: "alice".into() },
            ArtifactRole::Viewer,
            None,
        )
        .await
        .expect("share");

    let dir = ApiWorkflowDirectory::new(
        Arc::clone(&artifacts),
        WorkflowSummaryStore::new(pool.clone()),
    );

    // ①②③: 所有∪共有だけ。bob の非共有・別テナントは出ない。既定は対話トリガのあるものだけ。
    let hits = dir.search(&alice, "経費", true, 20).await.expect("search");
    assert_eq!(names(&hits), ["bob-shared", "expense-approval"]);
    let all = dir.search(&alice, "", false, 20).await.expect("search all");
    assert_eq!(
        names(&all),
        ["bob-shared", "expense-approval", "nightly-report"]
    );
    let bobs = dir.search(&bob, "", false, 20).await.expect("bob");
    assert_eq!(names(&bobs), ["bob-private", "bob-shared"]);
    let elsewhere = dir
        .search(&alice_elsewhere, "", false, 20)
        .await
        .expect("elsewhere");
    assert_eq!(names(&elsewhere), ["elsewhere"]);

    // ④: 語は名前・表示名・説明の部分一致（大文字小文字を区別しない・空白区切りは AND）。
    let by_name = dir.search(&alice, "EXPENSE", true, 20).await.unwrap();
    assert_eq!(names(&by_name), ["expense-approval"]);
    let and = dir.search(&alice, "経費 承認", true, 20).await.unwrap();
    assert_eq!(names(&and), ["expense-approval"]);
    let by_description = dir.search(&alice, "毎晩", false, 20).await.unwrap();
    assert_eq!(names(&by_description), ["nightly-report"]);
    // 利用者の % はワイルドカードにならない。
    assert!(dir.search(&alice, "%", false, 20).await.unwrap().is_empty());
    assert_eq!(dir.search(&alice, "", false, 1).await.unwrap().len(), 1);

    // 射影: 入力スキーマ・トリガ種・登録状態が返る。
    let expense = hits.iter().find(|h| h.name == "expense-approval").unwrap();
    assert_eq!(expense.display_name.as_deref(), Some("経費承認"));
    assert_eq!(expense.trigger_kinds, ["interactive"]);
    assert_eq!(expense.enabled_status, "none");
    assert_eq!(
        expense.input_schema.as_ref().unwrap()["required"],
        json!(["amount"])
    );

    // ⑤: 返した name は図のボタンの解決（回答した主体の viewer 権限）でそのまま引ける。
    let resolver = gui::WorkflowResolver::new(Arc::clone(&artifacts));
    for hit in &all {
        let pin = resolver
            .resolve_workflow_name(&alice, &hit.name, None)
            .await
            .unwrap_or_else(|| panic!("{} がボタンで解決できる", hit.name));
        assert_eq!(pin.artifact_id, Some(hit.id));
    }
}
