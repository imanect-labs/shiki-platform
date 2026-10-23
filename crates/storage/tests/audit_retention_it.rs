//! 監査ログの保持期間（#831）の結合テスト。
//!
//! 検証:
//! - 期限より古い行を消し（連結行は先頭から連続して）、消した最後の連結行を境目として残す。
//! - 境目から検証すると、残った行と削除の記録がつながっている。
//! - テナント撤去の記録（tenant.purge）と、それより後ろの連結行は消さない。
//! - 保持日数の設定は active なテナントだけ・範囲内だけ。変更は監査に残る。
//!
//! 全テナントを掃除する `sweep` は、同じ DB を使うほかのテストの行まで消しうるので呼ばない
//! （テナント・org を指定する `sweep_org` で確かめる）。
//! `STORAGE_TEST_DATABASE_URL` 未設定ならスキップ（他の結合テストと同じ env ゲート）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use authz::{AuthContext, Principal, PrincipalKind};
use chrono::Utc;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::audit::{record_on, AuditAction, AuditEntry, AuditRecorder, Chain, Decision};
use storage::audit_retention::DEFAULT_RETENTION_DAYS;
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

fn ctx(tenant: &str, org: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        tenant.into(),
    )
}

async fn chained(pool: &PgPool, c: &AuthContext, action: AuditAction) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    record_on(
        &mut tx,
        c,
        AuditEntry {
            action,
            object_type: "file",
            object_id: "f",
            decision: Decision::Allow,
            trace_id: None,
            metadata: json!({}),
        },
        Chain::Yes,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query_scalar("SELECT max(id) FROM audit_log WHERE tenant_id = $1 AND org = $2")
        .bind(&c.tenant_id)
        .bind(&c.org)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn read(rec: &AuditRecorder, c: &AuthContext) {
    rec.record(
        c,
        AuditEntry {
            action: AuditAction::FileMetadataRead,
            object_type: "file",
            object_id: "f",
            decision: Decision::Allow,
            trace_id: None,
            metadata: json!({}),
        },
    )
    .await
    .unwrap();
}

async fn actions(pool: &PgPool, c: &AuthContext) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log WHERE tenant_id = $1 AND org = $2 ORDER BY id")
        .bind(&c.tenant_id)
        .bind(&c.org)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn expired_rows_are_purged_and_the_chain_verifies_from_the_anchor() {
    let Some(pool) = setup().await else { return };
    let c = ctx(&format!("ret-{}", Uuid::new_v4().simple()), "org");
    let rec = AuditRecorder::new(pool.clone());
    // 期限より古い行（連結 3・連結しない 2）。
    chained(&pool, &c, AuditAction::FileDelete).await;
    read(&rec, &c).await;
    chained(&pool, &c, AuditAction::FileDelete).await;
    let last_old = chained(&pool, &c, AuditAction::FileDelete).await;
    read(&rec, &c).await;
    let cutoff = Utc::now();
    // 期限より新しい行。
    let keep = chained(&pool, &c, AuditAction::FolderCreate).await;
    read(&rec, &c).await;

    let r = rec
        .retention()
        .sweep_org(&c.tenant_id, &c.org, cutoff, 365)
        .await
        .unwrap();
    assert_eq!((r.chained, r.unchained), (3, 2));
    assert_eq!(
        actions(&pool, &c).await,
        [
            "folder.create",
            "file.metadata.read",
            "audit.retention.purge"
        ],
        "新しい行と削除の記録だけが残る"
    );

    let v = rec.chain_verifier().verify(&c, None, 1000).await.unwrap();
    assert_eq!(v.broken, None, "境目から検証すると残りがつながっている");
    assert_eq!(v.anchor_id, Some(last_old));
    assert_eq!(v.purged_rows, 5);
    assert_eq!(
        (v.checked, v.complete),
        (2, true),
        "残った連結行と削除の記録"
    );
    assert!(v.last_id > Some(keep));

    // もう一度回しても、消すものが無ければ記録も増えない。
    let again = rec
        .retention()
        .sweep_org(&c.tenant_id, &c.org, cutoff, 365)
        .await
        .unwrap();
    assert_eq!((again.chained, again.unchained), (0, 0));
    assert_eq!(actions(&pool, &c).await.len(), 3);
}

#[tokio::test]
async fn when_every_row_expires_new_rows_link_to_the_anchor() {
    let Some(pool) = setup().await else { return };
    let c = ctx(&format!("ret-{}", Uuid::new_v4().simple()), "org");
    let rec = AuditRecorder::new(pool.clone());
    chained(&pool, &c, AuditAction::FileDelete).await;
    let last = chained(&pool, &c, AuditAction::FileDelete).await;
    // 期限より新しい連結行が無い（全部消える）。削除の記録は境目につながる。
    let r = rec
        .retention()
        .sweep_org(&c.tenant_id, &c.org, Utc::now(), 365)
        .await
        .unwrap();
    assert_eq!(r.chained, 2);
    assert_eq!(actions(&pool, &c).await, ["audit.retention.purge"]);
    // その後に増えた行も同じチェーンにつながる。
    chained(&pool, &c, AuditAction::FolderCreate).await;
    let v = rec.chain_verifier().verify(&c, None, 1000).await.unwrap();
    assert_eq!(v.broken, None, "削除の記録が境目につながっている");
    assert_eq!((v.anchor_id, v.checked), (Some(last), 2));
}

#[tokio::test]
async fn the_tenant_purge_record_and_rows_after_it_are_kept() {
    let Some(pool) = setup().await else { return };
    let c = ctx(&format!("ret-{}", Uuid::new_v4().simple()), "org");
    let rec = AuditRecorder::new(pool.clone());
    let before = chained(&pool, &c, AuditAction::FileDelete).await;
    chained(&pool, &c, AuditAction::TenantPurge).await;
    // 撤去の後に増える連結行（削除の記録など）も、撤去の記録の後ろなので消さない。
    chained(&pool, &c, AuditAction::FolderCreate).await;
    let r = rec
        .retention()
        .sweep_org(&c.tenant_id, &c.org, Utc::now(), 365)
        .await
        .unwrap();
    assert_eq!(r.chained, 1, "撤去の記録より前だけを消す");
    assert_eq!(
        actions(&pool, &c).await,
        ["tenant.purge", "folder.create", "audit.retention.purge"]
    );
    let v = rec.chain_verifier().verify(&c, None, 1000).await.unwrap();
    assert_eq!(v.broken, None);
    assert_eq!(v.anchor_id, Some(before));
}

#[tokio::test]
async fn retention_days_are_set_for_active_tenants_and_audited() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("ret-{}", Uuid::new_v4().simple());
    let retention = AuditRecorder::new(pool.clone()).retention();
    // 台帳に無いテナントは既定値で消す（オンプレなど台帳を使わない構成）。
    assert_eq!(
        retention.retention_days(&tenant).await.unwrap(),
        DEFAULT_RETENTION_DAYS
    );
    assert_eq!(
        retention
            .set_retention_days(&tenant, 730, "cli", json!({}))
            .await
            .unwrap(),
        None,
        "台帳に無いテナントは設定できない"
    );
    storage::tenant::TenantStore::new(pool.clone())
        .upsert_active(&tenant, "org", "テスト")
        .await
        .unwrap();
    assert_eq!(retention.retention_days(&tenant).await.unwrap(), 365);
    assert!(retention
        .set_retention_days(&tenant, 30, "cli", json!({}))
        .await
        .is_err());
    assert_eq!(
        retention
            .set_retention_days(&tenant, 730, "provisioner:test", json!({"k": 1}))
            .await
            .unwrap(),
        Some(365)
    );
    assert_eq!(retention.retention_days(&tenant).await.unwrap(), 730);
    let (actor, metadata, chained): (String, serde_json::Value, bool) = sqlx::query_as(
        "SELECT actor, metadata, chained FROM audit_log \
         WHERE tenant_id = $1 AND action = 'tenant.audit_retention.set'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(actor, "provisioner:test");
    assert!(chained, "設定の変更はチェーンに連結する");
    assert_eq!(
        metadata,
        json!({"k": 1, "retention_days": 730, "previous": 365})
    );
}
