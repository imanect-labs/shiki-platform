//! 索引のオンラインバックフィルと parity 検証の結合テスト（Phase 15 Task 15.2 受け入れ条件）。
//!
//! - バックフィルが中断・再開でき、完了後に旧索引（＝本体に式を当てた内容）と新索引が一致する
//! - `building` 中の書込（走査済み・未走査・新規・削除）が完成後の索引に反映される（PIT-72）
//! - 進捗（件数・状態）が観測できる・parity の不一致を検出できる
//! - スキーマ改訂で索引が増えると building → active、外すと absent になり行が消える
//! - `purge_tenant` が索引テーブルと台帳を消す
//!
//! ジョブキュー経由の実行は `index_backfill_job_it.rs`（キューを消費するので別バイナリで直列に走らせる）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

mod common;

use common::*;
use data::{DataStore, FieldType, TableSchema};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// rich_schema ＋ 顧客への record_ref。
fn schema_with_link(customer: Uuid) -> TableSchema {
    let mut s = rich_schema();
    let mut c = field("customer", FieldType::RecordRef);
    c.ref_table = Some(customer);
    s.fields.push(c);
    s
}

/// Task 15.2 より前の状態を再現する: 索引行・台帳・進捗を消し、台帳を未初期化に戻す。
async fn wipe_pivot_index(pool: &PgPool, tenant: &str, table: Uuid) {
    let (ti, tb) = keys_of(pool, tenant, table).await;
    for t in [
        "data_index_text",
        "data_index_num",
        "data_link",
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
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE data_table SET pivot_index_init = false WHERE tenant_id = $1 AND id = $2")
        .bind(tenant)
        .bind(table)
        .execute(pool)
        .await
        .unwrap();
}

async fn seed_records(
    store: &DataStore,
    c: &authz::AuthContext,
    table: Uuid,
    customers: &[Uuid],
    n: usize,
) -> Vec<data::DataRecord> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut v = json!({
            "title": format!("申請 {i}"),
            "amount": i,
            "tags": if i % 3 == 0 { json!(["a", "b"]) } else { json!(["c"]) },
            "status": if i % 2 == 0 { "draft" } else { "submitted" },
            "due": format!("2026-10-{:02}", i % 28 + 1),
        });
        if i % 4 != 3 {
            v["customer"] = json!(customers[i % customers.len()].to_string());
        }
        out.push(store.create_record(c, table, v, None).await.unwrap());
    }
    out
}

fn states(rows: &[(i16, i16, String)]) -> Vec<&str> {
    let mut s: Vec<&str> = rows.iter().map(|(_, _, st)| st.as_str()).collect();
    s.sort_unstable();
    s.dedup();
    s
}

