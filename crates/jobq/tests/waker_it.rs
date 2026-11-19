//! jobq の待機（LISTEN/NOTIFY・#1332）の結合テスト（実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行する（未設定ならスキップ）。
//!
//! 検証: 投入のコミットで待機中の consumer が起きる／ロールバックした投入では起きない／
//! 時刻で見えるようになるジョブ（defer・バックオフ明け）まで眠り、長い上限まで寝過ごさない／
//! DLQ からの再投入でも起きる／LISTEN を持たない Waker は従来どおり `poll` だけ眠る。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::time::{Duration, Instant};

use jobq::{NewJob, Waker};
use sqlx::{postgres::PgPoolOptions, PgConnection, PgPool};

/// 通知で起きなかったときに眠る時間。これより十分短く返れば「通知（か可視時刻）で起きた」。
const LONG: Duration = Duration::from_secs(30);

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
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

fn unique_queue(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// LISTEN を張り終えた Waker（張る前は poll で眠るので、待たないと検証にならない）。
async fn listening_waker(pool: &PgPool) -> Waker {
    let waker = Waker::listen_with_idle_max(vec![pool.clone()], LONG);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !waker.is_listening() {
        assert!(Instant::now() < deadline, "LISTEN を張れること");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    waker
}

async fn enqueue_on(conn: &mut PgConnection, queue: &str, max_attempts: i32) -> i64 {
    let payload = serde_json::json!({"k": "v"});
    jobq::enqueue_on(
        conn,
        NewJob {
            queue,
            tenant_id: "a-corp",
            payload: &payload,
            trace_id: None,
            max_attempts,
        },
    )
    .await
    .unwrap()
}

/// `wait` を別タスクで始め、眠りに入ってから `act` を実行し、`wait` が返るまでの時間を測る。
async fn elapsed_after<F, Fut>(waker: &Waker, queue: &str, act: F) -> Duration
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let w = waker.clone();
    let q = queue.to_string();
    let waiting = tokio::spawn(async move {
        w.wait(&q, LONG).await;
        Instant::now()
    });
    // 眠りに入る（可視時刻を引いて select に入る）まで待つ。
    tokio::time::sleep(Duration::from_millis(300)).await;
    let start = Instant::now();
    act().await;
    let woke = tokio::time::timeout(LONG + Duration::from_secs(5), waiting)
        .await
        .expect("wait が返ること")
        .unwrap();
    woke.saturating_duration_since(start)
}

#[tokio::test]
async fn committed_enqueue_wakes_the_waiting_consumer() {
    let Some(pool) = setup().await else { return };
    let waker = listening_waker(&pool).await;
    let queue = unique_queue("wake");
    let took = elapsed_after(&waker, &queue, || async {
        let mut conn = pool.acquire().await.unwrap();
        enqueue_on(&mut conn, &queue, 5).await;
    })
    .await;
    assert!(
        took < Duration::from_secs(5),
        "投入の通知で起きる: {took:?}"
    );
}

#[tokio::test]
async fn notification_waits_for_commit_and_is_dropped_on_rollback() {
    let Some(pool) = setup().await else { return };
    let waker = listening_waker(&pool).await;
    let queue = unique_queue("rollback");

    // ロールバックした投入は通知を出さない（上限まで眠る）。
    let w = waker.clone();
    let q = queue.clone();
    let waiting = tokio::spawn(async move {
        let start = Instant::now();
        w.wait_at_most(&q, LONG, Duration::from_secs(2)).await;
        start.elapsed()
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut conn = pool.acquire().await.unwrap();
    let mut tx = sqlx::Connection::begin(&mut *conn).await.unwrap();
    enqueue_on(&mut tx, &queue, 5).await;
    tx.rollback().await.unwrap();
    drop(conn);
    let slept = waiting.await.unwrap();
    assert!(
        slept >= Duration::from_millis(1900),
        "ロールバックでは起きない: {slept:?}"
    );

    // コミットまでは起きず、コミットで起きる。
    let w = waker.clone();
    let q = queue.clone();
    let waiting = tokio::spawn(async move {
        w.wait(&q, LONG).await;
        Instant::now()
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut conn = pool.acquire().await.unwrap();
    let mut tx = sqlx::Connection::begin(&mut *conn).await.unwrap();
    enqueue_on(&mut tx, &queue, 5).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!waiting.is_finished(), "コミット前に起きない");
    let committed = Instant::now();
    tx.commit().await.unwrap();
    let woke = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("コミットで起きる")
        .unwrap();
    assert!(woke.saturating_duration_since(committed) < Duration::from_secs(5));
}

#[tokio::test]
async fn sleeps_only_until_a_deferred_job_becomes_visible() {
    let Some(pool) = setup().await else { return };
    let waker = listening_waker(&pool).await;
    let queue = unique_queue("due");
    let mut conn = pool.acquire().await.unwrap();
    let id = enqueue_on(&mut conn, &queue, 5).await;
    let jobs = jobq::claim(&mut conn, &queue, Duration::from_secs(60), 1)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    // 先送り（通知は出ない）。3 秒後に見えるようになる。
    jobq::defer(&mut conn, id, Duration::from_secs(3))
        .await
        .unwrap();

    let start = Instant::now();
    waker.wait(&queue, LONG).await;
    let slept = start.elapsed();
    assert!(
        slept >= Duration::from_millis(2500) && slept < Duration::from_secs(10),
        "可視時刻まで眠り、上限まで寝過ごさない: {slept:?}"
    );
    let jobs = jobq::claim(&mut conn, &queue, Duration::from_secs(60), 1)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "起きた時点で取れる");
}

#[tokio::test]
async fn already_visible_job_is_retried_after_the_floor() {
    let Some(pool) = setup().await else { return };
    let waker = listening_waker(&pool).await;
    let queue = unique_queue("floor");
    let mut conn = pool.acquire().await.unwrap();
    // 投入済み（通知は待つ前に配られ済み）のジョブが残っていても、下限で起きて取りに行ける。
    enqueue_on(&mut conn, &queue, 5).await;
    // 投入の通知が配られ終わってから待つ（待機中に届くと通知で起きてしまい、下限を測れない）。
    tokio::time::sleep(Duration::from_millis(500)).await;
    let start = Instant::now();
    waker.wait(&queue, LONG).await;
    let slept = start.elapsed();
    assert!(
        slept >= jobq::DUE_FLOOR - Duration::from_millis(50) && slept < Duration::from_secs(5),
        "見えている行があれば下限だけ眠る: {slept:?}"
    );
}

#[tokio::test]
async fn requeue_from_dlq_wakes_the_waiting_consumer() {
    let Some(pool) = setup().await else { return };
    let waker = listening_waker(&pool).await;
    let queue = unique_queue("requeue");
    let mut conn = pool.acquire().await.unwrap();
    let id = enqueue_on(&mut conn, &queue, 1).await;
    assert!(jobq::kill(&mut conn, id, "boom").await.unwrap());
    drop(conn);
    let took = elapsed_after(&waker, &queue, || async {
        let mut conn = pool.acquire().await.unwrap();
        assert!(jobq::requeue_dead(&mut conn, id).await.unwrap());
    })
    .await;
    assert!(
        took < Duration::from_secs(5),
        "再投入の通知で起きる: {took:?}"
    );
}

#[tokio::test]
async fn polling_waker_sleeps_for_poll() {
    let waker = Waker::polling();
    assert!(!waker.is_listening());
    let start = Instant::now();
    waker.wait("any", Duration::from_millis(200)).await;
    let slept = start.elapsed();
    assert!(slept >= Duration::from_millis(190) && slept < Duration::from_secs(2));
    let start = Instant::now();
    waker
        .wait_at_most("any", Duration::from_secs(5), Duration::from_millis(100))
        .await;
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "上限が poll より優先"
    );
}
