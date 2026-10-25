//! 索引バックフィルのジョブ経路（jobq）の結合テスト（Phase 15 Task 15.2）。
//!
//! 常駐ワーカーと同じ手順（スイープ → ジョブ消費）で、台帳未初期化のテーブル（Task 15.2 より前に
//! 作られたもの・識別子未移行の v1 テーブルを含む）が初期化・バックフィルされ、完了と同じ Tx で
//! 積まれた parity 検証ジョブまで走ることを確かめる。キューを消費するので他の結合テストと
//! 並行しないよう、このバイナリはこのテスト 1 本だけにする。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

mod common;

use common::*;
use data::{FieldType, INDEX_BACKFILL_QUEUE};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn sweep_and_jobs_initialize_backfill_and_verify_parity() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    // ① v2 のテーブル（作成後に「Task 15.2 より前」の状態へ戻す）。
    let table = create_table(&store, &c, "v2", rich_schema()).await;
    for i in 0..120 {
        store
            .create_record(
                &c,
                table.id,
                json!({"title": format!("t{i}"), "amount": i, "tags": ["a"], "status": "draft"}),
                None,
            )
            .await
            .unwrap();
    }
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    for t in [
        "data_index_text",
        "data_index_num",
        "data_unique",
        "data_index_search",
        "data_index_state",
        "data_index_backfill",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {t} WHERE tenant_int = $1 AND table_int = $2"
        ))
        .bind(ti)
        .bind(tb)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE data_table SET pivot_index_init = false WHERE tenant_id = $1 AND id = $2")
        .bind(&tenant)
        .bind(table.id)
        .execute(&pool)
        .await
        .unwrap();

    // ② v1 形式（識別子未移行）のテーブル。スイープが識別子を移行してから台帳を作る。
    let mut conn = pool.acquire().await.unwrap();
    let placement = storage::tenant::ensure_placement(&mut conn, &tenant)
        .await
        .unwrap();
    let legacy_int = storage::tenant::allocate_table_int(&mut conn)
        .await
        .unwrap();
    let legacy: Uuid = sqlx::query_scalar(
        "INSERT INTO data_table (tenant_id, org, name, schema, created_by, tenant_int, table_int, \
           key_format, next_field_id) \
         VALUES ($1, 'acme', 'legacy', $2, 'alice', $3, $4, 1, 16) RETURNING id",
    )
    .bind(&tenant)
    .bind(json!({"fields": [
        {"name": "title", "type": "text", "indexed": true},
        {"name": "price", "type": "number", "unique": true}
    ]}))
    .bind(placement.tenant_int)
    .bind(legacy_int)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    for i in 0..30 {
        sqlx::query(
            "INSERT INTO data_record (tenant_id, table_id, org, data, owner, tenant_int, table_int) \
             VALUES ($1, $2, 'acme', $3, 'bob', $4, $5)",
        )
        .bind(&tenant)
        .bind(legacy)
        .bind(json!({"title": format!("旧 {i}"), "price": i * 10}))
        .bind(placement.tenant_int)
        .bind(legacy_int)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    drop(conn);

    // 常駐ワーカーと同じ手順: スイープ（初期化＋積み直し）→ ジョブを消費し尽くす。
    // スイープは 1 回あたりの初期化数に上限があるので、自分のテーブルが初期化されるまで回す。
    let init_done = |id: Uuid| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT pivot_index_init FROM data_table WHERE tenant_id = $1 AND id = $2",
            )
            .bind(&tenant)
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    for round in 0.. {
        store.enqueue_index_backfills_if_needed().await.unwrap();
        if init_done(table.id).await && init_done(legacy).await {
            break;
        }
        assert!(round < 100, "スイープで初期化される");
    }
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM job_queue WHERE queue = $1 AND tenant_id = $2")
            .bind(INDEX_BACKFILL_QUEUE)
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(pending >= 2, "テーブルごとにジョブが積まれる: {pending}");
    let mut consumed = 0;
    loop {
        let done = |id: Uuid| {
            let store = store.clone();
            let tenant = tenant.clone();
            async move {
                store
                    .index_backfill_status(&tenant, id)
                    .await
                    .unwrap()
                    .is_some_and(|s| s.status == "done" && s.parity_checked_at.is_some())
            }
        };
        if done(table.id).await && done(legacy).await {
            break;
        }
        assert!(
            store.consume_index_job_once().await.unwrap(),
            "完了前にキューが空になった"
        );
        consumed += 1;
        assert!(consumed < 5_000, "収束する");
    }
    for id in [table.id, legacy] {
        let s = store
            .index_backfill_status(&tenant, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.parity_mismatches, Some(0), "{id}: {s:?}");
        let r = store.verify_index_parity(&tenant, id).await.unwrap();
        assert!(r.is_consistent(), "{r:?}");
    }
    assert_eq!(
        ledger(&pool, ti, tb)
            .await
            .iter()
            .filter(|(_, _, s)| s != "active")
            .count(),
        0
    );
    // v1 テーブルは識別子も移行済みで、索引は "f{id}" の値から作られている。
    let legacy_table = store.get_table(&c, legacy, None).await.unwrap();
    assert_eq!(
        legacy_table.schema.field("price").unwrap().field_type,
        FieldType::Number
    );
    let (lti, ltb) = keys_of(&pool, &tenant, legacy).await;
    let prices: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_unique WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(lti)
    .bind(ltb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(prices, 30);
    // 旧方式の式インデックスは張らない（Task 15.3 で撤去・ランタイム DDL ゼロ）。読取は v2 の索引で効く。
    let legacy_registry: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('data_index_registry')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(legacy_registry.is_none());
    // 片付いた後のスイープでは、このテナントのテーブルを再初期化しない（冪等）。
    store.enqueue_index_backfills_if_needed().await.unwrap();
    let status = store
        .index_backfill_status(&tenant, table.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((status.status.as_str(), status.generation), ("done", 1));
}
