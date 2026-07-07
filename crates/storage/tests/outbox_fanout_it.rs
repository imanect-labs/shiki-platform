//! P10-A0: outbox の per-consumer fan-out（配送台帳）結合テスト。
//!
//! 検証（roadmap/phase-10.md P10-A0 の受け入れ条件）:
//! - 同一イベントが複数コンシューマ（追加台帳コンシューマ／RAG の processed_at 経路）に**独立に**届く。
//! - **並行書込で遅れてコミットした小さい id のイベントも取りこぼさない**（未コミット飛び越し回避）。
//! - GC は全台帳コンシューマ配送済み（＋processed_at ack）後に削除する。
//!
//! `STORAGE_TEST_DATABASE_URL` 未設定ならスキップ（他の結合テストと同じ env ゲート）。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use storage::event::{
    claim_undelivered, gc_delivered, mark_delivered, mark_processed, register_consumer,
};
use uuid::Uuid;

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

/// テスト用 outbox 行を 1 件 INSERT して id を返す（`emit_on` と同じ列・任意の executor 上で）。
async fn insert_event<'e, E>(exec: E, tenant: &str, node_id: Uuid) -> i64
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query(
        "INSERT INTO storage_event_outbox (org, tenant_id, node_id, version, op, actor, payload) \
         VALUES ('acme', $1, $2, 1, 'create', 'tester', '{}'::jsonb) RETURNING id",
    )
    .bind(tenant)
    .bind(node_id)
    .fetch_one(exec)
    .await
    .expect("insert event")
    .get::<i64, _>("id")
}

/// 大きな limit で claim し、自テナントの id のみに絞る（共有テーブルの他テスト行を排除）。
async fn claim_mine(pool: &PgPool, consumer: &str, tenant: &str) -> Vec<i64> {
    let mut tx = pool.begin().await.expect("tx");
    let events = claim_undelivered(&mut tx, consumer, 1_000_000)
        .await
        .expect("claim");
    let ids: Vec<i64> = events
        .into_iter()
        .filter(|e| e.tenant_id == tenant)
        .map(|e| e.id)
        .collect();
    mark_delivered(&mut tx, consumer, &ids)
        .await
        .expect("mark_delivered");
    tx.commit().await.expect("commit");
    ids
}

#[tokio::test]
async fn fanout_delivers_same_event_to_independent_consumers() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let id = insert_event(&pool, &tenant, node).await;

    // コンシューマ A（追加台帳）が配送を記録しても…
    let a = format!("wf-a-{}", Uuid::new_v4().simple());
    assert_eq!(claim_mine(&pool, &a, &tenant).await, vec![id]);
    // …同一イベントは RAG（processed_at 経路）から見て未処理のまま（片方の消費が他方を消さない）。
    let unprocessed: bool =
        sqlx::query_scalar("SELECT processed_at IS NULL FROM storage_event_outbox WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        unprocessed,
        "台帳コンシューマの配送は processed_at を消費しない"
    );
    // …別の台帳コンシューマ B からも独立に届く。
    let b = format!("wf-b-{}", Uuid::new_v4().simple());
    assert_eq!(claim_mine(&pool, &b, &tenant).await, vec![id]);

    // A が再スキャンしても二度は届かない（配送台帳で冪等）。
    assert!(claim_mine(&pool, &a, &tenant).await.is_empty());

    // 逆向き: RAG が processed_at を立てても、台帳コンシューマ C には引き続き届く。
    {
        let mut tx = pool.begin().await.unwrap();
        mark_processed(&mut tx, &[id]).await.unwrap();
        tx.commit().await.unwrap();
    }
    let c = format!("wf-c-{}", Uuid::new_v4().simple());
    assert_eq!(
        claim_mine(&pool, &c, &tenant).await,
        vec![id],
        "processed_at は台帳コンシューマの claim に影響しない"
    );
}

#[tokio::test]
async fn late_committed_lower_id_is_not_skipped() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let consumer = format!("wf-adv-{}", Uuid::new_v4().simple());

    // txn A: 小さい id のイベントを INSERT するが **コミットしない**（未コミットで保持）。
    let mut tx_a = pool.begin().await.expect("tx_a");
    let id_a = insert_event(&mut *tx_a, &tenant, node).await;

    // txn B: 後続の（大きい id の）イベントを INSERT して **先にコミット**。
    let mut tx_b = pool.begin().await.expect("tx_b");
    let id_b = insert_event(&mut *tx_b, &tenant, node).await;
    tx_b.commit().await.expect("commit b");
    assert!(id_b > id_a, "B の id が A より大きい前提");

    // 1 回目の claim: A は未コミットで不可視。B のみ見えるはず。
    let first = claim_mine(&pool, &consumer, &tenant).await;
    assert_eq!(first, vec![id_b], "コミット済みの B のみ配送される");

    // ここで A を遅れてコミットする（B より小さい id が後からコミット確定）。
    tx_a.commit().await.expect("commit a");

    // 2 回目の claim: 単純 last_seq カーソルなら A（< 既配送 B）を飛ばすが、
    // NOT EXISTS(delivery) 方式なので **A を取りこぼさない**。
    let second = claim_mine(&pool, &consumer, &tenant).await;
    assert_eq!(second, vec![id_a], "遅れてコミットした小さい id も拾う");
}

