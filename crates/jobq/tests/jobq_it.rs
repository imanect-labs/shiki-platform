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

/// `defer` は**試行回数を消費しない**（順番待ちは失敗ではない）。
///
/// これが崩れると、先行 run を待っているだけのジョブが max_attempts を食い潰して DLQ へ
/// 落ち、ユーザーが送ったメッセージが静かに消える。
#[tokio::test]
async fn defer_reschedules_without_consuming_attempts() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("defer");
    let id = enqueue(&pool, &queue, "t1", 2).await;
    let mut conn = pool.acquire().await.unwrap();

    // max_attempts=2 を超える回数だけ先送りしても DLQ へ行かない。
    for _ in 0..5 {
        let jobs = jobq::claim(&mut conn, &queue, Duration::from_secs(60), 10)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 1, "先送りしたジョブは再び claim できる");
        assert_eq!(
            jobs[0].attempts, 1,
            "attempts は claim の +1 だけで増え続けない"
        );
        jobq::defer(&mut conn, id, Duration::ZERO).await.unwrap();
    }
    let dead = jobq::dead_jobs(&mut conn, &queue, 10).await.unwrap();
    assert!(dead.is_empty(), "先送りは DLQ を作らない");

    // 先送り後も通常どおり ack できる。
    let jobs = jobq::claim(&mut conn, &queue, Duration::from_secs(60), 10)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    jobq::ack(&mut conn, id).await.unwrap();
    let jobs = jobq::claim(&mut conn, &queue, Duration::from_secs(60), 10)
        .await
        .unwrap();
    assert!(jobs.is_empty(), "ack 済みは再配信されない");
}

/// 1 件 claim → ack を繰り返し、取り出したテナント順を返す。
async fn drain_tenants(pool: &PgPool, queue: &str, n: usize) -> Vec<(String, i64)> {
    let mut conn = pool.acquire().await.unwrap();
    let mut out = Vec::new();
    for _ in 0..n {
        let jobs = jobq::claim(&mut conn, queue, Duration::from_secs(300), 1)
            .await
            .unwrap();
        let Some(job) = jobs.into_iter().next() else {
            break;
        };
        jobq::ack(&mut conn, job.id).await.unwrap();
        out.push((job.tenant_id, job.id));
    }
    out
}

/// #816: 1 テナントが先に大量投入しても、後から来たテナントは洪水が捌けるのを待たずに
/// 交互に取り出される（到着順 `ORDER BY id` だと後着は 30 件待たされる）。
#[tokio::test]
async fn claim_interleaves_tenants_instead_of_draining_the_flood_first() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("fair");
    let mut flood_ids = Vec::new();
    for _ in 0..30 {
        flood_ids.push(enqueue(&pool, &queue, "flood", 5).await);
    }
    let late_a = enqueue(&pool, &queue, "late-a", 5).await;
    let late_b = enqueue(&pool, &queue, "late-b", 5).await;

    let order = drain_tenants(&pool, &queue, 40).await;
    assert_eq!(
        order.len(),
        32,
        "全件取り出せる（取りこぼし・二重配信なし）"
    );
    let pos = |id: i64| order.iter().position(|(_, j)| *j == id).unwrap();
    assert!(
        pos(late_a) < 3 && pos(late_b) < 3,
        "後着テナントは先頭近くで取り出される: {order:?}"
    );
    // テナント内の順序（投入順）は保たれる。
    let flood_order: Vec<i64> = order
        .iter()
        .filter(|(t, _)| t == "flood")
        .map(|(_, id)| *id)
        .collect();
    assert_eq!(flood_order, flood_ids, "テナント内は投入順");
}

/// #816: 後着テナントが続けて投入しても、洪水テナントを飢えさせない（1:1 で交互）。
/// 公平はキュー単位で、別キューの投入は順序に影響しない。
#[tokio::test]
async fn claim_alternates_between_two_backlogged_tenants() {
    let Some(pool) = setup().await else { return };
    let queue = unique_queue("fair2");
    let other = unique_queue("fair2-other");
    for _ in 0..10 {
        enqueue(&pool, &queue, "a", 5).await;
    }
    // 別キューに b の大量投入があっても、このキューの b の開始位置は変わらない。
    for _ in 0..20 {
        enqueue(&pool, &other, "b", 5).await;
    }
    for _ in 0..10 {
        enqueue(&pool, &queue, "b", 5).await;
    }
    let order = drain_tenants(&pool, &queue, 20).await;
    let tenants: Vec<&str> = order.iter().map(|(t, _)| t.as_str()).collect();
    let expected: Vec<&str> = std::iter::repeat_n(["a", "b"], 10).flatten().collect();
    assert_eq!(tenants, expected, "2 テナントが 1:1 で交互に取り出される");
}

/// #816: 先頭の行が**不可視**（バックオフ中・defer 中・長時間の処理中）でも、後着テナントに
/// その古い fair_seq が振られて先行テナントの残りを追い越すことはない（仮想時刻は可視な先頭）。
/// 旧実装（キュー全体の最小 fair_seq）だと後着の 5 件が先行テナントの残り 5 件より全部先に出る。
#[tokio::test]
async fn invisible_head_does_not_let_late_tenant_jump_the_backlog() {
    let Some(pool) = setup().await else { return };
    for mode in ["fail", "defer", "in_flight"] {
        let queue = unique_queue(&format!("fair-{mode}"));
        for _ in 0..10 {
            enqueue(&pool, &queue, "a", 5).await;
        }
        let mut conn = pool.acquire().await.unwrap();
        // a の先頭（fair_seq 最小）を 1 時間不可視にする。
        let head = jobq::claim(&mut conn, &queue, Duration::from_hours(1), 1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        match mode {
            "fail" => {
                let outcome = jobq::fail(&mut conn, head.id, "transient", Duration::from_hours(1))
                    .await
                    .unwrap();
                assert!(matches!(outcome, FailOutcome::Retry { .. }));
            }
            "defer" => jobq::defer(&mut conn, head.id, Duration::from_hours(1))
                .await
                .unwrap(),
            _ => {} // claim したまま ack しない（vt 1 時間の長い処理）。
        }
        // a は先へ進む（4 件処理）。
        assert_eq!(drain_tenants(&pool, &queue, 4).await.len(), 4);
        // 後着テナント b が 5 件投入する。
        for _ in 0..5 {
            enqueue(&pool, &queue, "b", 5).await;
        }
        let order = drain_tenants(&pool, &queue, 20).await;
        let tenants: Vec<&str> = order.iter().map(|(t, _)| t.as_str()).collect();
        let expected: Vec<&str> = std::iter::repeat_n(["a", "b"], 5).flatten().collect();
        assert_eq!(
            tenants, expected,
            "{mode}: 後着 b は a の残りと交互（不可視の先頭に引きずられない）"
        );
        assert!(
            !order.iter().any(|(_, id)| *id == head.id),
            "{mode}: 不可視の行は出てこない"
        );
    }
}
