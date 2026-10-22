//! v1 の `record_ref` の保存値（単一 UUID 文字列）→ 配列の移行（data-platform §7.1 / §13 M1）の結合テスト。
//!
//! 移行前のデータは、v1 のバイナリが書いた形（文字列・大文字を含む）を DB に直接置いて再現する
//! （索引行 `data_link` / `data_unique` は v1 と同じ形で書かれている）。
//!
//! - 移行の前・途中・後で、読取（get / クエリの索引経路・本体経路 / lookup / 履歴）と書込が同じ値を見る
//! - チャンク単位で進み、途中で旧バイナリが文字列を書き戻しても末尾の検査でもう一周して拾う
//! - 冪等（もう一度流しても何も変わらない）・`rev` / `updated_at` と索引行は動かない・parity が一致する
//! - 常駐ワーカー（索引ジョブキュー）経由でも完了する

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::many_single_char_names
)]

mod common;

use chrono::{DateTime, Utc};
use common::{create_table, ctx, field, keys_of, schema, setup, store_with, unique_tenant};
use data::{
    CondOp, Condition, DataError, DataQuery, DataStore, FieldDef, FieldType, LookupDef,
    QueryTuning, ReadPath,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// 未移行テーブルを作るテストを直列にする（スイープは全テーブルを拾うので、ジョブ経路のテストが
/// 他のテストのテーブルを途中で移行してしまわないように）。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn v1_ref(key: &str, to: Uuid) -> FieldDef {
    let mut f = field(key, FieldType::RecordRef);
    f.ref_table = Some(to);
    f.indexed = true;
    f
}