/// 受け入れ条件: バックフィルが中断・再開でき、完了後に旧索引と新索引の内容が一致する。
/// PIT-72: building 中の書込が完成後の索引に反映される。
#[tokio::test]
async fn backfill_resumes_after_interruption_and_matches_old_index() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let customer = create_table(
        &store,
        &c,
        "customer",
        schema(vec![field("name", FieldType::Text)]),
    )
    .await;
    let mut customers = Vec::new();
    for i in 0..3 {
        customers.push(
            store
                .create_record(
                    &c,
                    customer.id,
                    json!({ "name": format!("顧客 {i}") }),
                    None,
                )
                .await
                .unwrap()
                .id,
        );
    }
    let table = create_table(&store, &c, "expense", schema_with_link(customer.id)).await;
    let records = seed_records(&store, &c, table.id, &customers, 230).await;
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;

    // 書込経路で作った索引と、バックフィルで作り直した索引が同じになることを比べるため控える。
    let snapshot = |pool: PgPool| async move {
        let rows: Vec<(i16, i64, i16, String)> = sqlx::query_as(
            "SELECT field_id, record_int, ord, val FROM data_index_text \
               WHERE tenant_int = $1 AND table_int = $2 \
             UNION ALL SELECT field_id, record_int, ord, val::text FROM data_index_num \
               WHERE tenant_int = $1 AND table_int = $2 \
             UNION ALL SELECT field_id, src_record, ord, dst_record::text FROM data_link \
               WHERE tenant_int = $1 AND table_int = $2 \
             UNION ALL SELECT field_id, record_int, 0::int2, val_norm FROM data_unique \
               WHERE tenant_int = $1 AND table_int = $2 \
             UNION ALL SELECT field_id, record_int, ord, val FROM data_index_search \
               WHERE tenant_int = $1 AND table_int = $2 \
             ORDER BY 1, 2, 3, 4",
        )
        .bind(ti)
        .bind(tb)
        .fetch_all(&pool)
        .await
        .unwrap();
        rows
    };
    let written_by_write_path = snapshot(pool.clone()).await;
    assert!(!written_by_write_path.is_empty());

    wipe_pivot_index(&pool, &tenant, table.id).await;
    assert!(snapshot(pool.clone()).await.is_empty());

    // 初回アクセスで台帳が作られる（既存レコードがあるので building・バックフィルを積む）。
    // unique（index_table = 4）だけは同じ Tx で同期構築して active（一意性の強制に空白期間を作らない・
    // Task 15.3 で旧方式の一意な式インデックスを撤去したため）。
    store.get_table(&c, table.id, None).await.unwrap();
    let ledger_rows = ledger(&pool, ti, tb).await;
    let (unique, others): (Vec<_>, Vec<_>) =
        ledger_rows.iter().cloned().partition(|(_, t, _)| *t == 4);
    assert_eq!(states(&others), vec!["building"], "{ledger_rows:?}");
    assert_eq!(states(&unique), vec!["active"], "{ledger_rows:?}");
    let status = store
        .index_backfill_status(&tenant, table.id)
        .await
        .unwrap()
        .expect("進捗がある");
    assert_eq!(
        (status.status.as_str(), status.rows_total, status.rows_done),
        ("running", 230, 0)
    );

    // 2 チャンクだけ進めて「中断」する。
    for _ in 0..2 {
        let step = store
            .index_backfill_step(&tenant, table.id, 50)
            .await
            .unwrap();
        assert_eq!(step.processed, 50);
        assert!(!step.done);
    }
    let status = store
        .index_backfill_status(&tenant, table.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.rows_done, 100, "進捗が観測できる");
    let rows = ledger(&pool, ti, tb).await;
    let others: Vec<_> = rows.iter().filter(|r| r.1 != 4).cloned().collect();
    assert_eq!(states(&others), vec!["building"]);

    // building 中の書込: 走査済みの更新・未走査の更新・新規・削除（走査済み／未走査）。
    let scanned = &records[5];
    let unscanned = &records[200];
    store
        .update_record(
            &c,
            table.id,
            scanned.id,
            json!({"title": "走査後に変更", "amount": 10_000, "tags": ["d"]}),
            1,
            None,
        )
        .await
        .unwrap();
    store
        .update_record(
            &c,
            table.id,
            unscanned.id,
            json!({"status": "approved"}),
            1,
            None,
        )
        .await
        .unwrap();
    let created = store
        .create_record(
            &c,
            table.id,
            json!({"title": "building 中に作成", "amount": 20_000,
                   "customer": customers[0].to_string()}),
            None,
        )
        .await
        .unwrap();
    store
        .delete_record(&c, table.id, records[10].id, 1, None)
        .await
        .unwrap();
    store
        .delete_record(&c, table.id, records[220].id, 1, None)
        .await
        .unwrap();

    // 別のストア（＝再起動したワーカー）で再開し、最後まで進める。
    let resumed = store_with(pool.clone());
    let mut steps = 0;
    loop {
        steps += 1;
        let step = resumed
            .index_backfill_step(&tenant, table.id, 50)
            .await
            .unwrap();
        if step.done {
            break;
        }
        assert!(steps < 10, "収束する");
    }
    assert_eq!(
        states(&ledger(&pool, ti, tb).await),
        vec!["active"],
        "原子的に active"
    );
    let status = resumed
        .index_backfill_status(&tenant, table.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.status, "done");
    assert!(status.finished_at.is_some());
    // 完了後の再実行は何もしない（冪等）。
    let again = resumed
        .index_backfill_step(&tenant, table.id, 50)
        .await
        .unwrap();
    assert_eq!((again.processed, again.done), (0, true));

    // parity: 旧索引（本体に式を当てた内容）と新索引が一致する。
    let report = resumed
        .verify_index_parity(&tenant, table.id)
        .await
        .unwrap();
    assert!(report.is_consistent(), "{report:?}");
    // 擬似列 3 ＋ title(text/search) ＋ amount(num/unique) ＋ tags ＋ status ＋ due ＋ customer(link)。
    assert_eq!(report.checked, 3 + 2 + 2 + 1 + 1 + 1 + 1);
    let status = resumed
        .index_backfill_status(&tenant, table.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.parity_mismatches, Some(0));
    assert!(status.parity_checked_at.is_some());

    // building 中の変更が反映されている。
    let title_id = i16::try_from(table.schema.field("title").unwrap().id).unwrap();
    let has_title = |v: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM data_index_text WHERE tenant_int = $1 \
                 AND table_int = $2 AND field_id = $3 AND val = $4)",
            )
            .bind(ti)
            .bind(tb)
            .bind(title_id)
            .bind(v)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert!(has_title("走査後に変更").await);
    assert!(has_title("building 中に作成").await);
    assert!(!has_title("申請 5").await, "走査済みの古い値は残らない");
    assert!(
        !has_title("申請 10").await,
        "削除したレコードの行は残らない"
    );
    assert!(!has_title("申請 220").await);
    let _ = created;

    // バックフィル結果は書込経路で作った索引と同じ形（削除・更新分を除けば同一）。
    let rebuilt = snapshot(pool.clone()).await;
    let changed: Vec<i64> = {
        let mut v = Vec::new();
        for r in [scanned, unscanned, &records[10], &records[220]] {
            if let Ok(ri) = sqlx::query_scalar::<_, i64>(
                "SELECT record_int FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND id = $3",
            )
            .bind(ti)
            .bind(tb)
            .bind(r.id)
            .fetch_one(&pool)
            .await
            {
                v.push(ri);
            }
        }
        v
    };
    let stable = |rows: &[(i16, i64, i16, String)]| -> Vec<(i16, i64, i16, String)> {
        rows.iter()
            .filter(|(f, r, _, _)| *f != 2 && !changed.contains(r))
            .filter(|(_, r, _, _)| {
                // 削除した 2 件と新規 1 件を除く（作成前の控えに無い／作り直しに無い）。
                written_by_write_path.iter().any(|(_, w, _, _)| w == r)
                    && rebuilt.iter().any(|(_, b, _, _)| b == r)
            })
            .cloned()
            .collect()
    };
    assert_eq!(
        stable(&rebuilt),
        stable(&written_by_write_path),
        "バックフィルは書込経路と同じ索引を作る"
    );

    // parity は欠落を検出する（索引行を 1 行消す）→ 書込で直れば一致に戻る。
    let amount_id = i16::try_from(table.schema.field("amount").unwrap().id).unwrap();
    let victim = &records[50];
    let vri = record_int_of(&pool, ti, tb, victim.id).await;
    sqlx::query(
        "DELETE FROM data_index_num WHERE tenant_int = $1 AND table_int = $2 \
         AND field_id = $3 AND record_int = $4",
    )
    .bind(ti)
    .bind(tb)
    .bind(amount_id)
    .bind(vri)
    .execute(&pool)
    .await
    .unwrap();
    let broken = resumed
        .verify_index_parity(&tenant, table.id)
        .await
        .unwrap();
    assert_eq!(broken.mismatches.len(), 1, "{broken:?}");
    assert_eq!(broken.mismatches[0].field, "amount");
    assert_eq!(broken.mismatches[0].index_table, "data_index_num");
    assert_eq!(
        (broken.mismatches[0].missing, broken.mismatches[0].extra),
        (1, 0)
    );
    let s = resumed
        .index_backfill_status(&tenant, table.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.parity_mismatches, Some(1));
    store
        .update_record(&c, table.id, victim.id, json!({"amount": 50_000}), 1, None)
        .await
        .unwrap();
    let fixed = resumed
        .verify_index_parity(&tenant, table.id)
        .await
        .unwrap();
    assert!(fixed.is_consistent(), "{fixed:?}");
}

