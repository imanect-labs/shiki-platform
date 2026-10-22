//! 監査ログのハッシュチェーンの検証（#828）の結合テスト。
//!
//! 検証:
//! - 記録したままなら末尾まで崩れていない（版 1 の古い行が混ざっていても）。
//! - 行の時刻・metadata の書き換えは、その行の「内容が違う」として検知する。
//! - 行の削除は、次の行の「つながっていない」として検知する。
//! - `after_id` で続きから確かめられる（上限で区切っても漏れない）。
//! - テナント ID の変更（retenant）より前の行は、旧 tenant_id で検証できる。
//!
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
use chrono::{SubsecRound, Utc};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::audit::{
    entry_hash, record_on, AuditAction, AuditEntry, AuditRecorder, Chain, Decision, HashInput,
};
use storage::audit_chain::ChainBreak;
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

/// 連結行を 1 件記録し、その id を返す。
async fn chained(pool: &PgPool, c: &AuthContext, n: i64) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    record_on(
        &mut tx,
        c,
        AuditEntry {
            action: AuditAction::FileDelete,
            object_type: "file",
            object_id: &format!("f{n}"),
            decision: Decision::Allow,
            trace_id: Some("tr"),
            metadata: json!({"n": n, "nested": {"b": 1, "a": [1.5, "x"]}}),
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

/// #828 より前の形式（版 1・作成時刻を含まない）の連結行を、チェーンの先頭として直接入れる。
async fn legacy_head(pool: &PgPool, c: &AuthContext) {
    let metadata = r#"{"legacy":true}"#;
    let hash = entry_hash(&HashInput {
        version: 1,
        prev_hash: None,
        tenant_id: &c.tenant_id,
        org: &c.org,
        actor: "alice",
        action: "folder.create",
        object_type: "folder",
        object_id: "d1",
        decision: "allow",
        trace_id: None,
        metadata,
        created_at: Utc::now().trunc_subsecs(6),
    });
    sqlx::query(
        "INSERT INTO audit_log (tenant_id, org, actor, action, object_type, object_id, decision, \
         metadata, chained, prev_hash, entry_hash) \
         VALUES ($1, $2, 'alice', 'folder.create', 'folder', 'd1', 'allow', $3::jsonb, true, NULL, $4)",
    )
    .bind(&c.tenant_id)
    .bind(&c.org)
    .bind(metadata)
    .bind(hash)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn untouched_chain_verifies_and_pages() {
    let Some(pool) = setup().await else { return };
    let c = ctx(&format!("chain-{}", Uuid::new_v4().simple()), "org");
    legacy_head(&pool, &c).await;
    let ids = [
        chained(&pool, &c, 1).await,
        chained(&pool, &c, 2).await,
        chained(&pool, &c, 3).await,
    ];
    // 連結しない行（読み取り）はチェーンの外。
    AuditRecorder::new(pool.clone())
        .record(
            &c,
            AuditEntry {
                action: AuditAction::FileMetadataRead,
                object_type: "file",
                object_id: "f1",
                decision: Decision::Allow,
                trace_id: None,
                metadata: json!({}),
            },
        )
        .await
        .unwrap();

    let v = AuditRecorder::new(pool.clone()).chain_verifier();
    let all = v.verify(&c, None, 1000).await.unwrap();
    assert_eq!(all.broken, None);
    assert!(all.complete);
    assert_eq!((all.checked, all.legacy), (4, 1));
    assert_eq!(all.last_id, Some(ids[2]));

    // 上限で区切って続きから確かめても、同じ結果になる。
    let first = v.verify(&c, None, 2).await.unwrap();
    assert!(!first.complete);
    assert_eq!(first.checked, 2);
    assert_eq!(first.last_id, Some(ids[0]));
    let rest = v.verify(&c, first.last_id, 1000).await.unwrap();
    assert_eq!((rest.checked, rest.complete, rest.broken), (2, true, None));
    // 別のチェーンの行や存在しない行からは続けられない。
    let other = ctx(&format!("chain-{}", Uuid::new_v4().simple()), "org");
    assert!(v.verify(&other, Some(ids[0]), 10).await.is_err());
}

#[tokio::test]
async fn tampering_is_detected_at_the_first_broken_row() {
    let Some(pool) = setup().await else { return };
    let c = ctx(&format!("chain-{}", Uuid::new_v4().simple()), "org");
    let ids = [
        chained(&pool, &c, 1).await,
        chained(&pool, &c, 2).await,
        chained(&pool, &c, 3).await,
    ];
    let v = AuditRecorder::new(pool.clone()).chain_verifier();

    // 時刻の書き換え（版 2 は作成時刻を含む）。
    sqlx::query("UPDATE audit_log SET created_at = created_at - interval '1 day' WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    let r = v.verify(&c, None, 1000).await.unwrap();
    let broken = r.broken.expect("時刻の書き換えを検知する");
    assert_eq!(
        (broken.id, broken.reason),
        (ids[1], ChainBreak::HashMismatch)
    );
    assert_eq!(r.checked, 2);
    sqlx::query("UPDATE audit_log SET created_at = created_at + interval '1 day' WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        v.verify(&c, None, 1000).await.unwrap().broken,
        None,
        "戻せば通る"
    );

    // metadata の書き換え。
    sqlx::query("UPDATE audit_log SET metadata = metadata || '{\"n\": 99}' WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    let broken = v.verify(&c, None, 1000).await.unwrap().broken.unwrap();
    assert_eq!(
        (broken.id, broken.reason),
        (ids[1], ChainBreak::HashMismatch)
    );

    // 行の削除は、次の行がつながらない。
    sqlx::query("DELETE FROM audit_log WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    let broken = v.verify(&c, None, 1000).await.unwrap().broken.unwrap();
    assert_eq!(
        (broken.id, broken.reason),
        (ids[2], ChainBreak::PrevMismatch)
    );
}

#[tokio::test]
async fn rows_before_a_retenant_verify_with_the_old_tenant() {
    let Some(pool) = setup().await else { return };
    let old = format!("chain-{}", Uuid::new_v4().simple());
    let new = format!("chain-{}", Uuid::new_v4().simple());
    let before = ctx(&old, "org");
    chained(&pool, &before, 1).await;
    chained(&pool, &before, 2).await;
    // retenant と同じく行の tenant_id を書き換え、新テナントのチェーンに変更の記録を連結する。
    sqlx::query("UPDATE audit_log SET tenant_id = $2 WHERE tenant_id = $1")
        .bind(&old)
        .bind(&new)
        .execute(&pool)
        .await
        .unwrap();
    let after = ctx(&new, "org");
    let mut tx = pool.begin().await.unwrap();
    record_on(
        &mut tx,
        &after,
        AuditEntry {
            action: AuditAction::TenantRetenant,
            object_type: "organization",
            object_id: "org",
            decision: Decision::Allow,
            trace_id: None,
            metadata: json!({"from": old, "to": new}),
        },
        Chain::Yes,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    chained(&pool, &after, 3).await;

    let r = AuditRecorder::new(pool.clone())
        .chain_verifier()
        .verify(&after, None, 1000)
        .await
        .unwrap();
    assert_eq!(r.broken, None);
    assert_eq!((r.checked, r.complete), (4, true));
}