#[tokio::test]
async fn register_consumer_skips_backlog_but_delivers_new_events() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let consumer = format!("wf-reg-{}", Uuid::new_v4().simple());

    // 有効化前のバックログ。
    let backlog = insert_event(&pool, &tenant, node).await;

    // コンシューマ登録: 現バックログを配送済みに刻む（初回一斉発火を防ぐ）。
    {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, &consumer).await.unwrap();
        tx.commit().await.unwrap();
    }

    // 有効化後の新規イベント。
    let fresh = insert_event(&pool, &tenant, node).await;

    let claimed = claim_mine(&pool, &consumer, &tenant).await;
    assert!(!claimed.contains(&backlog), "バックログは再配送しない");
    assert!(claimed.contains(&fresh), "有効化以降のイベントは配送する");
}

#[tokio::test]
async fn gc_never_deletes_unacked_events() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let consumer = format!("wf-noack-{}", Uuid::new_v4().simple());

    // 古い created_at だが未 ack（processed_at NULL・台帳未配送）→ GC は消してはいけない。
    let unacked = insert_event(&pool, &tenant, node).await;
    sqlx::query(
        "UPDATE storage_event_outbox SET created_at = now() - interval '400 days' WHERE id = $1",
    )
    .bind(unacked)
    .execute(&pool)
    .await
    .unwrap();

    {
        let mut tx = pool.begin().await.unwrap();
        gc_delivered(&mut tx, &[consumer.as_str()]).await.unwrap();
        tx.commit().await.unwrap();
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM storage_event_outbox WHERE id = $1)")
            .bind(unacked)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(exists, "未 ack の古いイベントを retention で消さない");
}

#[tokio::test]
async fn gc_removes_only_fully_delivered_events() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let consumer = format!("wf-gc-{}", Uuid::new_v4().simple());

    // done: 台帳コンシューマ配送 ＋ RAG processed_at 済み → GC 対象。
    let done = insert_event(&pool, &tenant, node).await;
    // pending_ledger: RAG は済みだが台帳コンシューマ未配送 → 残す。
    let pending_ledger = insert_event(&pool, &tenant, node).await;
    // pending_rag: 台帳配送済みだが RAG 未 ack → 残す。
    let pending_rag = insert_event(&pool, &tenant, node).await;

    // 台帳配送: done と pending_rag。
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered(&mut tx, &consumer, &[done, pending_rag])
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    // RAG ack: done と pending_ledger。
    {
        let mut tx = pool.begin().await.unwrap();
        mark_processed(&mut tx, &[done, pending_ledger])
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // GC は配送済み＋processed_at ack のみ削除（未 ack は決して消さない）。
    let deleted = {
        let mut tx = pool.begin().await.unwrap();
        let n = gc_delivered(&mut tx, &[consumer.as_str()]).await.unwrap();
        tx.commit().await.unwrap();
        n
    };
    assert!(deleted >= 1, "全配送済みの done は GC される");

    let exists = |id: i64| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM storage_event_outbox WHERE id = $1)",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert!(!exists(done).await, "全配送済みは削除");
    assert!(exists(pending_ledger).await, "台帳未配送は残る");
    assert!(exists(pending_rag).await, "RAG 未 ack は残る");

    // done の配送台帳行も CASCADE で消えていること。
    let ledger_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM outbox_delivery WHERE event_id = $1")
            .bind(done)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ledger_rows, 0, "outbox 削除で配送台帳も CASCADE 削除");
}

#[tokio::test]
async fn register_consumer_is_one_time_only() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let consumer = format!("wf-once-{}", Uuid::new_v4().simple());

    // 初回登録（バックログ無し）。
    {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, &consumer).await.unwrap();
        tx.commit().await.unwrap();
    }
    // 登録後に到着した未配送イベント（サーバ停止中の到着を模す）。
    let pending = insert_event(&pool, &tenant, node).await;

    // 再起動を模した 2 回目の登録は **no-op**（未配送を配送済みにしない）。
    {
        let mut tx = pool.begin().await.unwrap();
        let n = register_consumer(&mut tx, &consumer).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(n, 0, "2 回目の登録は fast-forward しない");
    }
    // pending は依然として配送される（取りこぼさない）。
    assert!(
        claim_mine(&pool, &consumer, &tenant)
            .await
            .contains(&pending),
        "再起動後も未配送イベントを取りこぼさない"
    );
}