/// スキーマ改訂で索引を足すと building → バックフィル → active、外すと absent になり行が消える。
#[tokio::test]
async fn schema_change_promotes_and_demotes_indexes() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = create_table(&store, &c, "promote", rich_schema()).await;
    for i in 0..40 {
        store
            .create_record(
                &c,
                table.id,
                json!({"title": "x", "memo": format!("備考 {i}")}),
                None,
            )
            .await
            .unwrap();
    }
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let memo_id = i16::try_from(table.schema.field("memo").unwrap().id).unwrap();
    let memo_state = |pool: PgPool| async move {
        ledger(&pool, ti, tb)
            .await
            .into_iter()
            .filter(|(f, _, _)| *f == memo_id)
            .map(|(_, t, s)| (t, s))
            .collect::<Vec<_>>()
    };
    assert!(
        memo_state(pool.clone()).await.is_empty(),
        "未宣言は台帳に無い"
    );

    // 昇格: memo を indexed ＋ searchable に。既存レコードがあるので building。
    let mut next = table.schema.clone();
    let memo = next.fields.iter_mut().find(|f| f.key == "memo").unwrap();
    memo.indexed = true;
    memo.searchable = true;
    let up = store
        .update_table_schema(&c, table.id, next, None, None)
        .await
        .unwrap();
    assert_eq!(
        memo_state(pool.clone()).await,
        vec![(1, "building".to_string()), (5, "building".to_string())]
    );
    // 既存の索引は active のまま（作り直さない）。
    assert!(ledger(&pool, ti, tb)
        .await
        .iter()
        .filter(|(f, _, _)| *f != memo_id)
        .all(|(_, _, s)| s == "active"));
    // building 中の新規書込は write-through。
    store
        .create_record(&c, table.id, json!({"title": "y", "memo": "昇格中"}), None)
        .await
        .unwrap();
    store.backfill_table_index(&tenant, table.id).await.unwrap();
    assert_eq!(
        memo_state(pool.clone()).await,
        vec![(1, "active".to_string()), (5, "active".to_string())]
    );
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_index_text WHERE tenant_int = $1 AND table_int = $2 \
         AND field_id = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(memo_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 41);
    let report = store.verify_index_parity(&tenant, table.id).await.unwrap();
    assert!(report.is_consistent(), "{report:?}");

    // 降格: searchable を外す → search は absent・行は同じ Tx で消える。
    let mut down = up.schema.clone();
    down.fields
        .iter_mut()
        .find(|f| f.key == "memo")
        .unwrap()
        .searchable = false;
    store
        .update_table_schema(&c, table.id, down, None, None)
        .await
        .unwrap();
    assert_eq!(
        memo_state(pool.clone()).await,
        vec![(1, "active".to_string()), (5, "absent".to_string())]
    );
    let search_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_index_search WHERE tenant_int = $1 AND table_int = $2 \
         AND field_id = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(memo_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(search_rows, 0);
    // 空のテーブルへの宣言追加は即 active（バックフィル不要）。
    let empty = create_table(&store, &c, "empty", rich_schema()).await;
    let mut e = empty.schema.clone();
    e.fields
        .iter_mut()
        .find(|f| f.key == "memo")
        .unwrap()
        .indexed = true;
    store
        .update_table_schema(&c, empty.id, e, None, None)
        .await
        .unwrap();
    let (eti, etb) = keys_of(&pool, &tenant, empty.id).await;
    assert_eq!(states(&ledger(&pool, eti, etb).await), vec!["active"]);
}

