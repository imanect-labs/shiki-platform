//! 束ねた部品の新しいバージョンと、上げると変わるところの結合テスト（GAP-11）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で次を確かめる。
//!
//! - ワークフローを改訂すると、固定した版より新しい版と、入力項目の変化（増える・必須になる）を返す。
//! - 保存ビューを改訂すると、変わる部分（絞り込みと並べ替え）だけを返す。
//! - 最新の版を固定していれば、差は空。
//! - 呼んだ人が読めない部品・種類の違う部品は `readable: false` で、最新の版も差も返さない（存在を区別しない）。
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

use artifact::{ArtifactKind, ArtifactStore, NewArtifact};
use authz::{AuthContext, AuthzClient, Principal};
use gui::{ComponentPin, MiniAppStore, PinDiff};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
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
        tenant.into(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pin_updates_show_newer_versions_and_what_changes() {
    let Some(pool) = pool().await else { return };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    use authz::client::{OpenFgaClient, OpenFgaConfig};
    let client = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: base_url.clone(),
            store_name: format!("shiki-miniapp-pins-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA 接続");
    let store_id = client.store_id().to_string();
    let fga: Arc<dyn AuthzClient> = Arc::new(client);
    let tenant = format!("t-pins-{}", Uuid::new_v4().simple());

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

    // ワークフロー: v1 は金額だけが必須、v2 で件名が増え（必須）、金額は必須でなくなる。
    let wf = artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: "expense".into(),
                body: json!({ "ir_version": 1, "name": "expense", "input_schema": {
                    "type": "object", "required": ["amount"],
                    "properties": { "amount": { "type": "integer", "title": "金額" } } } }),
            },
            None,
        )
        .await
        .expect("workflow")
        .id;
    artifacts
        .append_version(
            &alice,
            wf,
            json!({ "ir_version": 1, "name": "expense", "input_schema": {
                "type": "object", "required": ["title"], "x-order": ["title", "amount"],
                "properties": {
                    "title": { "type": "string", "title": "件名" },
                    "amount": { "type": "integer", "title": "金額" }
                } } }),
            Some(1),
            None,
        )
        .await
        .expect("workflow v2");
    // 保存ビュー: v2 で絞り込みだけを変える。
    let table = Uuid::new_v4();
    let view = artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::DataView,
                name: "requests".into(),
                body: json!({ "table_id": table, "query": {}, "display": {} }),
            },
            None,
        )
        .await
        .expect("view")
        .id;
    artifacts
        .append_version(
            &alice,
            view,
            json!({ "table_id": table, "query": { "limit": 20 }, "display": {} }),
            Some(1),
            None,
        )
        .await
        .expect("view v2");

    let pin = |artifact_id: Uuid, version: i64| ComponentPin {
        artifact_id,
        version,
    };
    let wf_updates = apps
        .pin_updates(
            &alice,
            ArtifactKind::Workflow,
            &[pin(wf, 1), pin(wf, 2)],
            None,
        )
        .await
        .expect("pin updates");
    assert!(wf_updates[0].readable);
    assert_eq!(wf_updates[0].latest_version, Some(2));
    assert_eq!(
        wf_updates[0].changes,
        vec![
            PinDiff::InputAdded {
                key: "title".into(),
                label: Some("件名".into()),
                required: true
            },
            PinDiff::InputRequired {
                key: "amount".into(),
                label: Some("金額".into()),
                required: false
            },
        ]
    );
    // 最新の版を固定していれば差は無い。
    assert_eq!(wf_updates[1].latest_version, Some(2));
    assert!(wf_updates[1].changes.is_empty());

    let view_updates = apps
        .pin_updates(&alice, ArtifactKind::DataView, &[pin(view, 1)], None)
        .await
        .expect("view updates");
    assert_eq!(view_updates[0].latest_version, Some(2));
    assert_eq!(view_updates[0].changes, vec![PinDiff::ViewQuery]);

    // 読めない人には、最新の版も差も返さない。種類の違う部品も読めないと同じ扱い。
    let hidden = apps
        .pin_updates(&bob, ArtifactKind::Workflow, &[pin(wf, 1)], None)
        .await
        .expect("bob");
    assert!(!hidden[0].readable);
    assert_eq!(hidden[0].latest_version, None);
    assert!(hidden[0].changes.is_empty());
    let wrong_kind = apps
        .pin_updates(&alice, ArtifactKind::DataView, &[pin(wf, 1)], None)
        .await
        .expect("wrong kind");
    assert!(!wrong_kind[0].readable);
    assert_eq!(wrong_kind[0].latest_version, None);
    let missing = apps
        .pin_updates(
            &alice,
            ArtifactKind::Workflow,
            &[pin(Uuid::new_v4(), 1)],
            None,
        )
        .await
        .expect("missing");
    assert!(!missing[0].readable);
}
