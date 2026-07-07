//! 委譲モデルの結合テスト（Task 10.4a 受け入れ条件・実 Postgres＋live OpenFGA）。
//!
//! - 有効化者の権限外スコープの委譲が拒否される（全体 fail-closed）
//! - 委譲者の権限剥奪後、次回実行が開始されず suspended_reconsent になる（黙って動き続けない）
//! - 棚卸しが失権を検知しタプル撤去＋停止する

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::{DelegationError, DelegationStore, GrantRequest, RunAdmission};

async fn setup() -> Option<(PgPool, Arc<OpenFgaClient>)> {
    let (Ok(db_url), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let config = OpenFgaConfig {
        base_url: fga_url,
        store_name: format!("shiki-deleg-test-{}", Uuid::new_v4()),
    };
    let fga = Arc::new(
        OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
            .await
            .expect("fga"),
    );
    Some((pool, fga))
}

fn user_ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
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

#[tokio::test]
async fn out_of_scope_delegation_rejected_wholesale() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");
    let store = DelegationStore::new(pool, fga.clone());

    // alice は folder A の viewer だが folder B は持たない。
    let folder_a = alice.ns().folder("A");
    let folder_b = alice.ns().folder("B");
    fga.write_tuple(&alice.subject(), Relation::Viewer, &folder_a)
        .await
        .unwrap();

    let workflow_id = Uuid::new_v4();
    // A（範囲内）＋B（範囲外）を委譲 → 全体拒否。
    let grants = vec![
        GrantRequest {
            scope: "storage.read".into(),
            object: folder_a.clone(),
            relation: Relation::Viewer,
        },
        GrantRequest {
            scope: "storage.read".into(),
            object: folder_b,
            relation: Relation::Viewer,
        },
    ];
    let res = store
        .enable(&alice, workflow_id, 1, &["storage.read".into()], &grants)
        .await;
    assert!(
        matches!(res, Err(DelegationError::OutOfScope(_))),
        "{res:?}"
    );

    // 部分委譲していない: A の workflow タプルも書かれていない（registration 未作成）。
    let admission = store
        .check_run_start(&tenant, workflow_id, &["storage.read".into()])
        .await
        .unwrap();
    assert!(matches!(admission, RunAdmission::DelegationInvalid(_)));
}

#[tokio::test]
async fn reenable_with_narrower_grants_revokes_dropped_object() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");
    let store = DelegationStore::new(pool.clone(), fga.clone());
    let wf = Uuid::new_v4();
    let wf_subject = alice.ns().workflow_principal(&wf.to_string());

    // A と B を委譲。
    let folder_a = alice.ns().folder("A");
    let folder_b = alice.ns().folder("B");
    fga.write_tuple(&alice.subject(), Relation::Viewer, &folder_a).await.unwrap();
    fga.write_tuple(&alice.subject(), Relation::Viewer, &folder_b).await.unwrap();
    let both = vec![
        GrantRequest { scope: "storage.read".into(), object: folder_a.clone(), relation: Relation::Viewer },
        GrantRequest { scope: "storage.read".into(), object: folder_b.clone(), relation: Relation::Viewer },
    ];
    store.enable(&alice, wf, 1, &["storage.read".into()], &both).await.expect("enable A+B");
    assert!(fga.check(&wf_subject, Relation::Viewer, &folder_b, authz::Consistency::HigherConsistency).await.unwrap());

    // A のみで再有効化 → B の委譲タプル・行が撤去される。
    let only_a = vec![GrantRequest {
        scope: "storage.read".into(),
        object: folder_a.clone(),
        relation: Relation::Viewer,
    }];
    store.enable(&alice, wf, 2, &["storage.read".into()], &only_a).await.expect("re-enable A");
    assert!(
        fga.check(&wf_subject, Relation::Viewer, &folder_a, authz::Consistency::HigherConsistency).await.unwrap(),
        "A は残る"
    );
    assert!(
        !fga.check(&wf_subject, Relation::Viewer, &folder_b, authz::Consistency::HigherConsistency).await.unwrap(),
        "B の委譲は撤去される（同意から外したオブジェクトに到達させない）"
    );
    let active_b: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_delegation \
         WHERE tenant_id = $1 AND workflow_id = $2 AND object_ref = $3 AND revoked_at IS NULL",
    )
    .bind(&tenant)
    .bind(wf)
    .bind(folder_b.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_b, 0, "B の委譲行は revoke 済み");
}

