//! 監査ログの閲覧（#634）の結合テスト。
//!
//! 検証:
//! - 範囲は `AuthContext` の tenant_id ＋ org に閉じる（同じ org 名の別テナントの行は出ない）。
//! - 条件（期間・操作した人〔ID とメール〕・操作の前方一致・判定・引用チャンク・trace_id）で絞れる。
//! - keyset ページングで漏れも重複もなく全件たどれる。
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
use chrono::{Duration, Utc};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use storage::audit_view::{AuditCursor, AuditDecision, AuditFilter};
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

fn ctx(tenant: &str, org: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        tenant.into(),
    )
}

async fn record(
    rec: &AuditRecorder,
    c: &AuthContext,
    action: &str,
    decision: Decision,
    trace: Option<&str>,
    metadata: serde_json::Value,
) {
    rec.record(
        c,
        AuditEntry {
            action,
            object_type: "file",
            object_id: "f1",
            decision,
            trace_id: trace,
            metadata,
        },
    )
    .await
    .expect("record");
}

fn window() -> AuditFilter {
    AuditFilter {
        from: Utc::now() - Duration::hours(1),
        to: Utc::now() + Duration::hours(1),
        ..AuditFilter::default()
    }
}

#[tokio::test]
async fn search_is_scoped_filtered_and_pageable() {
    let Some(pool) = setup().await else { return };
    // テストごとに別テナント（同じ org 名を 2 テナントで共有して越境しないことを見る）。
    let t1 = format!("audit-{}", Uuid::new_v4().simple());
    let t2 = format!("audit-{}", Uuid::new_v4().simple());
    let org = "shared-org";
    sqlx::query(
        "INSERT INTO directory_user (user_id, tenant_id, org, email, display_name) \
         VALUES ('bob', $1, $2, 'Bob@Example.com', 'ボブ')",
    )
    .bind(&t1)
    .bind(org)
    .execute(&pool)
    .await
    .expect("directory");

    let rec = AuditRecorder::new(pool.clone());
    let alice = ctx(&t1, org, "alice");
    let bob = ctx(&t1, org, "bob");
    let other = ctx(&t2, org, "alice");
    record(
        &rec,
        &alice,
        "file.delete",
        Decision::Allow,
        Some("tr-1"),
        json!({}),
    )
    .await;
    record(
        &rec,
        &bob,
        "file.download_url.issue",
        Decision::Deny,
        None,
        json!({}),
    )
    .await;
    record(
        &rec,
        &bob,
        "rag.search",
        Decision::Allow,
        Some("tr-2"),
        json!({"cited_chunk_ids": ["chunk-a", "chunk-b"]}),
    )
    .await;
    record(
        &rec,
        &alice,
        "folder.create",
        Decision::Allow,
        None,
        json!({}),
    )
    .await;
    record(
        &rec,
        &other,
        "file.delete",
        Decision::Allow,
        None,
        json!({}),
    )
    .await;

    let viewer = rec.viewer();
    let all = viewer.search(&alice, &window(), None, 50).await.unwrap();
    assert_eq!(all.len(), 4, "別テナントの行は出ない");
    assert_eq!(all[0].action, "folder.create", "新しい順");
    let bob_row = all.iter().find(|e| e.actor == "bob").unwrap();
    assert_eq!(bob_row.actor_name.as_deref(), Some("ボブ"));

    let by = |f: AuditFilter| {
        let viewer = viewer.clone();
        let alice = alice.clone();
        async move { viewer.search(&alice, &f, None, 50).await.unwrap() }
    };
    let actor_by_email = by(AuditFilter {
        actor: Some("bob@example.com".into()),
        ..window()
    })
    .await;
    assert_eq!(actor_by_email.len(), 2, "メールは大文字小文字を区別しない");
    let actor_by_id = by(AuditFilter {
        actor: Some("alice".into()),
        ..window()
    })
    .await;
    assert_eq!(actor_by_id.len(), 2);
    let prefix = by(AuditFilter {
        action: Some("file.".into()),
        ..window()
    })
    .await;
    assert_eq!(prefix.len(), 2);
    let exact = by(AuditFilter {
        action: Some("file".into()),
        ..window()
    })
    .await;
    assert!(exact.is_empty(), "末尾の . が無ければ完全一致");
    let denied = by(AuditFilter {
        decision: Some(AuditDecision::Deny),
        ..window()
    })
    .await;
    assert_eq!(denied.len(), 1);
    let cited = by(AuditFilter {
        chunk_id: Some("chunk-b".into()),
        ..window()
    })
    .await;
    assert_eq!(cited.len(), 1);
    assert_eq!(cited[0].action, "rag.search");
    let traced = by(AuditFilter {
        trace_id: Some("tr-1".into()),
        ..window()
    })
    .await;
    assert_eq!(traced.len(), 1);
    let past = by(AuditFilter {
        from: Utc::now() - Duration::days(3),
        to: Utc::now() - Duration::days(2),
        ..AuditFilter::default()
    })
    .await;
    assert!(past.is_empty(), "期間外は出ない");

    // 2 件ずつのページで、漏れも重複もなく 4 件たどれる。
    let mut seen = vec![];
    let mut after: Option<AuditCursor> = None;
    loop {
        let page = viewer.search(&alice, &window(), after, 2).await.unwrap();
        if page.is_empty() {
            break;
        }
        let last = page.last().unwrap();
        after = Some(AuditCursor {
            created_at: last.created_at,
            id: last.id,
        });
        seen.extend(page.iter().map(|e| e.id));
    }
    let mut ids: Vec<i64> = all.iter().map(|e| e.id).collect();
    assert_eq!(seen, ids);
    ids.dedup();
    assert_eq!(ids.len(), 4);
}
