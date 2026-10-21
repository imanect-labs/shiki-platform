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

/// 配送を**期待する** claim の短時間リトライ版。
///
/// `claim_undelivered` は `FOR UPDATE SKIP LOCKED` で行をロックし、コンシューマ横断で
/// 他テナントの行も掴む。並行実行中の他テストの claim txn に行を掴まれた瞬間は空振りする
/// ため（CI flake の実績あり）、ロック解放を待って再試行する（at-least-once の意味論）。
/// 空を期待する検証にはリトライしない [`claim_mine`] を使うこと。
async fn claim_mine_eventually(pool: &PgPool, consumer: &str, tenant: &str) -> Vec<i64> {
    for _ in 0..40 {
        let ids = claim_mine(pool, consumer, tenant).await;
        if !ids.is_empty() {
            return ids;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    Vec::new()
}

#[tokio::test]
async fn fanout_delivers_same_event_to_independent_consumers() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let id = insert_event(&pool, &tenant, node).await;

    // コンシューマ A（追加台帳）が配送を記録しても…
    let a = format!("wf-a-{}", Uuid::new_v4().simple());
    assert_eq!(claim_mine_eventually(&pool, &a, &tenant).await, vec![id]);
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
    assert_eq!(claim_mine_eventually(&pool, &b, &tenant).await, vec![id]);

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
        claim_mine_eventually(&pool, &c, &tenant).await,
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
    let first = claim_mine_eventually(&pool, &consumer, &tenant).await;
    assert_eq!(first, vec![id_b], "コミット済みの B のみ配送される");

    // ここで A を遅れてコミットする（B より小さい id が後からコミット確定）。
    tx_a.commit().await.expect("commit a");

    // 2 回目の claim: 単純 last_seq カーソルなら A（< 既配送 B）を飛ばすが、
    // NOT EXISTS(delivery) 方式なので **A を取りこぼさない**。
    let second = claim_mine_eventually(&pool, &consumer, &tenant).await;
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

    let claimed = claim_mine_eventually(&pool, &consumer, &tenant).await;
    assert!(!claimed.contains(&backlog), "バックログは再配送しない");
    assert!(claimed.contains(&fresh), "有効化以降のイベントは配送する");

    // `outbox_consumer` はテスト間共有なので片付ける（残すと GC 検証が他テストに引きずられる）。
    unregister(&pool, &consumer).await;
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
        gc_delivered(&mut tx, &[consumer.as_str()], 1_000)
            .await
            .unwrap();
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
    // GC は `FOR UPDATE SKIP LOCKED` なので、他テストが同じ行をロック中だとその回は
    // スキップされる（本番では次周期で回収される正しい挙動）。消えるまで回して判定する。
    assert!(
        gc_until_gone(&pool, &[consumer.as_str()], done).await,
        "全配送済みの done は GC される"
    );

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
        claim_mine_eventually(&pool, &consumer, &tenant)
            .await
            .contains(&pending),
        "再起動後も未配送イベントを取りこぼさない"
    );

    unregister(&pool, &consumer).await;
}

