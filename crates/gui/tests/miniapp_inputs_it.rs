//! 画面の入力と、束ねたワークフローの入力項目の突き合わせの結合テスト（GAP-12・BUG-122）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で次を確かめる。
//!
//! - 保存すると、画面の入力とワークフローの入力項目の食い違いを `input_warnings` で返す（保存は止めない）。
//! - 入力の手がかり（エディタの部品の候補）は、呼んだ人が読めるワークフローだけ返す。読めないワークフローは
//!   `readable: false` で、保存のときにも突き合わせない（共同編集者が読めない部品を言わない）。
//! - 入力項目を決めていないワークフローは突き合わせない。
//! - AI が作る道具の保存（`SaveMode::Agent`・D-10 T07）は、同じ食い違いを誤りとして止める（保存しない）。会話の回答を
//!   保存する経路（`SaveMode::Strict`）は止めない。読めないワークフローは Agent でも突き合わせない。
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

use artifact::{ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use authz::{AuthContext, AuthzClient, Principal};
use gui::{ComponentPin, MiniAppStore, SaveMode};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn screen_inputs_are_matched_against_readable_workflows_on_save() {
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
            store_name: format!("shiki-miniapp-inputs-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA 接続");
    let store_id = client.store_id().to_string();
    let fga: Arc<dyn AuthzClient> = Arc::new(client);
    let tenant = format!("t-inputs-{}", Uuid::new_v4().simple());

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

async fn body(pool: PgPool, fga: Arc<dyn AuthzClient>, tenant: String) {
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), fga));
    let apps = MiniAppStore::new(Arc::clone(&artifacts), pool);
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let workflow = |name: &str, body: serde_json::Value| {
        let artifacts = Arc::clone(&artifacts);
        let alice = alice.clone();
        let name = name.to_string();
        async move {
            artifacts
                .create(
                    &alice,
                    NewArtifact {
                        kind: ArtifactKind::Workflow,
                        name,
                        body,
                    },
                    None,
                )
                .await
                .expect("workflow")
                .id
        }
    };
    let expense = workflow(
        "expense",
        json!({ "ir_version": 1, "name": "expense", "input_schema": {
            "type": "object", "required": ["amount", "title"], "x-order": ["title", "amount"],
            "properties": {
                "amount": { "type": "integer" },
                "title": { "type": "string", "title": "件名" }
            } } }),
    )
    .await;
    // 入力項目を決めていないワークフロー。
    let free = workflow("free", json!({ "ir_version": 1, "name": "free" })).await;
    let screen = "```ui\napp 経費の申請\n金額 = text as amount\nメモ = text as memo\nbutton 申請する -> workflow expense\n```\n\n\
                  ```ui\napp 連絡\n本文 = textarea as body\nbutton 送る -> workflow free\n```\n";
    let app = json!({
        "description": "経費",
        "screen": screen,
        "workflows": [
            { "alias": "expense", "artifact_id": expense, "version": 1 },
            { "alias": "free", "artifact_id": free, "version": 1 }
        ]
    });
    let saved = apps
        .create(&alice, "inputs-app", &app, SaveMode::Draft, None)
        .await
        .expect("保存は止めない");
    let got: Vec<(&str, &str)> = saved
        .input_warnings
        .iter()
        .map(|e| (e.code.as_str(), e.message.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("miniapp.missing_input", "画面の図「経費の申請」のボタン「申請する」: 「expense」の必須の入力「件名」（title）に当たる部品がありません"),
            ("miniapp.input_type", "画面の図「経費の申請」の「金額」: 「expense」の「amount」は数値を受けます（この部品は文字）"),
            ("miniapp.input_required", "画面の図「経費の申請」の「金額」: 「expense」では必須の入力です。「必須」にしてください"),
            ("miniapp.unknown_input", "画面の図「経費の申請」の「メモ」: 「expense」に「memo」という入力はありません（送っても使われないか、断られます）"),
        ]
    );
    assert!(
        saved.warnings.is_empty(),
        "書き損じではない（公開を止めない）"
    );

    // AI が作る道具の保存は、同じ食い違いを全件、誤りとして返して保存しない。
    let codes = |e: gui::GuiError| -> Vec<String> {
        match e {
            gui::GuiError::Validation(errors) => errors.into_iter().map(|e| e.code).collect(),
            other => panic!("検証の誤りのはず: {other:?}"),
        }
    };
    let rejected = apps
        .create(&alice, "inputs-app-ai", &app, SaveMode::Agent, None)
        .await
        .expect_err("AI の経路では食い違いを止める");
    assert_eq!(
        codes(rejected),
        vec![
            "miniapp.missing_input",
            "miniapp.input_type",
            "miniapp.input_required",
            "miniapp.unknown_input"
        ]
    );
    // 会話の回答を保存する経路は、書き損じだけを止め、食い違いは注意のまま。
    let strict = apps
        .create(&alice, "inputs-app-strict", &app, SaveMode::Strict, None)
        .await
        .expect("食い違いでは止めない");
    assert_eq!(strict.input_warnings.len(), 4);
    // 食い違いを直せば AI の経路でも保存できる（同じ名前で作れる＝止めた保存は何も作っていない）。
    let fixed_screen = "```ui\napp 経費の申請\n件名 = text 必須 as title\n金額 = number 必須 as amount\nbutton 申請する -> workflow expense\n```\n\n\
                        ```ui\napp 連絡\n本文 = textarea as body\nbutton 送る -> workflow free\n```\n";
    let mut fixed = app.clone();
    fixed["screen"] = json!(fixed_screen);
    let ai = apps
        .create(&alice, "inputs-app-ai", &fixed, SaveMode::Agent, None)
        .await
        .expect("食い違いの無い画面は保存できる");
    assert!(ai.input_warnings.is_empty() && ai.warnings.is_empty());

    // 入力の手がかり: 入力項目の並びの順。入力項目を決めていないワークフローは null。
    let pins = [
        ComponentPin {
            artifact_id: expense,
            version: 1,
        },
        ComponentPin {
            artifact_id: free,
            version: 1,
        },
    ];
    let mine = apps.workflow_inputs(&alice, &pins, None).await.unwrap();
    assert!(mine[0].readable && mine[1].readable);
    let keys: Vec<&str> = mine[0]
        .inputs
        .as_ref()
        .unwrap()
        .iter()
        .map(|h| h.key.as_str())
        .collect();
    assert_eq!(keys, vec!["title", "amount"]);
    assert_eq!(mine[1].inputs, None);

    // bob はアプリの共同編集者だが、ワークフローは読めない。手がかりは読めないと返し、保存のときにも突き合わせない。
    artifacts
        .share(
            &alice,
            saved.id,
            &storage::ShareTarget::User { id: "bob".into() },
            ArtifactRole::Editor,
            None,
        )
        .await
        .expect("share");
    let theirs = apps.workflow_inputs(&bob, &pins, None).await.unwrap();
    assert!(theirs.iter().all(|p| !p.readable && p.inputs.is_none()));
    let edited = apps
        .update(&bob, saved.id, &app, Some(1), SaveMode::Draft, None)
        .await
        .expect("読めない部品に触れなければ保存できる");
    assert!(
        edited.input_warnings.is_empty(),
        "{:?}",
        edited.input_warnings
    );
    // AI の経路でも、保存する人に読めないワークフローは突き合わせない（止めない）。
    apps.update(&bob, saved.id, &app, Some(2), SaveMode::Agent, None)
        .await
        .expect("読めない部品は突き合わせない");
}
