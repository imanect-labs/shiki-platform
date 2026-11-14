//! 実 Postgres・実 OpenFGA で、ログインの同期（[`super::role_member::sync_roles`]）が、メンバーのタプルが既にある
//! ロールの台帳（`directory_role`）の欠けた行を戻すことを確かめる（BUG-270）。
//!
//! `STORAGE_TEST_DATABASE_URL`・`OPENFGA_TEST_URL` のどちらかが無ければスキップする。作ったテナントの行と OpenFGA の
//! ストアは、テストの最後に消す（落ちても消す）。
//!
//! 1. 初めての同期は、台帳の行とタプルを両方作る。
//! 2. 台帳の行だけを消してから同期し直すと、行が戻る（タプルは変えない）。残っていた行（表示名を変えたもの）は、
//!    表示名・作成日時・更新日時を変えない。
//! 3. 部署の行の組織が部署の組織と違えば（前は付けた人の組織で書いていた）、同期し直したときに直す（表示名・作成
//!    日時は変えない）。
//! 4. あるべき集合から外れたロールはタプルを外し、台帳の行は残す（台帳はテナント撤去の列挙に使う・今までどおり）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind};
use futures::FutureExt as _;
use sqlx::PgPool;
use storage::DirectoryStore;
use uuid::Uuid;

use super::membership_audit::{Discard, MembershipAudit, MembershipSource};
use super::role_member::{current_roles, sync_roles};

/// 台帳の行（表示名・作成日時・更新日時）。無ければ `None`。
async fn row(
    pool: &PgPool,
    tenant: &str,
    role: &str,
) -> Option<(
    String,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
)> {
    sqlx::query_as(
        "SELECT display_name, created_at, updated_at FROM directory_role \
         WHERE tenant_id = $1 AND role_id = $2",
    )
    .bind(tenant)
    .bind(role)
    .fetch_optional(pool)
    .await
    .expect("directory_role")
}

async fn scenario(pool: &PgPool, fga: &dyn AuthzClient, tenant: &str) {
    let ctx = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    );
    let directory = DirectoryStore::new(pool.clone());
    let audit = MembershipAudit::new(
        &Discard,
        MembershipSource::Login,
        ctx.tenant(),
        &ctx.principal().id,
    );
    // engineering は属性 roles から、acme/sales は所属グループ（部署）から来たロール。
    let desired: Vec<(String, bool)> =
        vec![("engineering".into(), false), ("acme/sales".into(), true)];

    // 1. 初めての同期: 台帳の行とタプルを両方作る。
    assert!(
        sync_roles(fga, &directory, &audit, &ctx, &desired).await,
        "タプルを足した"
    );
    let mut held = current_roles(fga, &ctx.subject(), &ctx.ns())
        .await
        .expect("read");
    held.sort();
    assert_eq!(held, ["acme/sales", "engineering"]);
    assert!(row(pool, tenant, "acme/sales").await.is_some());
    // 管理者や初期データが付けた表示名（同期は上書きしない）。
    directory
        .upsert_role("engineering", tenant, "acme", "技術部")
        .await
        .unwrap();
    let kept = row(pool, tenant, "engineering").await.expect("engineering");

    // 2. 台帳の行だけを消す（DB だけを戻したときなど）と、同期し直したときに戻る。タプルは変えない。
    sqlx::query("DELETE FROM directory_role WHERE tenant_id = $1 AND role_id = $2")
        .bind(tenant)
        .bind("acme/sales")
        .execute(pool)
        .await
        .unwrap();
    assert!(row(pool, tenant, "acme/sales").await.is_none());
    assert!(
        !sync_roles(fga, &directory, &audit, &ctx, &desired).await,
        "タプルは変えない"
    );
    let restored = row(pool, tenant, "acme/sales")
        .await
        .expect("台帳の行が戻る");
    assert_eq!(
        restored.0, "acme/sales",
        "表示名はロール ID（付与のときと同じ）"
    );
    assert_eq!(
        row(pool, tenant, "engineering").await.expect("engineering"),
        kept,
        "残っていた行は表示名・作成日時・更新日時を変えない"
    );
    assert!(directory.role_exists(&ctx, "acme/sales").await.unwrap());

    // 3. 部署の行の組織が持ち主と違う（前は付けた人の組織で書いていた）と、同期し直したときに部署の組織に直す。
    //    表示名・作成日時は変えない。属性 roles のロールの行は変えない。
    sqlx::query(
        "UPDATE directory_role SET org = 'globex', display_name = '営業部' \
         WHERE tenant_id = $1 AND role_id = $2",
    )
    .bind(tenant)
    .bind("acme/sales")
    .execute(pool)
    .await
    .unwrap();
    let moved = row(pool, tenant, "acme/sales").await.expect("acme/sales");
    assert!(!directory.role_exists(&ctx, "acme/sales").await.unwrap());
    assert!(
        !sync_roles(fga, &directory, &audit, &ctx, &desired).await,
        "タプルは変えない"
    );
    assert!(directory.role_exists(&ctx, "acme/sales").await.unwrap());
    let fixed = row(pool, tenant, "acme/sales").await.expect("acme/sales");
    assert_eq!(
        (fixed.0.as_str(), fixed.1),
        (moved.0.as_str(), moved.1),
        "表示名・作成日時は変えない"
    );
    assert_eq!(
        row(pool, tenant, "engineering").await.expect("engineering"),
        kept,
        "属性 roles のロールの行は変えない"
    );

    // 4. あるべき集合から外れたロールはタプルを外し、台帳の行は残す。
    assert!(sync_roles(fga, &directory, &audit, &ctx, &desired[..1]).await);
    assert_eq!(
        current_roles(fga, &ctx.subject(), &ctx.ns()).await.unwrap(),
        ["engineering"]
    );
    assert!(row(pool, tenant, "acme/sales").await.is_some());
}

#[tokio::test]
async fn login_sync_restores_missing_role_rows() {
    let (Ok(db_url), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let client = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("shiki-role-ledger-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("fga");
    let store_id = client.store_id().to_string();
    let fga: Arc<dyn AuthzClient> = Arc::new(client);
    let tenant = format!("t-role-ledger-{}", Uuid::new_v4().simple());

    let outcome = AssertUnwindSafe(scenario(&pool, &*fga, &tenant))
        .catch_unwind()
        .await;

    // 後片付け（落ちても）: 作った行とストア。
    sqlx::query("DELETE FROM directory_role WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .expect("cleanup");
    let deleted = reqwest::Client::new()
        .delete(format!("{fga_url}/stores/{store_id}"))
        .send()
        .await
        .expect("delete store");
    assert!(deleted.status().is_success(), "{}", deleted.status());
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