/// `OutboxRow` を共有する **3 つのクエリすべて**が同じ列集合で読めることを固定する（#392）。
///
/// `claim` / `claim_undelivered` / `peek_app_events_after` は 1 つの行構造体を共有しており、
/// フィールドを足したのに列を足し忘れたクエリがあると `query_as` が**実行時に**失敗して
/// その経路が丸ごと無音になる（app-gateway の SSE ライブテールで実際に踏んだ）。
/// ここで 3 経路すべてを 1 度は通しておく。
#[tokio::test]
async fn all_outbox_read_paths_map_the_same_row_shape() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let node = Uuid::new_v4();

    // app 向けドメインイベント（`payload.event_type` 付き）を 1 件。
    let id: i64 = sqlx::query(
        "INSERT INTO storage_event_outbox (org, tenant_id, node_id, version, op, actor, payload) \
         VALUES ('acme', $1, $2, 1, 'create', 'tester', \
                 '{\"event_type\": \"data.record.created\"}'::jsonb) RETURNING id",
    )
    .bind(&tenant)
    .bind(node)
    .fetch_one(&pool)
    .await
    .expect("insert app event")
    .get::<i64, _>("id");

    // ① ライブテール（app-gateway の SSE）。
    let peeked = storage::event::peek_app_events_after(&pool, &tenant, 0, 100)
        .await
        .expect("peek は列不足で落ちない");
    let hit = peeked
        .iter()
        .find(|e| e.id == id)
        .expect("投入したイベントが読める");
    assert!(!hit.system, "node 行が無いイベントは system=false 扱い");

    // ② per-consumer 配送台帳／③ 破壊的消費（rag relay）。
    //
    // ここで見るのは**列集合が合っていること**だけなので、自分の行が返ることは要求しない
    // （同一バイナリの他テストと outbox を共有しており、`SKIP LOCKED` で取り合いになる）。
    // 取り合いを最小にするため limit も小さくし、ロックは rollback で即返す。
    let mut tx = pool.begin().await.expect("tx");
    claim_undelivered(&mut tx, "shape-test", 5)
        .await
        .expect("claim_undelivered は列不足で落ちない");
    tx.rollback().await.expect("rollback");

    let mut tx = pool.begin().await.expect("tx");
    storage::event::claim(&mut tx, 5)
        .await
        .expect("claim は列不足で落ちない（system フラグはここで使われる）");
    tx.rollback().await.expect("rollback");
}

// ---------------------------------------------------------------------------
// #413: GC の本番配線に伴う追加検証。
//
// GC 判定のコンシューマ集合は `outbox_consumer`（登録台帳）が正本である。台帳コンシューマは
// それぞれ独立したフィーチャフラグの背後で起動するため、「このプロセスが spawn した集合」を
// 渡すと片方のフラグが off のレプリカが他方宛の未配送イベントを消してしまう（イベント喪失）。
//
// ⚠️ テスト設計上の注意: `outbox_consumer` は**全テストで共有される**グローバル状態なので、
// `gc_delivered_registered` の「消える」方向をアサートすると他テストの登録に左右されて不安定に
// なる（余計な登録が 1 つあれば消えない）。したがって:
//   * 「消える／消えない」の判定ロジックは明示集合の `gc_delivered` で決定的に検証する。
//   * `gc_delivered_registered` は**登録集合を読んでいること**を、安全側（＝未配送があれば
//     消さない）の方向だけで検証する（他テストの登録が増えても結論が反転しない）。
// ---------------------------------------------------------------------------

/// GC は渡されたコンシューマ**全員**の配送が揃うまで削除しない（片方停止中は消さない）。
#[tokio::test]
async fn gc_waits_for_every_ledger_consumer() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    // 別フラグ背後の 2 コンシューマ（workflow / miniapp-functions 相当）。
    let live = format!("wf-live-{}", Uuid::new_v4().simple());
    let stopped = format!("wf-stopped-{}", Uuid::new_v4().simple());
    let both = [live.as_str(), stopped.as_str()];

    let event = insert_event(&pool, &tenant, node).await;
    // 稼働側のみ配送 ＋ RAG ack 済み。停止側は未配送のまま。
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered(&mut tx, &live, &[event]).await.unwrap();
        mark_processed(&mut tx, &[event]).await.unwrap();
        tx.commit().await.unwrap();
    }

    {
        let mut tx = pool.begin().await.unwrap();
        gc_delivered(&mut tx, &both, 1_000).await.unwrap();
        tx.commit().await.unwrap();
    }
    assert!(
        event_exists(&pool, event).await,
        "停止中コンシューマが未配送のイベントを GC してはいけない（イベント喪失）"
    );

    // 停止側も配送すれば GC 対象になる。
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered(&mut tx, &stopped, &[event]).await.unwrap();
        tx.commit().await.unwrap();
    }
    assert!(
        gc_until_gone(&pool, &both, event).await,
        "全台帳コンシューマ配送済みなら GC される"
    );
}

