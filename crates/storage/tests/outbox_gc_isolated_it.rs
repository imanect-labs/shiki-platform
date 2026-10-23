//! outbox の GC と配送記録の並行性・保持期間（data-platform §9.2・Phase 15 Task 15.6 の追補）。
//!
//! 検証:
//! - GC（本番入口 `gc_delivered_registered`）は、ロック待ちの間にコミットした新規登録も
//!   「配送を待つべきコンシューマ」に数える（登録済みコンシューマの未配送イベントを消さない）。
//! - 配送の記録（`mark_delivered` / `mark_delivered_seq`）は、並行 GC が消している最中のイベントで
//!   FK 違反を起こしてバッチ全体を巻き戻さず、残ったイベントだけを記録する（番号に穴を空けない）。
//! - 購読ストリームの台帳行が保持期間（`retain_until`）内の行は、全コンシューマが配送済みでも GC しない。
//!
//! 並行性はロック待ちの順序を作って決定的に再現する。どれも表全体を対象に GC を回すので、他テストの
//! 行や登録（明示集合での GC を含む）と干渉しないよう、**テストごとに専用のデータベース**を作って使う。
//! `STORAGE_TEST_DATABASE_URL` 未設定ならスキップ（他の結合テストと同じ env ゲート）。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::time::{Duration, Instant};

use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgPool};
use storage::event::{
    claim_undelivered, gc_delivered_registered, mark_delivered, mark_delivered_seq, mark_processed,
    register_consumer,
};
use uuid::Uuid;

/// テスト専用のデータベース（マイグレーション済み）。最後に [`IsolatedDb::drop_db`] で消す。
struct IsolatedDb {
    admin: PgPool,
    name: String,
    pool: PgPool,
}

impl IsolatedDb {
    async fn create() -> Option<Self> {
        let Ok(base) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            return None;
        };
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&base)
            .await
            .expect("connect");
        let name = format!("shiki_outbox_gc_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .expect("create database");
        let url = format!("{}/{name}", base.rsplit_once('/').expect("db url").0);
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect(&url)
            .await
            .expect("connect isolated");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("migrate");
        Some(Self { admin, name, pool })
    }

    async fn drop_db(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.name))
            .execute(&self.admin)
            .await
            .expect("drop database");
    }
}

async fn insert_event(pool: &PgPool, tenant: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO storage_event_outbox (org, tenant_id, node_id, version, op, actor, payload) \
         VALUES ('acme', $1, $2, 1, 'create', 'tester', '{}'::jsonb) RETURNING id",
    )
    .bind(tenant)
    .bind(Uuid::new_v4())
    .fetch_one(pool)
    .await
    .expect("insert event")
}

async fn event_exists(pool: &PgPool, id: i64) -> bool {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM storage_event_outbox WHERE id = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("exists")
}

