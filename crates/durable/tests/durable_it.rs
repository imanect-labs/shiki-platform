//! durable プリミティブの統合テスト（実 Postgres・env ゲート）。
//!
//! workflow が使う **tenant 複合キー**のスクラッチテーブルで claim 競合・fencing ゾンビ拒否・
//! `(キー, seq)` exactly-once を検証する（Task 10.0 受け入れ条件）。
//! chat スキーマ経由の等価性は chat 側の既存統合テスト（generation_it 等）が担保する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use durable::{EventTableSpec, Key, KeyValue, RunTableSpec};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

/// workflow ステップ相当の記述子: 複合キー・attempt は claim で増やさない（engine.md §9.5）。
const RUN_SPEC: RunTableSpec = RunTableSpec {
    table: "durable_test_run",
    status_column: "status",
    fencing_column: "fencing_token",
    lease_column: "lease_until",
    worker_column: "worker_id",
    attempt_column: None,
    updated_at_column: None,
    queued_status: "queued",
    running_status: "running",
};

const EVENT_SPEC: EventTableSpec = EventTableSpec {
    table: "durable_test_event",
    seq_column: "seq",
    kind_column: "kind",
    payload_column: "payload",
};

const KEY_COLUMNS: &[&str] = &["tenant_id", "run_id"];

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
    // 並列テストの CREATE TABLE IF NOT EXISTS は pg_type で衝突し得るため advisory lock で直列化。
    let mut tx = pool.begin().await.expect("tx begin");
    sqlx::query("SELECT pg_advisory_xact_lock(792_010_000)")
        .execute(&mut *tx)
        .await
        .expect("advisory lock");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS durable_test_run (
            tenant_id text NOT NULL,
            run_id uuid NOT NULL,
            status text NOT NULL DEFAULT 'queued',
            worker_id text,
            lease_until timestamptz,
            fencing_token bigint NOT NULL DEFAULT 0,
            attempt int NOT NULL DEFAULT 0,
            last_error text,
            cancel_requested boolean NOT NULL DEFAULT false,
            PRIMARY KEY (tenant_id, run_id)
        )",
    )
    .execute(&mut *tx)
    .await
    .expect("scratch run table");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS durable_test_event (
            tenant_id text NOT NULL,
            run_id uuid NOT NULL,
            seq bigint NOT NULL,
            kind text NOT NULL,
            payload jsonb NOT NULL,
            PRIMARY KEY (tenant_id, run_id, seq)
        )",
    )
    .execute(&mut *tx)
    .await
    .expect("scratch event table");
    tx.commit().await.expect("tx commit");
    Some(pool)
}

async fn insert_queued(pool: &PgPool, tenant: &str, run_id: Uuid) {
    sqlx::query("INSERT INTO durable_test_run (tenant_id, run_id) VALUES ($1, $2)")
        .bind(tenant)
        .bind(run_id)
        .execute(pool)
        .await
        .expect("insert queued row");
}

#[derive(Debug, sqlx::FromRow)]
struct Claimed {
    fencing_token: i64,
    attempt: i32,
}

async fn claim(pool: &PgPool, tenant: &str, run_id: Uuid, worker: &str) -> Option<Claimed> {
    let kv = [KeyValue::Text(tenant), KeyValue::Uuid(run_id)];
    durable::claim(
        pool,
        &RUN_SPEC,
        &Key::new(KEY_COLUMNS, &kv),
        worker,
        60,
        "fencing_token, attempt",
    )
    .await
    .expect("claim query")
}

#[tokio::test]
async fn claim_lease_and_takeover() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let run_id = Uuid::new_v4();
    insert_queued(&pool, &tenant, run_id).await;

    // queued → claim 成功（fencing 1・attempt_column None なので attempt は不変）。
    let c1 = claim(&pool, &tenant, run_id, "w1").await.expect("claim 1");
    assert_eq!(c1.fencing_token, 1);
    assert_eq!(
        c1.attempt, 0,
        "attempt_column=None では attempt を増やさない"
    );

    // 有効リース保持中は他ワーカーが claim できない。
    assert!(claim(&pool, &tenant, run_id, "w2").await.is_none());

    // リース失効 → takeover 成功（fencing 2）。
    sqlx::query(
        "UPDATE durable_test_run SET lease_until = now() - interval '1 second' \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&pool)
    .await
    .expect("expire lease");
    let c2 = claim(&pool, &tenant, run_id, "w2").await.expect("takeover");
    assert_eq!(c2.fencing_token, 2);
}

#[tokio::test]
async fn concurrent_claim_races_single_winner() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let run_id = Uuid::new_v4();
    insert_queued(&pool, &tenant, run_id).await;

    let (a, b) = tokio::join!(
        claim(&pool, &tenant, run_id, "wa"),
        claim(&pool, &tenant, run_id, "wb"),
    );
    assert!(
        a.is_some() ^ b.is_some(),
        "同時 claim はちょうど 1 つだけ成功する（a={a:?}, b={b:?}）"
    );
}