/// `gc_delivered_registered` は登録台帳（`outbox_consumer`）を判定に使う。
///
/// 登録済みかつ未配送のコンシューマが居る限り削除しない（安全側の方向のみ検証。
/// 他テストの登録が増えても結論は反転しない）。
#[tokio::test]
async fn registered_gc_reads_consumer_registry() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let consumer = format!("wf-registry-{}", Uuid::new_v4().simple());

    {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, &consumer).await.unwrap();
        tx.commit().await.unwrap();
    }
    // 登録が台帳から読めること（GC が参照する正本）。
    let names = {
        let mut conn = pool.acquire().await.unwrap();
        storage::event::registered_consumers(&mut conn)
            .await
            .unwrap()
    };
    assert!(names.contains(&consumer), "登録が台帳から読める");

    // 登録後の新規イベント（fast-forward 対象外＝このコンシューマには未配送）を ack 済みにする。
    let event = insert_event(&pool, &tenant, node).await;
    {
        let mut tx = pool.begin().await.unwrap();
        mark_processed(&mut tx, &[event]).await.unwrap();
        tx.commit().await.unwrap();
    }

    {
        let mut tx = pool.begin().await.unwrap();
        storage::event::gc_delivered_registered(&mut tx, 1_000)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    assert!(
        event_exists(&pool, event).await,
        "登録済みコンシューマが未配送なら GC しない（登録台帳を見ている証拠）"
    );

    // 後片付け（登録台帳はテスト間で共有されるため必ず外す）。
    unregister(&pool, &consumer).await;
}

/// コンシューマを恒久廃止すると登録が消え、GC がその分を待たなくなる。
///
/// これが無いと「コードからコンシューマを消しただけ」で GC が永久停止する。
#[tokio::test]
async fn unregister_consumer_unblocks_gc() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let retired = format!("wf-retired-{}", Uuid::new_v4().simple());
    // keeper = 廃止しない側。retired が集合から抜けたら GC が進むことを見るために置く
    // （空集合 `&[]` を使わない理由は下記コメント参照）。
    let keeper = format!("wf-keeper-{}", Uuid::new_v4().simple());

    for c in [&retired, &keeper] {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, c).await.unwrap();
        tx.commit().await.unwrap();
    }
    let event = insert_event(&pool, &tenant, node).await;
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered(&mut tx, &keeper, &[event]).await.unwrap();
        mark_processed(&mut tx, &[event]).await.unwrap();
        tx.commit().await.unwrap();
    }

    // 廃止コンシューマが未配送のうちは、その集合を渡した GC は待つ。
    {
        let mut tx = pool.begin().await.unwrap();
        gc_delivered(&mut tx, &[retired.as_str(), keeper.as_str()], 1_000)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    assert!(
        event_exists(&pool, event).await,
        "登録が残っている間は GC が待つ"
    );

    // 廃止 → 登録台帳から消える。
    let removed = {
        let mut tx = pool.begin().await.unwrap();
        let r = storage::event::unregister_consumer(&mut tx, &retired)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        r
    };
    assert!(removed, "登録済みコンシューマは廃止できる");
    let names = {
        let mut conn = pool.acquire().await.unwrap();
        storage::event::registered_consumers(&mut conn)
            .await
            .unwrap()
    };
    assert!(
        !names.contains(&retired),
        "廃止したコンシューマは登録台帳から消える＝GC が待たなくなる"
    );

    // 廃止後の集合（= 登録台帳から retired が抜けた状態）では GC が進む。
    //
    // ⚠️ ここで空集合 `&[]` を渡してはいけない。「待つべきコンシューマ無し」は
    // **`processed_at` ack 済みの全行**が対象になる総ざらいで、共有テスト DB では並行して
    // 走る他テストの行まで消してしまう（実際に他テストを落とした）。
    assert!(
        gc_until_gone(&pool, &[keeper.as_str()], event).await,
        "廃止後は GC が進む（永久停止しない）"
    );

    // 二重廃止は false（冪等に扱える）。
    let again = {
        let mut tx = pool.begin().await.unwrap();
        let r = storage::event::unregister_consumer(&mut tx, &retired)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        r
    };
    assert!(!again, "未登録の廃止は false");
}

