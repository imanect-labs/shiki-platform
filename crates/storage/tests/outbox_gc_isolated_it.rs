//! outbox の GC と配送記録の並行性・保持期間（data-platform §9.2・Phase 15 Task 15.6 の追補）。
//!
//! 検証:
//! - GC（本番入口 `gc_delivered_registered`）は、ロック待ちの間にコミットした新規登録も
//!   「配送を待つべきコンシューマ」に数える（登録済みコンシューマの未配送イベントを消さない）。
//! - 配送の記録（`mark_delivered` / `mark_delivered_seq`）は、並行 GC が消している最中のイベントで
//!   FK 違反を起こしてバッチ全体を巻き戻さず、残ったイベントだけを記録する（番号に穴を空けない）。
//! - 購読ストリームの台帳行が保持期間（`retain_until`）内の行は、全コンシューマが配送済みでも GC しない。
//! - RAG（`processed_at` の ack コンシューマ）が登録されていない配備では、登録済みの全台帳コンシューマへ
//!   配送済み・保持期間外の行を `processed_at` に依らず GC する（3 回目の検証 3-D-1）。未配送・保持期間内の
//!   行は残す。登録されていれば従来どおり `processed_at` を待つ。滞留の原因は先頭の行で判定する。
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
    outbox_backlog, register_ack_consumer, register_consumer, RAG_ACK_CONSUMER,
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

async fn count_left(pool: &PgPool, ids: &[i64]) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM storage_event_outbox WHERE id = ANY($1)")
        .bind(ids)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn gc_all(pool: &PgPool) -> u64 {
    let mut conn = pool.acquire().await.unwrap();
    gc_delivered_registered(&mut conn, 1_000).await.unwrap()
}

