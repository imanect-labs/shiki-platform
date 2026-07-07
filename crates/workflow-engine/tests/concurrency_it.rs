//! 並行制御・レート制限の結合テスト（Task 10.5 受け入れ条件）。
//!
//! - concurrency カウンタは上限まで予約でき、超過は拒否でなく「取れない（順番待ち）」
//! - 全階層 all-or-nothing（1 階層でも超過なら部分予約を残さない）
//! - Redis トークンバケットはバースト上限で頭打ち・補充で回復

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::concurrency::{ConcurrencyStore, Slot};
use workflow_engine::ratelimit::{BucketConfig, TokenBucket};

async fn pg() -> Option<PgPool> {
    let url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

#[tokio::test]
async fn concurrency_limit_queues_beyond_capacity() {
    let Some(pool) = pg().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = ConcurrencyStore::new(pool.clone());

    // workflow 上限 2。
    let slots = vec![Slot::workflow(wf, 2)];
    assert!(
        store.try_acquire(&tenant, &slots).await.unwrap(),
        "1 本目は取れる"
    );
    assert!(
        store.try_acquire(&tenant, &slots).await.unwrap(),
        "2 本目も取れる"
    );
    assert!(
        !store.try_acquire(&tenant, &slots).await.unwrap(),
        "3 本目は上限で取れない（順番待ち）"
    );

    // 1 本解放すると再び取れる。
    store.release(&tenant, &slots).await.unwrap();
    assert!(
        store.try_acquire(&tenant, &slots).await.unwrap(),
        "解放後は取れる"
    );
}

#[tokio::test]
async fn multi_tier_is_all_or_nothing() {
    let Some(pool) = pg().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let store = ConcurrencyStore::new(pool.clone());

    // global 上限 5・node 上限 1。node を先に埋める。
    let node_slot = Slot::node_kind(wf, "storage.write", 1);
    let combo = vec![Slot::global(5), node_slot.clone()];

    assert!(
        store.try_acquire(&tenant, &combo).await.unwrap(),
        "初回は全階層取れる"
    );
    // node が上限 → combo は取れない。かつ global に部分予約が残らないこと。
    assert!(
        !store.try_acquire(&tenant, &combo).await.unwrap(),
        "node 上限で全体失敗"
    );

    // global 単独ならまだ 4 枠あるので取れる（部分予約が残っていない証拠）。
    let g: i32 = sqlx::query_scalar(
        "SELECT current_n FROM concurrency_counter \
         WHERE tenant_id = $1 AND scope_kind = 'global' AND scope_key = ''",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        g, 1,
        "global は 1（失敗した予約は巻き戻り 2 になっていない）"
    );
}

#[tokio::test]
async fn token_bucket_bursts_then_throttles() {
    let Ok(redis_url) = std::env::var("REDIS_TEST_URL") else {
        eprintln!("REDIS_TEST_URL 未設定のためスキップ");
        return;
    };
    let client = redis::Client::open(redis_url).expect("redis");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("conn");
    let bucket = TokenBucket::new(conn);
    let key = format!("test-{}", Uuid::new_v4());
    // 容量 3・補充ほぼ 0（バーストのみ検証）。
    let cfg = BucketConfig {
        capacity: 3,
        refill_per_sec: 0.001,
    };

    assert!(bucket.try_acquire(&key, cfg, 1).await.unwrap(), "1");
    assert!(bucket.try_acquire(&key, cfg, 1).await.unwrap(), "2");
    assert!(bucket.try_acquire(&key, cfg, 1).await.unwrap(), "3");
    assert!(
        !bucket.try_acquire(&key, cfg, 1).await.unwrap(),
        "4 本目はバースト超過で拒否"
    );

    // 補充を効かせて回復（容量 10・毎秒 1000 補充なら即回復）。
    let fast = BucketConfig {
        capacity: 10,
        refill_per_sec: 1000.0,
    };
    let key2 = format!("test-{}", Uuid::new_v4());
    for _ in 0..10 {
        assert!(bucket.try_acquire(&key2, fast, 1).await.unwrap());
    }
}