/// `batch` 上限を超えて削除しない（大きな DELETE を 1 文で撃たない刻み）。
#[tokio::test]
async fn gc_respects_batch_limit() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let consumer = format!("wf-batch-{}", Uuid::new_v4().simple());

    // 3 件を「配送済み＋ack 済み」にする。
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(insert_event(&pool, &tenant, node).await);
    }
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered(&mut tx, &consumer, &ids).await.unwrap();
        mark_processed(&mut tx, &ids).await.unwrap();
        tx.commit().await.unwrap();
    }

    // batch=2 なら 1 回で 2 件までしか消えない。
    let deleted = {
        let mut tx = pool.begin().await.unwrap();
        let n = gc_delivered(&mut tx, &[consumer.as_str()], 2)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        n
    };
    assert!(deleted <= 2, "batch 上限を超えて削除しない: {deleted}");

    // 残りは次バッチで消える（刻んでも最終的に全部消える）。
    for id in ids {
        assert!(
            gc_until_gone(&pool, &[consumer.as_str()], id).await,
            "刻んでも最終的に全て GC される"
        );
    }
}

/// 滞留観測は登録コンシューマの lag を返す（GC が進まない原因の切り分け用）。
#[tokio::test]
async fn outbox_backlog_reports_consumer_lag() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let node = Uuid::new_v4();
    let consumer = format!("wf-lag-{}", Uuid::new_v4().simple());

    {
        let mut tx = pool.begin().await.unwrap();
        register_consumer(&mut tx, &consumer).await.unwrap();
        tx.commit().await.unwrap();
    }
    // 登録後に 1 件積む（このコンシューマには未配送＝lag が立つ）。
    let event = insert_event(&pool, &tenant, node).await;

    let backlog = {
        let mut conn = pool.acquire().await.unwrap();
        storage::event::outbox_backlog(&mut conn).await.unwrap()
    };
    assert!(
        backlog.latest_event_id >= event,
        "最大 event id は投入済みイベントを含む"
    );
    let mine = backlog
        .consumers
        .iter()
        .find(|c| c.consumer == consumer)
        .expect("登録コンシューマが列挙される");
    assert!(mine.lag > 0, "未配送イベントがあれば lag が立つ");
    assert!(backlog.max_lag() >= mine.lag);

    // 配送すると配送済み最大 id が進む。
    {
        let mut tx = pool.begin().await.unwrap();
        mark_delivered(&mut tx, &consumer, &[event]).await.unwrap();
        tx.commit().await.unwrap();
    }
    let after = {
        let mut conn = pool.acquire().await.unwrap();
        storage::event::outbox_backlog(&mut conn).await.unwrap()
    };
    let mine_after = after
        .consumers
        .iter()
        .find(|c| c.consumer == consumer)
        .expect("登録コンシューマが列挙される");
    assert!(
        mine_after.delivered_upto >= event,
        "配送済み最大 id が進む: {} < {event}",
        mine_after.delivered_upto
    );

    unregister(&pool, &consumer).await;
}

/// 指定イベントが GC されるまで GC を回す（消えたら `true`、上限まで残れば `false`）。
///
/// GC は `FOR UPDATE SKIP LOCKED` なので、**他の並行テストが同じ行をロックしていればその回は
/// スキップされる**（本番では次の GC 周期で回収される＝意図した挙動）。共有テスト DB では
/// これが偽陰性になるため、消えるまで数回試す。
async fn gc_until_gone(pool: &PgPool, consumers: &[&str], id: i64) -> bool {
    for _ in 0..20 {
        if !event_exists(pool, id).await {
            return true;
        }
        {
            let mut tx = pool.begin().await.unwrap();
            gc_delivered(&mut tx, consumers, 1_000).await.unwrap();
            tx.commit().await.unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    !event_exists(pool, id).await
}

/// outbox 行の存在確認。
async fn event_exists(pool: &PgPool, id: i64) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM storage_event_outbox WHERE id = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("exists")
}

/// テスト用の登録解除（`outbox_consumer` はテスト間共有なので必ず片付ける）。
async fn unregister(pool: &PgPool, consumer: &str) {
    let mut tx = pool.begin().await.unwrap();
    storage::event::unregister_consumer(&mut tx, consumer)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