/// 1 レコードの保存形（JSONB）をそのまま引く。
async fn stored(pool: &PgPool, ti: i32, tb: i64, id: Uuid) -> Value {
    sqlx::query_scalar::<_, sqlx::types::Json<Value>>(
        "SELECT data FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND id = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
    .0
}

/// v1 のバイナリが書いた形に戻す（`fields` の値を単一 UUID 文字列に・`upper` なら大文字）。
async fn write_legacy(pool: &PgPool, ti: i32, tb: i64, id: Uuid, fields: &[(&str, bool)]) {
    for (jk, upper) in fields {
        sqlx::query(
            "UPDATE data_record SET data = jsonb_set(data, ARRAY[$4::text], \
               to_jsonb(CASE WHEN $5 THEN upper(data -> $4 ->> 0) ELSE data -> $4 ->> 0 END)) \
             WHERE tenant_int = $1 AND table_int = $2 AND id = $3 \
               AND jsonb_typeof(data -> $4) = 'array'",
        )
        .bind(ti)
        .bind(tb)
        .bind(id)
        .bind(*jk)
        .bind(*upper)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn legacy_count(pool: &PgPool, ti: i32, tb: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM data_record r, jsonb_each(r.data) e \
         WHERE r.tenant_int = $1 AND r.table_int = $2 AND jsonb_typeof(e.value) = 'string' \
           AND e.key IN ('f17', 'f18')",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn format_of(pool: &PgPool, ti: i32, tb: i64) -> i16 {
    sqlx::query_scalar(
        "SELECT link_value_format FROM data_table WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 索引行（data_link の順方向・data_unique）のスナップショット。
async fn index_rows(
    pool: &PgPool,
    ti: i32,
    tb: i64,
) -> (Vec<(i64, i16, i16, i64)>, Vec<(i64, String)>) {
    let links = sqlx::query_as(
        "SELECT src_record, field_id, ord, dst_record FROM data_link \
         WHERE tenant_int = $1 AND table_int = $2 AND NOT is_reverse ORDER BY 1, 2, 3",
    )
    .bind(ti)
    .bind(tb)
    .fetch_all(pool)
    .await
    .unwrap();
    let uniq = sqlx::query_as(
        "SELECT record_int, val_norm FROM data_unique \
         WHERE tenant_int = $1 AND table_int = $2 ORDER BY 1",
    )
    .bind(ti)
    .bind(tb)
    .fetch_all(pool)
    .await
    .unwrap();
    (links, uniq)
}

/// 版と更新時刻（移行で動かないこと）。
async fn revs(pool: &PgPool, ti: i32, tb: i64) -> Vec<(Uuid, i64, DateTime<Utc>)> {
    sqlx::query_as(
        "SELECT id, rev, updated_at FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2 ORDER BY id",
    )
    .bind(ti)
    .bind(tb)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn query_ids(
    store: &DataStore,
    c: &authz::AuthContext,
    table: Uuid,
    filter: Condition,
) -> Vec<Vec<Uuid>> {
    let q = DataQuery {
        filter: Some(filter),
        limit: Some(100),
        ..DataQuery::default()
    };
    let mut out = Vec::new();
    for tuning in [
        QueryTuning::default(),
        QueryTuning {
            read_path: ReadPath::Body,
            ..QueryTuning::default()
        },
    ] {
        let (r, _) = store.run_query_debug(c, table, &q, &tuning).await.unwrap();
        let mut ids: Vec<Uuid> = r.items.unwrap().into_iter().map(|i| i.id).collect();
        ids.sort();
        out.push(ids);
    }
    out
}

struct Fixture {
    store: DataStore,
    c: authz::AuthContext,
    orders: Uuid,
    customers: Vec<Uuid>,
    orders_ids: Vec<Uuid>,
    ti: i32,
    tb: i64,
}

/// 顧客 4 件・注文 7 件（`customer` は v1 の record_ref・`account` は unique の v1 record_ref・
/// `customer_name` は `customer` 越しの lookup）。注文の値を v1 の文字列に戻し、形式を 1 にする。
async fn fixture(pool: &PgPool) -> Fixture {
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let customers_t = create_table(
        &store,
        &c,
        "customers",
        schema(vec![field("name", FieldType::Text)]),
    )
    .await;
    let mut account = v1_ref("account", customers_t.id);
    account.unique = true;
    let mut name = field("customer_name", FieldType::Lookup);
    name.lookup = Some(LookupDef {
        via_field: "customer".into(),
        target_field: "name".into(),
    });
    let orders = create_table(
        &store,
        &c,
        "orders",
        schema(vec![
            field("title", FieldType::Text),
            v1_ref("customer", customers_t.id),
            account,
            name,
        ]),
    )
    .await;
    let o = orders.schema.field("customer").unwrap();
    let a = orders.schema.field("account").unwrap();
    assert_eq!((o.id, a.id), (17, 18), "JSONB キーは f17 / f18");
    let mut customers = Vec::new();
    for i in 0..4 {
        let r = store
            .create_record(&c, customers_t.id, json!({"name": format!("c{i}")}), None)
            .await
            .unwrap();
        customers.push(r.id);
    }
    let mut orders_ids = Vec::new();
    for i in 0..7usize {
        // 書込は文字列でも受ける（v1 互換の入力）。account は先頭 4 件だけ（unique）。
        let mut d = json!({"title": format!("o{i}"), "customer": customers[i % 2].to_string()});
        if i < 4 {
            d["account"] = json!(customers[i].to_string());
        }
        let r = store.create_record(&c, orders.id, d, None).await.unwrap();
        // 書込は常に配列を書く。
        assert_eq!(r.data["customer"], json!([customers[i % 2].to_string()]));
        orders_ids.push(r.id);
    }
    let (ti, tb) = keys_of(pool, &tenant, orders.id).await;
    for (i, id) in orders_ids.iter().enumerate() {
        write_legacy(pool, ti, tb, *id, &[("f17", i % 3 == 0), ("f18", i == 1)]).await;
    }
    // 履歴にも v1 の形の差分を置く（create の差分を文字列にする）。
    sqlx::query(
        "UPDATE data_record_revision SET patch = jsonb_build_array(jsonb_build_object( \
           'field', 'customer', 'old', NULL, 'new', $2::text)) \
         WHERE record_id = $1 AND change_kind = 'create'",
    )
    .bind(orders_ids[0])
    .bind(customers[0].to_string().to_uppercase())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE data_table SET link_value_format = 1 WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        legacy_count(pool, ti, tb).await,
        11,
        "customer 7 件＋account 4 件"
    );
    Fixture {
        store,
        c,
        orders: orders.id,
        customers,
        orders_ids,
        ti,
        tb,
    }
}

/// 読取・書込・クエリ・lookup・履歴が、保存形に関わらず同じ値を見ること。
async fn assert_reads(pool: &PgPool, f: &Fixture) {
    let (store, c) = (&f.store, &f.c);
    for (i, id) in f.orders_ids.iter().enumerate() {
        let d = store.get_record(c, f.orders, *id, None).await.unwrap().data;
        assert_eq!(
            d["customer"],
            json!([f.customers[i % 2].to_string()]),
            "{i}"
        );
        assert_eq!(d["customer_name"], json!(format!("c{}", i % 2)), "{i}");
        if i < 4 {
            assert_eq!(d["account"], json!([f.customers[i].to_string()]));
        } else {
            assert!(d.get("account").is_none());
        }
    }
    // 等値・in・exists（索引経路と本体経路が同じ結果）。
    let mut c0: Vec<Uuid> = f.orders_ids.iter().step_by(2).copied().collect();
    c0.sort();
    for filter in [
        Condition::cmp(
            "customer",
            CondOp::Eq,
            Some(json!(f.customers[0].to_string())),
        ),
        Condition::cmp(
            "customer",
            CondOp::Eq,
            Some(json!(f.customers[0].to_string().to_uppercase())),
        ),
        Condition::cmp(
            "customer",
            CondOp::In,
            Some(json!([f.customers[0].to_string()])),
        ),
    ] {
        assert_eq!(
            query_ids(store, c, f.orders, filter).await,
            vec![c0.clone(), c0.clone()]
        );
    }
    let mut with_account: Vec<Uuid> = f.orders_ids[..4].to_vec();
    with_account.sort();
    assert_eq!(
        query_ids(
            store,
            c,
            f.orders,
            Condition::cmp("account", CondOp::Exists, None)
        )
        .await,
        vec![with_account.clone(), with_account]
    );
    // 履歴の差分も同じ形（移行前に記録された文字列も配列で返す）。
    let revs = store
        .list_revisions(c, f.orders, f.orders_ids[0], None, 10, None)
        .await
        .unwrap();
    let created = revs.iter().find(|r| r.change_kind == "create").unwrap();
    let p = created
        .patch
        .iter()
        .find(|p| p.field == "customer")
        .unwrap();
    assert_eq!(p.new, json!([f.customers[0].to_string()]));
    let _ = pool;
}

#[tokio::test]
async fn v1_record_ref_values_migrate_to_arrays_online_and_idempotently() {
    let Some(pool) = setup().await else { return };
    let _serial = SERIAL.lock().await;
    let f = fixture(&pool).await;
    let (ti, tb) = (f.ti, f.tb);
    let before_index = index_rows(&pool, ti, tb).await;
    assert_eq!(before_index.0.len(), 11, "customer 7 本＋account 4 本");

    // ① 移行前: 文字列のままでも読取・クエリは配列として見る。
    assert_reads(&pool, &f).await;

    // ② 移行前の行を他の列だけ更新しても、書き戻しは配列（差分は title だけ）。
    let o6 = f.orders_ids[6];
    let cur = f.store.get_record(&f.c, f.orders, o6, None).await.unwrap();
    let up = f
        .store
        .update_record(&f.c, f.orders, o6, json!({"title": "o6b"}), cur.rev, None)
        .await
        .unwrap();
    assert_eq!(up.rev, cur.rev + 1);
    assert!(stored(&pool, ti, tb, o6).await["f17"].is_array());
    let last = f
        .store
        .list_revisions(&f.c, f.orders, o6, None, 1, None)
        .await
        .unwrap();
    assert_eq!(
        last[0]
            .patch
            .iter()
            .map(|p| p.field.as_str())
            .collect::<Vec<_>>(),
        vec!["title"],
        "保存形の違いだけでは差分を作らない"
    );
    assert_eq!(legacy_count(&pool, ti, tb).await, 10);
    let before_revs = revs(&pool, ti, tb).await;

    // ③ チャンク 2 件ずつ（record_int 順。採番は作成順とは限らない）進める。1 チャンク目の後に、
    //    処理済みの行へ旧バイナリが文字列を書き戻す。
    let by_int: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM data_record WHERE tenant_int = $1 AND table_int = $2 ORDER BY record_int",
    )
    .bind(ti)
    .bind(tb)
    .fetch_all(&pool)
    .await
    .unwrap();
    let first = f
        .store
        .link_value_migration_step(&f.c.tenant_id, f.orders, 2)
        .await
        .unwrap();
    let legacy_in_first = by_int[..2].iter().filter(|id| **id != o6).count() as u64;
    assert_eq!(
        (first.scanned, first.converted, first.done),
        (2, legacy_in_first, false)
    );
    assert_eq!(format_of(&pool, ti, tb).await, 1);
    // 途中でも読取は変わらない。
    assert_reads(&pool, &f).await;
    write_legacy(&pool, ti, tb, by_int[0], &[("f17", true)]).await;
    let mut steps = 1;
    loop {
        let s = f
            .store
            .link_value_migration_step(&f.c.tenant_id, f.orders, 2)
            .await
            .unwrap();
        steps += 1;
        assert!(steps < 20, "終わらない");
        if s.done {
            break;
        }
    }
    let status = f
        .store
        .link_value_migration_status(&f.c.tenant_id, f.orders)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.status, "done");
    assert_eq!(status.passes, 1, "書き戻された行を拾うためにもう一周した");
    assert_eq!(
        status.rows_converted,
        6 + 1,
        "o0〜o5（o6 は更新で配列になった）＋書き戻された 1 件"
    );
    assert_eq!(status.parity_mismatches, Some(0));
    assert!(status.parity_checked_at.is_some() && status.finished_at.is_some());

    // ④ 移行後: 文字列は残らず、形式は 2。値は正準形（小文字）の要素 1 件の配列。
    assert_eq!(legacy_count(&pool, ti, tb).await, 0);
    assert_eq!(format_of(&pool, ti, tb).await, 2);
    assert_eq!(
        stored(&pool, ti, tb, f.orders_ids[0]).await["f17"],
        json!([f.customers[0].to_string()])
    );
    assert_eq!(
        stored(&pool, ti, tb, f.orders_ids[1]).await["f18"],
        json!([f.customers[1].to_string()])
    );
    // 索引行・版・更新時刻は動かない（利用者の変更ではない）。
    assert_eq!(index_rows(&pool, ti, tb).await, before_index);
    assert_eq!(revs(&pool, ti, tb).await, before_revs);
    let report = f
        .store
        .verify_index_parity(&f.c.tenant_id, f.orders)
        .await
        .unwrap();
    assert!(report.is_consistent(), "{report:?}");
    assert_reads(&pool, &f).await;

    // ⑤ 冪等: もう一度流しても何も変わらない。
    let again = f
        .store
        .link_value_migration_step(&f.c.tenant_id, f.orders, 2)
        .await
        .unwrap();
    assert!(again.done && again.converted == 0);
    assert_eq!(
        f.store
            .migrate_table_link_values(&f.c.tenant_id, f.orders)
            .await
            .unwrap(),
        0
    );
    assert_eq!(revs(&pool, ti, tb).await, before_revs);

    // ⑥ unique は移行の前後で同じキー（文字列の入力でも配列でも重複は 409）。
    for dup in [
        json!(f.customers[0].to_string().to_uppercase()),
        json!([f.customers[0].to_string()]),
    ] {
        let r = f
            .store
            .create_record(
                &f.c,
                f.orders,
                json!({"title": "dup", "account": dup}),
                None,
            )
            .await;
        assert!(matches!(r, Err(DataError::Conflict(_))), "{r:?}");
    }
}

/// 書込の形: 文字列・要素 1 件の配列を受けて配列で書く。2 件以上・不正は 422。空配列は値なし。
#[tokio::test]
async fn v1_record_ref_writes_emit_arrays() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let to = create_table(&store, &c, "to", schema(vec![field("n", FieldType::Text)])).await;
    let a = store
        .create_record(&c, to.id, json!({"n": "a"}), None)
        .await
        .unwrap()
        .id;
    let b = store
        .create_record(&c, to.id, json!({"n": "b"}), None)
        .await
        .unwrap()
        .id;
    let mut req = v1_ref("req", to.id);
    req.required = true;
    let t = create_table(&store, &c, "from", schema(vec![v1_ref("opt", to.id), req])).await;
    let r = store
        .create_record(
            &c,
            t.id,
            json!({"req": [a.to_string().to_uppercase()], "opt": []}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.data["req"], json!([a.to_string()]));
    assert!(r.data.get("opt").is_none(), "空配列は値なし（null と同じ）");
    for bad in [
        json!({"req": [a.to_string(), b.to_string()]}),
        json!({"req": []}),
        json!({"req": 1}),
        json!({"req": ["not-a-uuid"]}),
    ] {
        let r = store.create_record(&c, t.id, bad.clone(), None).await;
        assert!(matches!(r, Err(DataError::Invalid(_))), "{bad}: {r:?}");
    }
    // 新しく作ったテーブルは最初から配列の形式（移行の対象にならない）。
    let (ti, tb) = keys_of(&pool, &tenant, t.id).await;
    assert_eq!(format_of(&pool, ti, tb).await, 2);
}

/// 常駐ワーカーの経路: スイープが未移行テーブルにジョブを積み、索引ジョブキューの消費で完了する。
#[tokio::test]
async fn link_value_migration_runs_from_the_index_job_queue() {
    let Some(pool) = setup().await else { return };
    let _serial = SERIAL.lock().await;
    let f = fixture(&pool).await;
    let (ti, tb) = (f.ti, f.tb);
    assert!(f.store.link_value_migration_remaining().await.unwrap() >= 1);
    f.store.enqueue_index_backfills_if_needed().await.unwrap();
    assert!(
        f.store
            .link_value_migration_status(&f.c.tenant_id, f.orders)
            .await
            .unwrap()
            .is_some(),
        "スイープが進捗行を作ってジョブを積む"
    );
    // 他のテストのジョブも同じキューに積まれるので、このテーブルが終わるまで消費する。
    for _ in 0..500 {
        if format_of(&pool, ti, tb).await == 2 {
            break;
        }
        if !f.store.consume_index_job_once().await.unwrap_or(true) {
            f.store.enqueue_index_backfills_if_needed().await.unwrap();
        }
    }
    assert_eq!(format_of(&pool, ti, tb).await, 2);
    assert_eq!(legacy_count(&pool, ti, tb).await, 0);
    assert_reads(&pool, &f).await;
}
