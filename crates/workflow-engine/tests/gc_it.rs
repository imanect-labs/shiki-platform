//! 実行履歴の保持期間 GC の結合テスト（#444・engine.md §12.2・実 Postgres）。
//!
//! - 保持期間を過ぎた terminal run が消え、step/event/wait_subscription が CASCADE で道連れになる
//! - **実行中/待機中の run は保持期間を跨いでも消えない**（§12.2）
//! - 保持期間内の terminal run は消えない
//! - 保持期間はテナントごとに効く
//! - effect_journal が TTL で消える
//! - 日次投入が二重に積まれない
//!
//! **削除の検証は `GcReport` の件数ではなく DB の事実で行う。** `daily_enqueue_is_not_duplicated`
//! が全テナント横断 GC（`purge_expired_history(None)`）を回すため、同一バイナリで並行実行すると
//! 他テストの期限切れ fixture を先に消し得る。件数で見ると 0 になって落ちるが、「消えるべきものが
//! 消えている」不変条件は変わらない。DB を見る形なら回帰検出力も落ちない（バグがあれば横断 GC も
//! 同じ理由で消せないため、行は残る）。

#![allow(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::run::graph::RunGraph;
use workflow_engine::RunStore;

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

fn ir() -> Value {
    json!({
        "ir_version": 1, "name": "gcme",
        "nodes": [{ "id": "a", "type": "debug.log", "params": {} }],
        "edges": [], "triggers": [], "declared_scopes": []
    })
}

/// テナントを保持期間つきで作る（GC はテナント設定を引くので実在が要る）。
async fn create_tenant(pool: &PgPool, retention_days: i32) -> String {
    let tenant = format!("t-{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO tenant (tenant_id, org, display_name, workflow_retention_days) \
         VALUES ($1, 'org', 'gc test', $2)",
    )
    .bind(&tenant)
    .bind(retention_days)
    .execute(pool)
    .await
    .unwrap();
    tenant
}

async fn create_run(store: &RunStore, tenant: &str) -> Uuid {
    let spec = ir();
    let graph = RunGraph::build(&workflow_engine::WorkflowIr::from_json(&spec).unwrap());
    store
        .create_run(
            tenant,
            "org",
            Uuid::new_v4(),
            1,
            "interactive",
            None,
            "u:1",
            "user",
            &json!({}),
            &spec,
            &graph,
        )
        .await
        .unwrap()
        .expect("run 作成")
}

/// run を terminal にして「N 日前に終わった」ことにする。
async fn finish_run_days_ago(pool: &PgPool, tenant: &str, run_id: Uuid, days: i32, status: &str) {
    sqlx::query(
        "UPDATE workflow_run SET status = $3, finished_at = now() - make_interval(days => $4) \
          WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant)
    .bind(run_id)
    .bind(status)
    .bind(days)
    .execute(pool)
    .await
    .unwrap();
}

async fn run_exists(pool: &PgPool, tenant: &str, run_id: Uuid) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant)
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap()
        > 0
}