/// purge_tenant が索引テーブル・リンク宣言・台帳・進捗を全て消す。
#[tokio::test]
async fn purge_tenant_removes_index_tables() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let customer = create_table(
        &store,
        &c,
        "customer",
        schema(vec![field("name", FieldType::Text)]),
    )
    .await;
    let cust = store
        .create_record(&c, customer.id, json!({"name": "A"}), None)
        .await
        .unwrap();
    let table = create_table(&store, &c, "purge", schema_with_link(customer.id)).await;
    store
        .create_record(
            &c,
            table.id,
            json!({"title": "x", "amount": 1, "tags": ["a"], "customer": cust.id.to_string()}),
            None,
        )
        .await
        .unwrap();
    // 進捗行も作る（スキーマ改訂で building にする）。
    let mut next = table.schema.clone();
    next.fields
        .iter_mut()
        .find(|f| f.key == "memo")
        .unwrap()
        .indexed = true;
    store
        .update_table_schema(&c, table.id, next, None, None)
        .await
        .unwrap();
    let (ti, _) = keys_of(&pool, &tenant, table.id).await;
    let tables = [
        "data_index_text",
        "data_index_num",
        "data_link",
        "data_link_field",
        "data_unique",
        "data_index_search",
        "data_index_state",
        "data_index_backfill",
    ];
    let count = |pool: PgPool, t: &'static str| async move {
        sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {t} WHERE tenant_int = $1"))
            .bind(ti)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    for t in tables {
        assert!(count(pool.clone(), t).await > 0, "{t} に行がある（前提）");
    }
    store.purge_tenant(&tenant).await.unwrap();
    for t in tables {
        assert_eq!(count(pool.clone(), t).await, 0, "{t} が消える");
    }
    let records: i64 = sqlx::query_scalar("SELECT count(*) FROM data_record WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(records, 0);
}