#[tokio::test]
async fn revocation_suspends_and_run_start_denied() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");
    let store = DelegationStore::new(pool.clone(), fga.clone());

    let folder = alice.ns().folder("shared");
    fga.write_tuple(&alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();

    let workflow_id = Uuid::new_v4();
    let grants = vec![GrantRequest {
        scope: "storage.read".into(),
        object: folder.clone(),
        relation: Relation::Viewer,
    }];
    store
        .enable(&alice, workflow_id, 1, &["storage.read".into()], &grants)
        .await
        .expect("enable");

    // 有効化直後は run 開始可。
    assert_eq!(
        store
            .check_run_start(&tenant, workflow_id, &["storage.read".into()])
            .await
            .unwrap(),
        RunAdmission::Ok
    );

    // workflow プリンシパルは委譲経由で folder を読める（confused-deputy でなく明示委譲）。
    let wf_subject = alice.ns().workflow_principal(&workflow_id.to_string());
    assert!(fga
        .check(
            &wf_subject,
            Relation::Viewer,
            &folder,
            authz::Consistency::HigherConsistency
        )
        .await
        .unwrap());

    // 委譲者 alice が folder への viewer を失う（退職/剥奪）。
    fga.delete_tuple(&alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();

    // run 開始時チェックが失権を検知 → DelegationInvalid＋suspended_reconsent。
    let admission = store
        .check_run_start(&tenant, workflow_id, &["storage.read".into()])
        .await
        .unwrap();
    assert!(
        matches!(admission, RunAdmission::DelegationInvalid(_)),
        "{admission:?}"
    );
    let status: String = sqlx::query_scalar(
        "SELECT status FROM workflow_registration WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&tenant)
    .bind(workflow_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        status, "suspended_reconsent",
        "黙って動き続けず再同意要求へ"
    );
}

#[tokio::test]
async fn inventory_revokes_workflow_tuple_on_delegator_loss() {
    let Some((pool, fga)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = user_ctx(&tenant, "alice");
    let store = DelegationStore::new(pool.clone(), fga.clone());

    let folder = alice.ns().folder("inv");
    fga.write_tuple(&alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();
    let workflow_id = Uuid::new_v4();
    store
        .enable(
            &alice,
            workflow_id,
            1,
            &["storage.read".into()],
            &[GrantRequest {
                scope: "storage.read".into(),
                object: folder.clone(),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect("enable");

    // alice 失権。
    fga.delete_tuple(&alice.subject(), Relation::Viewer, &folder)
        .await
        .unwrap();

    // 棚卸しが失権を検知し workflow タプルを撤去＋停止。
    let revoked = store.inventory(&tenant).await.expect("inventory");
    assert!(revoked.contains(&workflow_id));

    // workflow プリンシパルは folder を読めなくなる（タプル撤去）。
    let wf_subject = alice.ns().workflow_principal(&workflow_id.to_string());
    assert!(!fga
        .check(
            &wf_subject,
            Relation::Viewer,
            &folder,
            authz::Consistency::HigherConsistency
        )
        .await
        .unwrap());
    // delegation 行は revoke 済み。
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_delegation \
         WHERE tenant_id = $1 AND workflow_id = $2 AND revoked_at IS NULL",
    )
    .bind(&tenant)
    .bind(workflow_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active, 0);
}