async fn count_children(pool: &PgPool, table: &str, tenant: &str, run_id: Uuid) -> i64 {
    let sql = format!("SELECT count(*) FROM {table} WHERE tenant_id = $1 AND run_id = $2");
    sqlx::query_scalar::<_, i64>(&sql)
        .bind(tenant)
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn expired_terminal_run_is_purged_with_children() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = create_tenant(&pool, 90).await;
    let run_id = create_run(&store, &tenant).await;

    // wait_subscription は FK が無く孤児化しうるので、CASCADE が効くことを明示的に確かめる。
    sqlx::query(
        "INSERT INTO wait_subscription (tenant_id, run_id, step_path, kind, wake_at) \
         VALUES ($1, $2, 'a', 'timer', now())",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(count_children(&pool, "step_execution", &tenant, run_id).await > 0);
    assert!(count_children(&pool, "run_event", &tenant, run_id).await > 0);

    finish_run_days_ago(&pool, &tenant, run_id, 91, "succeeded").await;
    let report = store.purge_expired_history(Some(&tenant)).await.unwrap();
    assert!(!report.truncated, "この規模ではバッチ上限に当たらない");
    // 削除の検証は **report の件数ではなく DB の事実**で行う（下の run_exists / count_children）。
    // 同一バイナリの `daily_enqueue_is_not_duplicated` が全テナント横断 GC を回すため、先に
    // 消されると report は 0 になる。「消えるべきものが消えている」という不変条件は変わらない。

    assert!(!run_exists(&pool, &tenant, run_id).await);
    assert_eq!(
        count_children(&pool, "step_execution", &tenant, run_id).await,
        0
    );
    assert_eq!(count_children(&pool, "run_event", &tenant, run_id).await, 0);
    assert_eq!(
        count_children(&pool, "wait_subscription", &tenant, run_id).await,
        0,
        "wait_subscription も CASCADE で消える（孤児を残さない）"
    );
}

#[tokio::test]
async fn running_run_survives_regardless_of_age() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = create_tenant(&pool, 1).await;

    // 「1000 日前に作られてまだ終わっていない run」。finished_at が NULL なので対象外。
    let running = create_run(&store, &tenant).await;
    sqlx::query(
        "UPDATE workflow_run SET created_at = now() - interval '1000 days' \
          WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(running)
    .execute(&pool)
    .await
    .unwrap();

    // 比較対象: 同テナントの期限切れ terminal run は消える。
    let expired = create_run(&store, &tenant).await;
    finish_run_days_ago(&pool, &tenant, expired, 2, "failed").await;

    store.purge_expired_history(Some(&tenant)).await.unwrap();

    assert!(
        run_exists(&pool, &tenant, running).await,
        "terminal でない run は保持期間を跨いでも消さない（§12.2）"
    );
    assert!(!run_exists(&pool, &tenant, expired).await);
}

#[tokio::test]
async fn retention_is_per_tenant_and_respects_the_window() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    // 保持 7 日のテナントと保持 365 日のテナント。どちらも「30 日前に終わった run」を持つ。
    let short = create_tenant(&pool, 7).await;
    let long = create_tenant(&pool, 365).await;
    let short_run = create_run(&store, &short).await;
    let long_run = create_run(&store, &long).await;
    finish_run_days_ago(&pool, &short, short_run, 30, "succeeded").await;
    finish_run_days_ago(&pool, &long, long_run, 30, "succeeded").await;

    // 保持期間内（1 日前に終わった）の run は消えない。
    let fresh = create_run(&store, &short).await;
    finish_run_days_ago(&pool, &short, fresh, 1, "succeeded").await;

    store.purge_expired_history(Some(&short)).await.unwrap();
    store.purge_expired_history(Some(&long)).await.unwrap();

    assert!(
        !run_exists(&pool, &short, short_run).await,
        "保持 7 日 → 30 日前は消える"
    );
    assert!(
        run_exists(&pool, &long, long_run).await,
        "保持 365 日 → 30 日前は残る"
    );
    assert!(run_exists(&pool, &short, fresh).await, "保持期間内は残る");
}

/// effect_journal は「期限切れ かつ 所有 run が消えている」ものだけ消す。
///
/// 保持期間は run の生存期間より短く設定できるため、`created_at` だけで消すと**実行中の run の
/// 副作用記録が先に消え**、その step が再開したときに二重実行される（PIT-31 違反・#445）。
#[tokio::test]
async fn effect_journal_survives_while_its_run_is_alive() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    // 保持 1 日。run はまだ実行中で、その副作用記録は 10 日前のもの。
    let tenant = create_tenant(&pool, 1).await;
    let alive = create_run(&store, &tenant).await;
    let expired = create_run(&store, &tenant).await;
    finish_run_days_ago(&pool, &tenant, expired, 10, "succeeded").await;

    let key_of = |run: Uuid| format!("wf:{tenant}:{run}:a");
    for run in [alive, expired] {
        sqlx::query(
            "INSERT INTO effect_journal (tenant_id, idempotency_key, op_digest, created_at) \
             VALUES ($1, $2, 'digest', now() - interval '10 days')",
        )
        .bind(&tenant)
        .bind(key_of(run))
        .execute(&pool)
        .await
        .unwrap();
    }
    // script の `#cN` 連番付きキーも同じ run に属する（step_path 側に付く）。
    sqlx::query(
        "INSERT INTO effect_journal (tenant_id, idempotency_key, op_digest, created_at) \
         VALUES ($1, $2, 'digest', now() - interval '10 days')",
    )
    .bind(&tenant)
    .bind(format!("wf:{tenant}:{alive}:a#c1"))
    .execute(&pool)
    .await
    .unwrap();

    store.purge_expired_history(Some(&tenant)).await.unwrap();

    let left: Vec<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM effect_journal WHERE tenant_id = $1 ORDER BY 1",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        left,
        vec![key_of(alive), format!("wf:{tenant}:{alive}:a#c1")],
        "実行中 run の journal は期限を過ぎても残す（#cN 連番も同じ run に属する）"
    );
}

