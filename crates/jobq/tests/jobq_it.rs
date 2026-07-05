//! jobq の結合テスト（実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行し、未設定なら early-return で
//! スキップする（素の `cargo test` を壊さない）。CI の coverage ジョブで実走する。
//!
//! 検証: enqueue → claim（vt で不可視化・SKIP LOCKED の二重確保防止）→ ack / fail
//! （バックオフ再配信・上限で DLQ）→ requeue_dead → テナント消去。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::time::Duration;

use jobq::{FailOutcome, NewJob};
use sqlx::{postgres::PgPoolOptions, PgPool};

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

/// テスト間の干渉を避けるため、テストごとに一意なキュー名を使う。
fn unique_queue(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

async fn enqueue(pool: &PgPool, queue: &str, tenant: &str, max_attempts: i32) -> i64 {
    let mut conn = pool.acquire().await.unwrap();
    let payload = serde_json::json!({"k": "v"});
    jobq::enqueue_on(
        &mut conn,
        NewJob {
            queue,
            tenant_id: tenant,
            payload: &payload,
            trace_id: Some("trace-1"),
            max_attempts,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn claim_hides_job_until_vt_and_ack_deletes() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("vt");
    let id = enqueue(&pool, &queue, "a-corp", 5).await;

    let mut conn = pool.acquire().await.unwrap();
    let claimed = jobq::claim(&mut conn, &queue, Duration::from_secs(300), 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, id);
    assert_eq!(claimed[0].attempts, 1);
    assert_eq!(claimed[0].tenant_id, "a-corp");
    assert_eq!(claimed[0].trace_id.as_deref(), Some("trace-1"));

    // vt 内は再 claim で見えない（可視性タイムアウト）。
    let again = jobq::claim(&mut conn, &queue, Duration::from_secs(300), 10)
        .await
        .unwrap();
    assert!(again.is_empty(), "vt 内のジョブは再配信されない");

    // ack で消える。二重 ack も冪等。
    jobq::ack(&mut conn, id).await.unwrap();
    jobq::ack(&mut conn, id).await.unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from job_queue where queue = $1")
        .bind(&queue)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn crashed_consumer_job_is_redelivered_after_vt() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("redeliver");
    let id = enqueue(&pool, &queue, "a-corp", 5).await;

    let mut conn = pool.acquire().await.unwrap();
    // vt=0 で claim（クラッシュした consumer の代役: ack も fail もしない）。
    let claimed = jobq::claim(&mut conn, &queue, Duration::from_secs(0), 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);

    // vt 経過後は自動で再配信され attempts が増える。
    let again = jobq::claim(&mut conn, &queue, Duration::from_secs(300), 10)
        .await
        .unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].id, id);
    assert_eq!(again[0].attempts, 2);
}

#[tokio::test]
async fn fail_backs_off_then_moves_to_dlq_and_requeue_recovers() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("dlq");
    let id = enqueue(&pool, &queue, "a-corp", 2).await;
    let mut conn = pool.acquire().await.unwrap();

    // 1 回目の失敗: attempts=1 < max=2 なのでバックオフ再配信。
    let claimed = jobq::claim(&mut conn, &queue, Duration::from_secs(300), 10)
        .await
        .unwrap();
    let outcome = jobq::fail(&mut conn, claimed[0].id, "boom", Duration::from_secs(0))
        .await
        .unwrap();
    assert_eq!(outcome, FailOutcome::Retry { attempts: 1 });

    // 2 回目の失敗: attempts=2 >= max=2 で DLQ へ。
    let claimed = jobq::claim(&mut conn, &queue, Duration::from_secs(300), 10)
        .await
        .unwrap();
    assert_eq!(claimed[0].attempts, 2);
    let outcome = jobq::fail(
        &mut conn,
        claimed[0].id,
        "boom again",
        Duration::from_secs(0),
    )
    .await
    .unwrap();
    assert_eq!(outcome, FailOutcome::Dead);

    // 待機列からは消え、DLQ に last_error 付きで残る。
    let remaining = jobq::claim(&mut conn, &queue, Duration::from_secs(300), 10)
        .await
        .unwrap();
    assert!(remaining.is_empty());
    let dead = jobq::dead_jobs(&mut conn, &queue, 10).await.unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].id, id);
    assert_eq!(dead[0].last_error, "boom again");

    // requeue で待機列へ戻り、attempts はリセットされる。
    assert!(jobq::requeue_dead(&mut conn, id).await.unwrap());
    assert!(
        !jobq::requeue_dead(&mut conn, id).await.unwrap(),
        "二重 requeue は no-op"
    );
    let revived = jobq::claim(&mut conn, &queue, Duration::from_secs(300), 10)
        .await
        .unwrap();
    assert_eq!(revived.len(), 1);
    assert_eq!(revived[0].attempts, 1, "requeue 後は試行回数リセット");
}

#[tokio::test]
async fn fail_on_missing_job_is_noop() {
    let Some(pool) = setup().await else { return };
    let mut conn = pool.acquire().await.unwrap();
    let outcome = jobq::fail(&mut conn, i64::MAX, "gone", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(outcome, FailOutcome::Retry { attempts: 0 });
}

#[tokio::test]
async fn concurrent_claims_do_not_double_deliver() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("skiplock");
    for _ in 0..10 {
        enqueue(&pool, &queue, "a-corp", 5).await;
    }

    // 2 本の接続から同時に claim しても同一ジョブは重複しない（SKIP LOCKED）。
    let mut c1 = pool.acquire().await.unwrap();
    let mut c2 = pool.acquire().await.unwrap();
    let (a, b) = tokio::join!(
        jobq::claim(&mut c1, &queue, Duration::from_secs(300), 7),
        jobq::claim(&mut c2, &queue, Duration::from_secs(300), 7),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.len() + b.len(), 10);
    let mut ids: Vec<i64> = a.iter().chain(b.iter()).map(|j| j.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 10, "二重配信なし");
}

#[tokio::test]
async fn tenant_erasure_drops_queued_and_dead_jobs() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("erase");
    let dead_id = enqueue(&pool, &queue, "erase-corp", 1).await;
    enqueue(&pool, &queue, "erase-corp", 5).await;
    enqueue(&pool, &queue, "other-corp", 5).await;
    let mut conn = pool.acquire().await.unwrap();

    // 1 件を DLQ へ落としてから消去する。
    let claimed = jobq::claim(&mut conn, &queue, Duration::from_secs(0), 1)
        .await
        .unwrap();
    assert_eq!(claimed[0].id, dead_id);
    jobq::fail(&mut conn, dead_id, "die", Duration::from_secs(0))
        .await
        .unwrap();

    let deleted = jobq::delete_tenant(&mut conn, "erase-corp").await.unwrap();
    assert_eq!(deleted, 2, "待機列 1 + DLQ 1");
    let survivors = jobq::claim(&mut conn, &queue, Duration::from_secs(300), 10)
        .await
        .unwrap();
    assert_eq!(survivors.len(), 1);
    assert_eq!(survivors[0].tenant_id, "other-corp");
}
