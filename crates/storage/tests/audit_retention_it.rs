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
use storage::audit_chain::ChainBreak;
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

    // 境目の後ろも改ざんを検知する: 残った行の書き換えはその行で、境目の書き換えは境目の次の行で崩れる。
    let tamper = |sql: &'static str| {
        let pool = pool.clone();
        let c = c.clone();
        async move {
            sqlx::query(sql)
                .bind(&c.tenant_id)
                .bind(&c.org)
                .bind(keep)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    tamper(
        "UPDATE audit_log SET metadata = metadata || '{\"n\": 1}' \
         WHERE tenant_id = $1 AND org = $2 AND id = $3",
    )
    .await;
    let broken = rec
        .chain_verifier()
        .verify(&c, None, 1000)
        .await
        .unwrap()
        .broken
        .unwrap();
    assert_eq!((broken.id, broken.reason), (keep, ChainBreak::HashMismatch));
    tamper(
        "UPDATE audit_log SET metadata = metadata - 'n' \
         WHERE tenant_id = $1 AND org = $2 AND id = $3",
    )
    .await;
    tamper(
        "UPDATE audit_chain_anchor SET anchor_hash = 'x' || anchor_hash \
         WHERE tenant_id = $1 AND org = $2 AND anchor_id < $3",
    )
    .await;
    let broken = rec
        .chain_verifier()
        .verify(&c, None, 1000)
        .await
        .unwrap()
        .broken
        .unwrap();
    assert_eq!((broken.id, broken.reason), (keep, ChainBreak::PrevMismatch));
    tamper(
        "UPDATE audit_chain_anchor SET anchor_hash = substr(anchor_hash, 2) \
         WHERE tenant_id = $1 AND org = $2 AND anchor_id < $3",
    )
    .await;
    let v = rec.chain_verifier().verify(&c, None, 1000).await.unwrap();
    assert_eq!(v.broken, None, "戻せば通る");

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
async fn multi_batch_sweep_keeps_its_own_purge_records() {
    // 1 バッチ（2,000 件）を超えて全部が期限切れのとき、消してよい上限は最初のバッチで決めた
    // 「その時点の最後の連結行の次」で、後のバッチが先のバッチの削除の記録を消さないこと。
    let Some(pool) = setup().await else { return };
    let c = ctx(&format!("ret-{}", Uuid::new_v4().simple()), "org");
    let rec = AuditRecorder::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    for _ in 0..2001 {
        record_on(
            &mut tx,
            &c,
            AuditEntry {
                action: AuditAction::FileDelete,
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
    }
    tx.commit().await.unwrap();

    let r = rec
        .retention()
        .sweep_org(&c.tenant_id, &c.org, Utc::now(), 365)
        .await
        .unwrap();
    assert_eq!((r.chained, r.unchained), (2001, 0));
    assert_eq!(
        actions(&pool, &c).await,
        ["audit.retention.purge", "audit.retention.purge"],
        "バッチごとの削除の記録が両方残る"
    );
    let v = rec.chain_verifier().verify(&c, None, 1000).await.unwrap();
    assert_eq!(v.broken, None);
    assert_eq!((v.checked, v.purged_rows), (2, 2001));
}

#[tokio::test]
async fn chained_rows_stop_at_the_first_unexpired_row_in_id_order() {
    // 作成時刻はレプリカの時計で決まり id 順と逆転しうる。id 順で最初の期限内の行より後ろは、
    // 期限切れでも消さない（間の期限内の行を消してチェーンに穴を開けない・Codex P2）。
    let Some(pool) = setup().await else { return };
    let c = ctx(&format!("ret-{}", Uuid::new_v4().simple()), "org");
    let rec = AuditRecorder::new(pool.clone());
    let a = chained(&pool, &c, AuditAction::FileDelete).await;
    let b = chained(&pool, &c, AuditAction::FileDelete).await;
    let d = chained(&pool, &c, AuditAction::FileDelete).await;
    let cutoff = Utc::now();
    // b だけ期限内（時計が進んでいたレプリカで書かれた行）。
    sqlx::query("UPDATE audit_log SET created_at = $2 WHERE id = $1")
        .bind(b)
        .bind(cutoff + chrono::Duration::days(1))
        .execute(&pool)
        .await
        .unwrap();
    let r = rec
        .retention()
        .sweep_org(&c.tenant_id, &c.org, cutoff, 365)
        .await
        .unwrap();
    assert_eq!(
        r.chained, 1,
        "a だけ消す（d は期限切れでも b より後ろなので残す）"
    );
    let left: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM audit_log WHERE tenant_id = $1 AND org = $2 AND id <= $3 ORDER BY id",
    )
    .bind(&c.tenant_id)
    .bind(&c.org)
    .bind(d)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(left, [b, d]);
    let anchor: i64 = sqlx::query_scalar(
        "SELECT anchor_id FROM audit_chain_anchor WHERE tenant_id = $1 AND org = $2",
    )
    .bind(&c.tenant_id)
    .bind(&c.org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(anchor, a);
}

#[tokio::test]
async fn verification_resumes_from_the_anchor_when_its_position_was_purged() {
    let Some(pool) = setup().await else { return };
    let c = ctx(&format!("ret-{}", Uuid::new_v4().simple()), "org");
    let rec = AuditRecorder::new(pool.clone());
    let first = chained(&pool, &c, AuditAction::FileDelete).await;
    chained(&pool, &c, AuditAction::FileDelete).await;
    let cutoff = Utc::now();
    chained(&pool, &c, AuditAction::FolderCreate).await;
    // 検証の続きの位置（first）が、呼び出しの間に保持期間で消えた。
    rec.retention()
        .sweep_org(&c.tenant_id, &c.org, cutoff, 365)
        .await
        .unwrap();
    let v = rec
        .chain_verifier()
        .verify(&c, Some(first), 1000)
        .await
        .unwrap();
    assert_eq!(v.broken, None, "境目の続きから確かめる");
    assert_eq!((v.checked, v.complete), (2, true));
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