#[tokio::test]
async fn daily_enqueue_is_not_duplicated() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());

    // 初回は行の作成のみで、投入は次回以降（起動直後に重い削除を走らせない）。
    assert!(!store.enqueue_history_gc_if_due().await.unwrap());

    // 24h 経過を作る。
    sqlx::query(
        "UPDATE maintenance_schedule SET last_enqueued_at = now() - interval '25 hours' \
          WHERE job_name = 'workflow_history_gc'",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(
        store.enqueue_history_gc_if_due().await.unwrap(),
        "期限到来で 1 件積む"
    );
    assert!(
        !store.enqueue_history_gc_if_due().await.unwrap(),
        "同日中の再 tick では積まない"
    );

    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM job_queue WHERE queue = $1")
        .bind(workflow_engine::WORKFLOW_GC_QUEUE)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queued, 1);

    // ワーカーが消費すると ack されキューから消える。
    assert!(store.consume_history_gc_once().await.unwrap().is_some());
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM job_queue WHERE queue = $1")
        .bind(workflow_engine::WORKFLOW_GC_QUEUE)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    assert!(store.consume_history_gc_once().await.unwrap().is_none());
}

/// 生存 run の journal が 1 バッチ分を埋めても、その先の削除可能な journal に到達する。
///
/// 所有 run の条件を `LIMIT` の後段で適用すると、先頭 N 件がすべて生存 run のものだった場合に
/// 削除 0 件となり「消し切った」と誤判定して**その先へ二度と到達しない**（#445 レビュー指摘）。
#[tokio::test]
async fn journal_purge_reaches_past_a_full_batch_of_live_runs() {
    let Some(pool) = setup().await else { return };
    let store = RunStore::new(pool.clone());
    let tenant = create_tenant(&pool, 1).await;
    // 実行中の run（journal は消せない）と、期限切れ terminal の run（先に消える → journal も消せる）。
    let alive = create_run(&store, &tenant).await;
    let expired = create_run(&store, &tenant).await;
    finish_run_days_ago(&pool, &tenant, expired, 10, "succeeded").await;

    // 生存 run に紐づく journal を 1 バッチ（2000）より多く、しかも**より古い**時刻で積む。
    // created_at 昇順なので、後段で絞る実装ではこれらだけで LIMIT が埋まる。
    sqlx::query(
        "INSERT INTO effect_journal (tenant_id, idempotency_key, op_digest, created_at) \
         SELECT $1, 'wf:' || $1 || ':' || $2::text || ':n' || i, 'digest', \
                now() - interval '30 days' + make_interval(secs => i) \
           FROM generate_series(1, 2500) i",
    )
    .bind(&tenant)
    .bind(alive)
    .execute(&pool)
    .await
    .unwrap();
    // 削除可能な孤児はそれより新しい時刻に置く（＝走査順で後ろ）。
    sqlx::query(
        "INSERT INTO effect_journal (tenant_id, idempotency_key, op_digest, created_at) \
         VALUES ($1, $2, 'digest', now() - interval '5 days')",
    )
    .bind(&tenant)
    .bind(format!("wf:{tenant}:{expired}:n1"))
    .execute(&pool)
    .await
    .unwrap();

    store.purge_expired_history(Some(&tenant)).await.unwrap();

    // ここも report の件数ではなく DB の事実で見る（全テナント横断 GC を回す別テストと同時に
    // 走ると、孤児を先に消されて report が 0 になる）。
    let orphan_left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM effect_journal WHERE tenant_id = $1 AND idempotency_key = $2",
    )
    .bind(&tenant)
    .bind(format!("wf:{tenant}:{expired}:n1"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        orphan_left, 0,
        "生存 run の journal に阻まれず、その先の孤児へ到達する"
    );
    let alive_left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM effect_journal WHERE tenant_id = $1 AND idempotency_key LIKE $2",
    )
    .bind(&tenant)
    .bind(format!("wf:{tenant}:{alive}:%"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(alive_left, 2500, "生存 run の journal は 1 件も消さない");
}
