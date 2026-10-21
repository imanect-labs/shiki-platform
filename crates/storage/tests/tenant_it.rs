//! テナント設定（ワークフロー実行履歴の保持期間）の結合テスト（#448）。
//!
//! 検証:
//! - 既定は 90 日（migration 0064）で、設定が `get` から読み戻せる
//! - **active なテナントにしか効かない**（撤去中/tombstone は更新しない = 戻り値 false）
//! - 範囲検証が境界を正しく弾く
//!
//! `STORAGE_TEST_DATABASE_URL` 未設定ならスキップ（他の結合テストと同じ env ゲート）。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::tenant::{
    validate_workflow_retention_days, TenantStore, MAX_WORKFLOW_RETENTION_DAYS,
    MIN_WORKFLOW_RETENTION_DAYS,
};
use uuid::Uuid;

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

#[tokio::test]
async fn workflow_retention_is_readable_and_writable() {
    let Some(pool) = setup().await else { return };
    let store = TenantStore::new(pool);
    let tenant = format!("t{}", Uuid::new_v4().simple());

    let created = store
        .upsert_active(&tenant, "org", "retention test")
        .await
        .unwrap();
    assert_eq!(
        created.workflow_retention_days, 90,
        "既定は 90 日（migration 0064 の DEFAULT）"
    );

    assert!(store.set_workflow_retention_days(&tenant, 7).await.unwrap());
    let after = store.get(&tenant).await.unwrap().unwrap();
    assert_eq!(
        after.workflow_retention_days, 7,
        "設定値が読み戻せる（GC はこの値で消す）"
    );
}

#[tokio::test]
async fn workflow_retention_only_applies_to_active_tenants() {
    let Some(pool) = setup().await else { return };
    let store = TenantStore::new(pool);
    let tenant = format!("t{}", Uuid::new_v4().simple());
    store
        .upsert_active(&tenant, "org", "retention test")
        .await
        .unwrap();

    // 撤去処理中へ遷移すると設定は通らない（進行中の purge と設定変更を競合させない）。
    store.mark_deleting(&tenant).await.unwrap();
    assert!(
        !store
            .set_workflow_retention_days(&tenant, 30)
            .await
            .unwrap(),
        "deleting のテナントは更新しない（API は 404 を返す）"
    );

    store.mark_deleted(&tenant).await.unwrap();
    assert!(
        !store
            .set_workflow_retention_days(&tenant, 30)
            .await
            .unwrap(),
        "tombstone も更新しない"
    );

    // 存在しないテナントも false（誤って作らない）。
    assert!(!store
        .set_workflow_retention_days("t-nonexistent", 30)
        .await
        .unwrap());
}

#[test]
fn workflow_retention_range_is_enforced_at_the_boundaries() {
    for ok in [MIN_WORKFLOW_RETENTION_DAYS, 90, MAX_WORKFLOW_RETENTION_DAYS] {
        assert!(validate_workflow_retention_days(ok).is_ok(), "{ok} は許可");
    }
    for ng in [
        MIN_WORKFLOW_RETENTION_DAYS - 1,
        MAX_WORKFLOW_RETENTION_DAYS + 1,
        -1,
    ] {
        assert!(validate_workflow_retention_days(ng).is_err(), "{ng} は拒否");
    }
}
