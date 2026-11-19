//! DLQ の運用（#1335）の結合テスト（実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行する（未設定ならスキップ）。
//!
//! 検証: キュー別の件数と最古の時刻／一覧は新しい順・キューで絞れる／保持期間を過ぎた行だけ消える／
//! 管理窓口から再投入できる／監視の 1 周期で削除と集計が進む。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::time::Duration;

use jobq::{DeadLetterAdmin, DeadLetterMonitor, NewJob};
use sqlx::{postgres::PgPoolOptions, PgPool};

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

/// `queue` に投入して即 DLQ へ落とし、DLQ に落ちた時刻を `age_secs` 秒前にずらす。
async fn dead_job(pool: &PgPool, queue: &str, tenant: &str, age_secs: i64) -> i64 {
    let mut conn = pool.acquire().await.unwrap();
    let payload = serde_json::json!({"k": "v"});
    let id = jobq::enqueue_on(
        &mut conn,
        NewJob {
            queue,
            tenant_id: tenant,
            payload: &payload,
            trace_id: None,
            max_attempts: 5,
        },
    )
    .await
    .unwrap();
    assert!(jobq::kill(&mut conn, id, &format!("boom-{id}"))
        .await
        .unwrap());
    sqlx::query(
        "update job_queue_dead set died_at = now() - $2 * interval '1 second' where id = $1",
    )
    .bind(id)
    .bind(age_secs as f64)
    .execute(&mut *conn)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn stats_and_list_report_dead_jobs_without_payload() {
    let Some(pool) = setup().await else { return };
    let q1 = unique_queue("dead-a");
    let q2 = unique_queue("dead-b");
    let old = dead_job(&pool, &q1, "t1", 3600).await;
    let new = dead_job(&pool, &q1, "t2", 10).await;
    dead_job(&pool, &q2, "t1", 60).await;

    let admin = DeadLetterAdmin::new(vec![pool.clone()]);
    let stats = admin.stats().await.unwrap();
    let s1 = stats.iter().find(|s| s.queue == q1).unwrap();
    assert_eq!(s1.count, 2);
    let age = chrono::Utc::now() - s1.oldest_died_at;
    assert!(age.num_seconds() >= 3590, "最古は 1 時間前: {age:?}");
    assert_eq!(stats.iter().find(|s| s.queue == q2).unwrap().count, 1);

    let jobs = admin.list(Some(&q1), 10).await.unwrap();
    assert_eq!(
        jobs.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![new, old],
        "新しい順・キューで絞れる"
    );
    assert_eq!(jobs[0].last_error, format!("boom-{new}"));
    assert_eq!(jobs[0].tenant_id, "t2");
    assert_eq!(admin.list(Some(&q1), 1).await.unwrap().len(), 1, "limit");
    assert!(!admin.list(None, 5).await.unwrap().is_empty(), "全キュー");
}

#[tokio::test]
async fn purge_removes_only_rows_past_retention_and_requeue_revives() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("dead-purge");
    let expired = dead_job(&pool, &queue, "t1", 3 * 86_400).await;
    let kept = dead_job(&pool, &queue, "t1", 3600).await;

    // 保持 2 日: 3 日前の行だけ消える（他のテストの行も 2 日より新しいので巻き込まない）。
    let monitor = DeadLetterMonitor::new(vec![pool.clone()])
        .with_retention(Duration::from_secs(2 * 86_400))
        .with_interval(Duration::from_secs(60));
    let stats = monitor.run_once().await.unwrap();
    assert_eq!(stats.iter().find(|s| s.queue == queue).unwrap().count, 1);
    let admin = DeadLetterAdmin::new(vec![pool.clone()]);
    let left: Vec<i64> = admin
        .list(Some(&queue), 10)
        .await
        .unwrap()
        .iter()
        .map(|j| j.id)
        .collect();
    assert_eq!(left, vec![kept], "期限切れの {expired} だけ消える");

    // 再投入すると元のキューで取れる・DLQ から消える・二度目は false。
    assert!(admin.requeue(kept).await.unwrap());
    assert!(!admin.requeue(kept).await.unwrap());
    let mut conn = pool.acquire().await.unwrap();
    let jobs = jobq::claim(&mut conn, &queue, Duration::from_secs(60), 10)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].attempts, 1, "試行回数はリセット");
    jobq::ack(&mut conn, jobs[0].id).await.unwrap();
    assert!(admin.list(Some(&queue), 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn monitor_spawn_runs_in_background() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("dead-spawn");
    dead_job(&pool, &queue, "t1", 5).await;
    let handle = DeadLetterMonitor::new(vec![pool.clone()])
        .with_interval(Duration::from_millis(100))
        .spawn();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!handle.is_finished(), "常駐する");
    handle.abort();
}