/// RAG が無効（ack コンシューマが未登録）の配備: 登録済みの全台帳コンシューマへ配送済みで保持期間を
/// 過ぎた行は、`processed_at` が立っていなくても GC される。1 つでも未配送・保持期間内の行は残る。
///
/// 以前は `processed_at IS NOT NULL` を必須にしていたため、`processed_at` を立てる RAG の relay が
/// いない配備で outbox が 1 行も消えず無制限に増えた（54 万行・974MB）。
#[tokio::test]
async fn gc_without_rag_collects_rows_delivered_to_every_consumer() {
    let Some(db) = IsolatedDb::create().await else {
        return;
    };
    let pool = db.pool.clone();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let (a, b) = ("ledger-a", "ledger-b");
    for c in [a, b] {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, c).await.unwrap();
        tx.commit().await.unwrap();
    }
    let mut delivered = Vec::new();
    for _ in 0..3 {
        delivered.push(insert_event(&pool, &tenant).await);
    }
    let partial = insert_event(&pool, &tenant).await; // b が未配送
    let retained = insert_event(&pool, &tenant).await; // 保持期間内
    let none = insert_event(&pool, &tenant).await; // 誰も配送していない
    {
        let mut tx = pool.begin().await.unwrap();
        // 購読ストリーム（保持期間を過ぎた・過ぎていない）を経由した配送も混ぜる。
        mark_delivered_seq(
            &mut tx,
            "ledger-stream:t",
            &tenant,
            &delivered[..1],
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
        mark_delivered_seq(
            &mut tx,
            "ledger-stream:t",
            &tenant,
            &[retained],
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
        let mut all_a = delivered.clone();
        all_a.extend([partial, retained]);
        mark_delivered(&mut tx, a, &all_a).await.unwrap();
        let mut all_b = delivered.clone();
        all_b.push(retained);
        mark_delivered(&mut tx, b, &all_b).await.unwrap();
        tx.commit().await.unwrap();
    }
    // 1 件目の保持期間だけを過ぎたことにする。
    sqlx::query(
        "UPDATE outbox_delivery SET retain_until = now() - interval '1 second' \
         WHERE event_id = $1 AND retain_until IS NOT NULL",
    )
    .bind(delivered[0])
    .execute(&pool)
    .await
    .unwrap();
    let processed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM storage_event_outbox WHERE processed_at IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(processed, 0, "RAG が無効なので processed_at は誰も立てない");

    assert_eq!(
        gc_all(&pool).await,
        3,
        "全コンシューマへ配送済みの 3 件を GC"
    );
    assert_eq!(count_left(&pool, &delivered).await, 0);
    assert_eq!(
        count_left(&pool, &[partial, retained, none]).await,
        3,
        "未配送・保持期間内・誰も配送していない行は残る"
    );
    let ledger_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM outbox_delivery WHERE event_id = ANY($1)")
            .bind(&delivered)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ledger_rows, 0, "配送台帳も一緒に消える");

    // 先頭の行が消せない理由は「b が未配送」（推測ではなく判定と同じ条件）。
    let mut conn = pool.acquire().await.unwrap();
    let head = outbox_backlog(&mut conn).await.unwrap().head.unwrap();
    assert_eq!(head.event_id, partial);
    assert_eq!(head.undelivered, vec![b.to_string()]);
    assert!(
        head.awaiting_ack.is_empty(),
        "RAG が無効なので ack は待たない"
    );
    assert!(head.retained_until.is_none());
    drop(conn);

    // b が追いつけば消える。保持期間内の行は期間を過ぎるまで残る。
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered(&mut tx, b, &[partial]).await.unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(gc_all(&pool).await, 1);
    assert_eq!(count_left(&pool, &[partial]).await, 0);
    let mut conn = pool.acquire().await.unwrap();
    let head = outbox_backlog(&mut conn).await.unwrap().head.unwrap();
    assert_eq!(head.event_id, retained);
    assert!(head.undelivered.is_empty());
    assert!(head.retained_until.is_some(), "保持期間内が原因");
    drop(conn);
    sqlx::query("UPDATE outbox_delivery SET retain_until = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(gc_all(&pool).await, 1);
    assert_eq!(
        count_left(&pool, &[retained, none]).await,
        1,
        "未配送の行は残る"
    );
    drop(pool);
    db.drop_db().await;
}

/// RAG が有効（ack コンシューマが登録済み）の配備: 全台帳コンシューマへ配送済みでも `processed_at` が
/// 立つまで GC しない（従来の挙動）。滞留の原因は ack 待ちとして示す。
#[tokio::test]
async fn gc_with_rag_registered_waits_for_processed_at() {
    let Some(db) = IsolatedDb::create().await else {
        return;
    };
    let pool = db.pool.clone();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let ledger = "ledger-with-rag";
    {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, ledger).await.unwrap();
        assert!(register_ack_consumer(&mut tx, RAG_ACK_CONSUMER)
            .await
            .unwrap());
        assert!(
            !register_ack_consumer(&mut tx, RAG_ACK_CONSUMER)
                .await
                .unwrap(),
            "冪等"
        );
        tx.commit().await.unwrap();
    }
    let ids = vec![
        insert_event(&pool, &tenant).await,
        insert_event(&pool, &tenant).await,
    ];
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered(&mut tx, ledger, &ids).await.unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(gc_all(&pool).await, 0, "processed_at を待つ");
    assert_eq!(count_left(&pool, &ids).await, 2);
    let mut conn = pool.acquire().await.unwrap();
    let head = outbox_backlog(&mut conn).await.unwrap().head.unwrap();
    assert_eq!(head.awaiting_ack, vec![RAG_ACK_CONSUMER.to_string()]);
    assert!(head.undelivered.is_empty(), "台帳コンシューマは配送済み");
    drop(conn);
    {
        let mut tx = pool.begin().await.unwrap();
        mark_processed(&mut tx, &ids[..1]).await.unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(gc_all(&pool).await, 1, "ack 済みの行だけ消える");
    assert_eq!(count_left(&pool, &ids[1..]).await, 1);
    drop(pool);
    db.drop_db().await;
}

async fn hinted(pool: &PgPool, hints: &storage::event::ClaimHints, consumer: &str) -> Vec<i64> {
    let mut tx = pool.begin().await.unwrap();
    let got: Vec<i64> =
        storage::event::claim_undelivered_hinted(&mut tx, hints, consumer, consumer, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
    mark_delivered(&mut tx, consumer, &got).await.unwrap();
    tx.commit().await.unwrap();
    got
}

/// アイドルのコンシューマは outbox を走査しない（3 回目の検証 3-D-1: 各コンシューマが毎秒 outbox 全体を
/// 読んでいた）。新しい書込と、後からコミットした小さい id の行は取りこぼさない。
#[tokio::test]
async fn hinted_claim_skips_scans_when_idle_and_still_sees_late_commits() {
    let Some(db) = IsolatedDb::create().await else {
        return;
    };
    let pool = db.pool.clone();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let consumer = "hinted";
    {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, consumer).await.unwrap();
        tx.commit().await.unwrap();
    }
    let hints = storage::event::ClaimHints::default();
    let e1 = insert_event(&pool, &tenant).await;
    assert_eq!(hinted(&pool, &hints, consumer).await, vec![e1]);
    // 空振りが続き、採番が今の値になった時点で実行中だった Tx（共有の PG では他の DB の Tx も含む）が
    // 終われば「全て配送済み」が確かめられ、以後は走査しない。
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(hinted(&pool, &hints, consumer).await.is_empty());
        let probe = {
            let mut conn = pool.acquire().await.unwrap();
            storage::event::claim_probe(&mut conn).await.unwrap()
        };
        assert_eq!(probe.seq, e1);
        if hints.plan(consumer, probe) == storage::event::ClaimPlan::Skip {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "アイドルの間は走査しない（確定しない）"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 小さい id を持ったまま未コミットの書込と、その後にコミットした書込。
    let mut late = pool.begin().await.unwrap();
    let e2: i64 = sqlx::query_scalar(
        "INSERT INTO storage_event_outbox (org, tenant_id, node_id, version, op, actor, payload) \
         VALUES ('acme', $1, $2, 1, 'create', 'tester', '{}'::jsonb) RETURNING id",
    )
    .bind(&tenant)
    .bind(Uuid::new_v4())
    .fetch_one(&mut *late)
    .await
    .unwrap();
    let e3 = insert_event(&pool, &tenant).await;
    assert!(e2 < e3);
    assert_eq!(
        hinted(&pool, &hints, consumer).await,
        vec![e3],
        "新しい書込は次の claim で届く"
    );
    // 未コミットの Tx が残っている間は「全て配送済み」と確定しない。
    assert!(hinted(&pool, &hints, consumer).await.is_empty());
    assert!(hinted(&pool, &hints, consumer).await.is_empty());
    late.commit().await.unwrap();
    assert_eq!(
        hinted(&pool, &hints, consumer).await,
        vec![e2],
        "後からコミットした小さい id の行も届く"
    );
    drop(pool);
    db.drop_db().await;
}
