//! 接続非依存生成（Task 3.11）の整合性不変条件の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定なら early-return skip）。
//! OpenFGA は使わずモック AuthzClient（AllowAll）で置換し、DB 上の generation_run/event の
//! claim/lease/fencing/exactly-once/cancel と outbox 投入を検証する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::{ChatStore, CHAT_GENERATION_QUEUE};
use sqlx::{postgres::PgPoolOptions, PgPool};

/// 全許可のモック authz（DB ロジックの検証に集中するため）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

async fn setup() -> Option<PgPool> {
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

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

async fn store(pool: &PgPool) -> ChatStore {
    ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .expect("chat store")
}

#[tokio::test]
async fn post_message_is_transactional_outbox() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);

    let thread = store
        .create_thread(&c, "テスト", false, None)
        .await
        .unwrap();
    let res = store
        .post_message(&c, thread.id, "経費規程は？", &[], None, false, None)
        .await
        .unwrap();

    // user/assistant message と run 行が同一 TX で作られている。
    let msg_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM message WHERE thread_id = $1 AND tenant_id = $2")
            .bind(thread.id)
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(msg_count, 2, "user+assistant の 2 メッセージ");

    // outbox: 同一 TX で job_queue に enqueue されている。並行実行のワーカー（別テスト）が
    // 先に claim すると行が不可視化/削除され得るため、claim せず直接 SELECT し、さらに
    // 「既にワーカーが claim して run が queued を抜けた」ケースも enqueue の証左として許容する。
    let in_queue: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job_queue WHERE queue = $1 AND payload->>'run_id' = $2",
    )
    .bind(CHAT_GENERATION_QUEUE)
    .bind(res.run_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    let claimed_by_worker = store
        .run_status(res.run_id)
        .await
        .unwrap()
        .is_some_and(|s| s != chat::RunStatus::Queued);
    assert!(
        in_queue > 0 || claimed_by_worker,
        "run_id が job_queue に enqueue されている（または既にワーカーが claim 済み）"
    );
}

#[tokio::test]
async fn claim_lease_fencing_and_exactly_once_append() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store.create_thread(&c, "t", false, None).await.unwrap();
    let res = store
        .post_message(&c, thread.id, "hi", &[], None, false, None)
        .await
        .unwrap();
    let run_id = res.run_id;

    // 1 回目 claim（fencing=1）。
    let r1 = store.claim_run(run_id, "w1", 30).await.unwrap().unwrap();
    assert_eq!(r1.fencing_token, 1);

    // 有効リース中の 2 回目 claim は None。
    assert!(store.claim_run(run_id, "w2", 30).await.unwrap().is_none());

    // イベント追記は単調 seq（exactly-once）。
    let s1 = store
        .append_stream_event(
            run_id,
            1,
            &chat::StreamEventKind::Token { text: "a".into() },
        )
        .await
        .unwrap();
    let s2 = store
        .append_stream_event(
            run_id,
            1,
            &chat::StreamEventKind::Token { text: "b".into() },
        )
        .await
        .unwrap();
    assert_eq!(s1, Some(1));
    assert_eq!(s2, Some(2));

    // リースを失効させて takeover（fencing=2）。
    sqlx::query(
        "UPDATE generation_run SET lease_until = now() - interval '1 second' WHERE run_id = $1",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    let r2 = store.claim_run(run_id, "w2", 30).await.unwrap().unwrap();
    assert_eq!(r2.fencing_token, 2, "takeover で fencing が上がる");

    // 旧 fencing のゾンビ書込は拒否（None）。
    let zombie = store
        .append_stream_event(
            run_id,
            1,
            &chat::StreamEventKind::Token { text: "z".into() },
        )
        .await
        .unwrap();
    assert_eq!(zombie, None, "旧 fencing のゾンビ書込は拒否");

    // 新 fencing なら追記できる（seq は継続）。
    let s3 = store
        .append_stream_event(
            run_id,
            2,
            &chat::StreamEventKind::Token { text: "c".into() },
        )
        .await
        .unwrap();
    assert_eq!(s3, Some(3));
}

#[tokio::test]
async fn cancel_is_cooperative_and_visible_to_claim() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store.create_thread(&c, "t", false, None).await.unwrap();
    let res = store
        .post_message(&c, thread.id, "hi", &[], None, false, None)
        .await
        .unwrap();

    // ユーザー明示停止。
    store
        .request_cancel(&c, thread.id, res.run_id, None)
        .await
        .unwrap();

    // claim すると cancel_requested が見える（ワーカーはキャンセル確定へ）。
    let claimed = store
        .claim_run(res.run_id, "w1", 30)
        .await
        .unwrap()
        .unwrap();
    assert!(
        claimed.cancel_requested,
        "claim 時に cancel_requested が見える"
    );

    // finalize(cancelled) 後、run 状態が cancelled。
    store
        .finalize_run(
            res.run_id,
            claimed.fencing_token,
            chat::RunStatus::Cancelled,
            &[],
            None,
            None,
        )
        .await
        .unwrap();
    let status = store.run_status(res.run_id).await.unwrap();
    assert_eq!(status, Some(chat::RunStatus::Cancelled));
}

#[tokio::test]
async fn replay_returns_events_after_cursor() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool).await;
    let c = ctx(&tenant);
    let thread = store.create_thread(&c, "t", false, None).await.unwrap();
    let res = store
        .post_message(&c, thread.id, "hi", &[], None, false, None)
        .await
        .unwrap();
    let run_id = res.run_id;
    store.claim_run(run_id, "w1", 30).await.unwrap().unwrap();
    for t in ["x", "y", "z"] {
        store
            .append_stream_event(run_id, 1, &chat::StreamEventKind::Token { text: t.into() })
            .await
            .unwrap();
    }
    // cursor=1 以降（seq 2,3）だけ返る（Last-Event-ID 再開・非重複）。
    let events = store.replay_events(run_id, 1).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 2);
    assert_eq!(events[1].seq, 3);
}