/// バックエンド `pid` がロック待ちに入るまで待つ（並行の順序を決定的にするため）。
async fn wait_until_blocked(pool: &PgPool, pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let waiting: Option<String> =
            sqlx::query_scalar("SELECT wait_event_type FROM pg_stat_activity WHERE pid = $1")
                .bind(pid)
                .fetch_optional(pool)
                .await
                .expect("pg_stat_activity")
                .flatten();
        if waiting.as_deref() == Some("Lock") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "pid {pid} がロック待ちに入らない"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn backend_pid(conn: &mut sqlx::PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(conn)
        .await
        .expect("pid")
}

/// GC がロックを待つ間にコミットした新規登録は、GC の判定集合に入る。
///
/// 集合をロックの前に読むと、新規登録を落として「登録より後に到着し、既存コンシューマだけが
/// 配送済みのイベント」を消してしまう（新規コンシューマはそのイベントを永久に受け取れない）。
#[tokio::test]
async fn registered_gc_counts_a_registration_it_waited_for() {
    let Some(db) = IsolatedDb::create().await else {
        return;
    };
    let pool = db.pool.clone();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let existing = "race-existing";
    let newcomer = "race-newcomer";
    {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, existing).await.unwrap();
        tx.commit().await.unwrap();
    }
    // 新規登録が fast-forward を終え、コミット前（GC との相互排除のロックを保持中）。
    let mut registering = pool.begin().await.unwrap();
    register_consumer(&mut registering, newcomer).await.unwrap();
    // GC はロック待ちに入る。
    let mut gc_conn = pool.acquire().await.unwrap();
    let gc_pid = backend_pid(&mut gc_conn).await;
    let gc = tokio::spawn(async move {
        let mut tx = gc_conn.begin().await.unwrap();
        let n = gc_delivered_registered(&mut tx, 1_000).await.unwrap();
        tx.commit().await.unwrap();
        n
    });
    wait_until_blocked(&pool, gc_pid).await;
    // 登録の fast-forward より後に到着したイベント: 既存コンシューマは配送済み・RAG も ack 済み。
    // 新規コンシューマはまだ配送していない。
    let event = insert_event(&pool, &tenant).await;
    {
        let mut tx = pool.begin().await.unwrap();
        mark_processed(&mut tx, &[event]).await.unwrap();
        mark_delivered(&mut tx, existing, &[event]).await.unwrap();
        tx.commit().await.unwrap();
    }
    registering.commit().await.unwrap();
    gc.await.unwrap();
    assert!(
        event_exists(&pool, event).await,
        "登録済みコンシューマ（ロック待ちの間に登録）が未配送のイベントは GC しない"
    );
    // 新規コンシューマはそのイベントを受け取れる。
    let mut tx = pool.begin().await.unwrap();
    let got: Vec<i64> = claim_undelivered(&mut tx, newcomer, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    tx.rollback().await.unwrap();
    assert_eq!(got, vec![event]);
    drop(pool);
    db.drop_db().await;
}

/// 配送の記録は、並行 GC が消している最中のイベントで FK 違反にならず、残りを記録する。
///
/// READ COMMITTED の `INSERT` は文のスナップショットで見えた行を、FK 検査の時点で消されていると
/// FK 違反で落ちる（バッチ全体が巻き戻り、同じバッチの他イベントの採番も失われる）。
#[tokio::test]
async fn delivery_marks_skip_an_event_deleted_concurrently() {
    let Some(db) = IsolatedDb::create().await else {
        return;
    };
    let pool = db.pool.clone();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let stream = "race-stream:table";
    let ledger = "race-ledger";
    for seq_path in [true, false] {
        let gone = insert_event(&pool, &tenant).await;
        let live = insert_event(&pool, &tenant).await;
        // GC 役: 行を消してコミット前（行ロックを保持中）。
        let mut deleting = pool.begin().await.unwrap();
        sqlx::query("DELETE FROM storage_event_outbox WHERE id = $1")
            .bind(gone)
            .execute(&mut *deleting)
            .await
            .unwrap();
        let mut relay_conn = pool.acquire().await.unwrap();
        let relay_pid = backend_pid(&mut relay_conn).await;
        let tenant2 = tenant.clone();
        let relay = tokio::spawn(async move {
            let mut tx = relay_conn.begin().await.unwrap();
            let got = if seq_path {
                mark_delivered_seq(
                    &mut tx,
                    stream,
                    &tenant2,
                    &[gone, live],
                    Duration::from_secs(3600),
                )
                .await
                .map(|v| v.into_iter().map(|(id, _)| id).collect::<Vec<_>>())
            } else {
                mark_delivered(&mut tx, ledger, &[gone, live])
                    .await
                    .map(|()| Vec::new())
            };
            tx.commit().await.unwrap();
            got
        });
        wait_until_blocked(&pool, relay_pid).await;
        deleting.commit().await.unwrap();
        let got = relay
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("消えたイベントで落ちない（seq_path={seq_path}）: {e}"));
        let consumer = if seq_path { stream } else { ledger };
        let rows: Vec<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT event_id, delivery_seq FROM outbox_delivery WHERE consumer = $1 \
             AND event_id = ANY($2) ORDER BY event_id",
        )
        .bind(consumer)
        .bind(vec![gone, live])
        .fetch_all(&pool)
        .await
        .unwrap();
        if seq_path {
            assert_eq!(got, vec![live], "残ったイベントだけに番号を払う");
            // 番号は残った分だけ（穴を空けない）: このストリームの最初の 1 件。
            assert_eq!(rows, vec![(live, Some(1))]);
            let next: i64 =
                sqlx::query_scalar("SELECT next_seq FROM outbox_delivery_seq WHERE consumer = $1")
                    .bind(stream)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(next, 2, "消えたイベントに番号を払っていない");
        } else {
            assert_eq!(
                rows,
                vec![(live, None)],
                "残ったイベントだけを配送済みにする"
            );
        }
    }
    drop(pool);
    db.drop_db().await;
}

/// 購読ストリームへ採番した台帳行が保持期間内の行は、全コンシューマが配送済み・RAG ack 済みでも
/// GC されない。保持期間を過ぎれば GC される。
#[tokio::test]
async fn stream_rows_within_retention_survive_gc() {
    let Some(db) = IsolatedDb::create().await else {
        return;
    };
    let pool = db.pool.clone();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let ledger = "retention-ledger";
    {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, ledger).await.unwrap();
        tx.commit().await.unwrap();
    }
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(insert_event(&pool, &tenant).await);
    }
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered_seq(
            &mut tx,
            "retention-stream:table",
            &tenant,
            &ids,
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
        mark_delivered(&mut tx, ledger, &ids).await.unwrap();
        mark_processed(&mut tx, &ids).await.unwrap();
        tx.commit().await.unwrap();
    }
    let left = |pool: PgPool, ids: Vec<i64>| async move {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM storage_event_outbox WHERE id = ANY($1)")
            .bind(ids)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let mut conn = pool.acquire().await.unwrap();
    gc_delivered_registered(&mut conn, 1_000).await.unwrap();
    assert_eq!(
        left(pool.clone(), ids.clone()).await,
        5,
        "保持期間内の行は残る"
    );
    sqlx::query(
        "UPDATE outbox_delivery SET retain_until = now() - interval '1 second' \
                 WHERE retain_until IS NOT NULL",
    )
    .execute(&pool)
    .await
    .unwrap();
    gc_delivered_registered(&mut conn, 1_000).await.unwrap();
    assert_eq!(
        left(pool.clone(), ids.clone()).await,
        0,
        "保持期間を過ぎた行は GC される"
    );
    drop(conn);
    drop(pool);
    db.drop_db().await;
}