#[tokio::test]
async fn heartbeat_extends_only_for_current_fencing() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let run_id = Uuid::new_v4();
    insert_queued(&pool, &tenant, run_id).await;
    let c = claim(&pool, &tenant, run_id, "w1").await.expect("claim");

    let kv = [KeyValue::Text(tenant.as_str()), KeyValue::Uuid(run_id)];
    let key = Key::new(KEY_COLUMNS, &kv);
    // 現 fencing → 延長成功・cancel_requested を返す。
    let cancel: Option<bool> = durable::heartbeat(
        &pool,
        &RUN_SPEC,
        &key,
        c.fencing_token,
        60,
        "cancel_requested",
    )
    .await
    .expect("heartbeat query");
    assert_eq!(cancel, Some(false));

    // 延長そのものを検証: heartbeat 後の lease_until が未来にある（リースが実際に延びた）。
    let lease_after: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "SELECT lease_until FROM durable_test_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("lease_until row");
    assert!(
        lease_after > chrono::Utc::now(),
        "heartbeat 後もリースが有効（延長された）であること"
    );

    // 古い fencing（ゾンビ）→ None。
    let stale: Option<bool> = durable::heartbeat(
        &pool,
        &RUN_SPEC,
        &key,
        c.fencing_token - 1,
        60,
        "cancel_requested",
    )
    .await
    .expect("heartbeat query");
    assert!(stale.is_none(), "fencing 不一致の heartbeat は失敗する");
}

#[tokio::test]
async fn append_is_exactly_once_and_rejects_zombie() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let run_id = Uuid::new_v4();
    insert_queued(&pool, &tenant, run_id).await;
    let c = claim(&pool, &tenant, run_id, "w1").await.expect("claim");

    let kv = [KeyValue::Text(tenant.as_str()), KeyValue::Uuid(run_id)];
    let key = Key::new(KEY_COLUMNS, &kv);
    let payload = serde_json::json!({ "n": 1 });

    let s1 = durable::append_event(
        &pool,
        &RUN_SPEC,
        &EVENT_SPEC,
        &key,
        "tick",
        &payload,
        c.fencing_token,
    )
    .await
    .expect("append 1");
    let s2 = durable::append_event(
        &pool,
        &RUN_SPEC,
        &EVENT_SPEC,
        &key,
        "tick",
        &payload,
        c.fencing_token,
    )
    .await
    .expect("append 2");
    assert_eq!((s1, s2), (Some(1), Some(2)), "seq は単調増加");

    // ゾンビ（古い fencing）の追記は seq を返さず、行も増えない。
    let zombie = durable::append_event(
        &pool,
        &RUN_SPEC,
        &EVENT_SPEC,
        &key,
        "tick",
        &payload,
        c.fencing_token - 1,
    )
    .await
    .expect("zombie append");
    assert!(zombie.is_none());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM durable_test_event WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(count, 2, "ゾンビ追記で行が増えない（exactly-once）");

    // replay: from_seq より後だけを seq 順に返す。
    let replayed: Vec<(i64, serde_json::Value)> =
        durable::replay_events(&pool, &EVENT_SPEC, &key, 1)
            .await
            .expect("replay");
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].0, 2);
}

#[tokio::test]
async fn fenced_finalize_clears_lease_and_rejects_zombie() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let run_id = Uuid::new_v4();
    insert_queued(&pool, &tenant, run_id).await;
    let c = claim(&pool, &tenant, run_id, "w1").await.expect("claim");

    let kv = [KeyValue::Text(tenant.as_str()), KeyValue::Uuid(run_id)];
    let key = Key::new(KEY_COLUMNS, &kv);

    // ゾンビの確定は no-op。
    let zombie: Option<Uuid> = durable::fenced_finalize(
        &pool,
        &RUN_SPEC,
        &key,
        c.fencing_token - 1,
        "failed",
        &[("last_error", KeyValue::OptText(Some("boom")))],
        "run_id",
    )
    .await
    .expect("zombie finalize");
    assert!(zombie.is_none());

    // 現 fencing の確定は成功し、リース解放＋追加 SET が反映される。
    let done: Option<Uuid> = durable::fenced_finalize(
        &pool,
        &RUN_SPEC,
        &key,
        c.fencing_token,
        "done",
        &[("last_error", KeyValue::OptText(None))],
        "run_id",
    )
    .await
    .expect("finalize");
    assert_eq!(done, Some(run_id));
    let (status, lease, last_error): (
        String,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status, lease_until, last_error FROM durable_test_run \
             WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("row");
    assert_eq!(status, "done");
    assert!(lease.is_none(), "確定でリースが解放される");
    assert!(last_error.is_none());

    // 端末状態では unfenced 追記も no-op（allowed_statuses ゲート）。
    let after = durable::append_event_unfenced(
        &pool,
        &RUN_SPEC,
        &EVENT_SPEC,
        &key,
        "error",
        &serde_json::json!({}),
        &["queued", "running"],
    )
    .await
    .expect("unfenced append");
    assert!(after.is_none(), "端末状態への強制追記は no-op");
}

#[tokio::test]
async fn pubsub_publish_and_subscribe_roundtrip() {
    let Ok(url) = std::env::var("REDIS_TEST_URL") else {
        eprintln!("REDIS_TEST_URL 未設定のためスキップ");
        return;
    };
    use futures::StreamExt;
    let ps = durable::RedisPubSub::connect(&url)
        .await
        .expect("redis connect");
    let channel = format!("durable:test:{}", Uuid::new_v4());
    let mut stream = ps.subscribe(&channel).await.expect("subscribe");
    ps.publish_best_effort(&channel, "hello").await;
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("timeout")
        .expect("message");
    let payload: String = msg.get_payload().expect("payload");
    assert_eq!(payload, "hello");
}
