//! pivot 索引テーブルの結合テスト（Phase 15 Task 15.2 受け入れ条件・書込経路と DB 制約）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）。authz は全許可モック。
//!
//! - 索引の物理本数がテーブル数・テナント数に依存しない（テーブルを大量生成して確認）
//! - 書込経路の差分更新（変更の無いフィールドの索引行は書き換わらない）・FSM 遷移も同じ
//! - 多値要素数の上限超過は 422（Invalid）
//! - `data_record` とリンクの順引き・逆引きがそれぞれ 1 パーティションへ枝刈りされる（EXPLAIN）
//! - `MAX_TEXT_LEN` 上限の値が indexed / unique / searchable 列でも書ける
//! - 部分一致 GIN がスコープ列を含み、他テナントのデータ量に影響されない
//! - 拡張が無い環境ではマイグレーションが名指しで失敗する
//! - リンクのカーディナリティが DB 制約で強制され、`single_valued` の偽装は FK で落ちる
//! - `FieldId` 32,767 超の採番は 422・NumericSpec の正規化が本体と索引で一致する

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_possible_wrap,
    clippy::similar_names
)]

mod common;

use common::*;
use data::{
    DataError, ExcessScale, FieldType, FsmBody, FsmTransition, NumericSpec, PolicyExpr,
    RoundingMode, MAX_INDEX_KEY_BYTES,
};
use serde_json::{json, Value};
use uuid::Uuid;

/// 索引テーブル（＋そのパーティション）に張られた索引の総数。
async fn index_table_index_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_index ix JOIN pg_class c ON c.oid = ix.indrelid \
         WHERE c.relname ~ '^(data_index_text|data_index_num|data_link|data_unique|data_index_search)(_p[0-9]+)?$'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 受け入れ条件: 索引テーブルの本数がテーブル数・テナント数に依存しない。
#[tokio::test]
async fn index_count_does_not_grow_with_tables_or_tenants() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let before = index_table_index_count(&pool).await;
    let partitions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_inherits WHERE inhparent = 'data_record'::regclass",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    // 1 パーティションあたりの索引は定数: PK 5（各索引テーブル）＋値索引 2（text/num）＋GIN 1
    // ＋リンクの部分一意 1 = 9（data_record の PK を加えると §3.3 の「10 本」）。
    let per_partition: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_index ix JOIN pg_class c ON c.oid = ix.indrelid \
         WHERE c.relname IN ('data_index_text_p0', 'data_index_num_p0', 'data_link_p0', \
                             'data_unique_p0', 'data_index_search_p0')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(per_partition, 9);

    // 3 テナント × 12 テーブル、各テーブルに索引宣言 5 列＋レコード。
    let mut written_rows = 0_i64;
    for _ in 0..3 {
        let tenant = unique_tenant();
        let c = ctx(&tenant, "alice");
        for t in 0..12 {
            let table = create_table(&store, &c, &format!("bulk-{t}"), rich_schema()).await;
            store
                .create_record(
                    &c,
                    table.id,
                    json!({"title": "x", "amount": t, "tags": ["a"], "status": "draft",
                           "due": "2026-10-01"}),
                    None,
                )
                .await
                .unwrap();
            let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM data_index_text WHERE tenant_int = $1 AND table_int = $2",
            )
            .bind(ti)
            .bind(tb)
            .fetch_one(&pool)
            .await
            .unwrap();
            written_rows += n;
        }
    }
    assert!(
        written_rows >= 36 * 7,
        "索引行は書かれている: {written_rows}"
    );
    let after = index_table_index_count(&pool).await;
    assert_eq!(
        before, after,
        "テーブル・テナントを増やしても索引テーブルの索引本数は変わらない"
    );
    // 親（パーティション表）の索引も pg_index に載るので「分割数 + 1」倍。
    assert_eq!(after, per_partition * (partitions + 1));
}

/// (field_id, index_table) → 行の xmin（書換えの検出）。
async fn index_xmins(
    pool: &sqlx::PgPool,
    ti: i32,
    tb: i64,
    rec: i64,
) -> Vec<(String, i16, String)> {
    sqlx::query_as(
        "SELECT 'text', field_id, xmin::text FROM data_index_text \
           WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3 \
         UNION ALL SELECT 'num', field_id, xmin::text FROM data_index_num \
           WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3 \
         UNION ALL SELECT 'unique', field_id, xmin::text FROM data_unique \
           WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3 \
         UNION ALL SELECT 'search', field_id, xmin::text FROM data_index_search \
           WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3 \
         ORDER BY 1, 2, 3",
    )
    .bind(ti)
    .bind(tb)
    .bind(rec)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// 受け入れ条件: 書込経路で変更の無いフィールドの索引行が更新されない（差分更新）。
#[tokio::test]
async fn write_path_updates_only_changed_fields() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = create_table(&store, &c, "diff", rich_schema()).await;
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let rec = store
        .create_record(
            &c,
            table.id,
            json!({"title": "経費", "amount": 100, "tags": ["a", "b"], "status": "draft",
                   "due": "2026-10-01", "memo": "未索引"}),
            None,
        )
        .await
        .unwrap();
    let ri = record_int_of(&pool, ti, tb, rec.id).await;
    let id = |key: &str| i16::try_from(table.schema.field(key).unwrap().id).unwrap();

    // create: 擬似列 3 行＋title/tags(2)/status/due の text・amount の num/unique・title の search。
    let rows = index_xmins(&pool, ti, tb, ri).await;
    let count = |kind: &str| rows.iter().filter(|(k, _, _)| k == kind).count();
    assert_eq!(count("text"), 3 + 1 + 2 + 1 + 1, "{rows:?}");
    assert_eq!(count("num"), 1);
    assert_eq!(count("unique"), 1);
    assert_eq!(count("search"), 1);
    assert!(
        !rows.iter().any(|(_, f, _)| *f == id("memo")),
        "未索引の列は索引行を持たない"
    );

    // amount だけを変える → amount の num/unique と updated_at（擬似列 2）以外の行は同じ xmin。
    let updated = store
        .update_record(&c, table.id, rec.id, json!({"amount": 250}), 1, None)
        .await
        .unwrap();
    let after = index_xmins(&pool, ti, tb, ri).await;
    for (kind, f, xmin) in &rows {
        let now = after
            .iter()
            .find(|(k, af, _)| k == kind && af == f)
            .map(|(_, _, x)| x.clone());
        let changed = now.as_deref() != Some(xmin.as_str());
        let expected = *f == id("amount") || (kind == "text" && *f == 2);
        assert_eq!(
            changed, expected,
            "{kind} field_id={f}: 変更の有無が差分どおり（before={xmin} after={now:?}）"
        );
    }
    let amount: String = sqlx::query_scalar(
        "SELECT val::text FROM data_index_num \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3 AND field_id = $4",
    )
    .bind(ti)
    .bind(tb)
    .bind(ri)
    .bind(id("amount"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(amount, "250");

    // 未索引の列だけの更新は updated_at の行だけが動く。
    let before_memo = index_xmins(&pool, ti, tb, ri).await;
    let updated = store
        .update_record(
            &c,
            table.id,
            rec.id,
            json!({"memo": "変更"}),
            updated.rev,
            None,
        )
        .await
        .unwrap();
    let after_memo = index_xmins(&pool, ti, tb, ri).await;
    let moved: Vec<_> = before_memo
        .iter()
        .filter(|b| !after_memo.contains(b))
        .collect();
    assert_eq!(moved.len(), 1, "updated_at の 1 行だけ: {moved:?}");
    assert_eq!(moved[0].1, 2);

    // FSM 遷移（唯一の status 変更経路）も status と updated_at だけを書き換える。
    let fsm = FsmBody {
        states: vec!["draft".into(), "submitted".into(), "approved".into()],
        transitions: vec![FsmTransition {
            from: "draft".into(),
            to: "submitted".into(),
            actor: PolicyExpr::Public,
        }],
    };
    let before_tr = index_xmins(&pool, ti, tb, ri).await;
    store
        .transition_record(
            &c,
            table.id,
            rec.id,
            "submitted",
            updated.rev,
            &fsm,
            "status",
            None,
        )
        .await
        .unwrap();
    let after_tr = index_xmins(&pool, ti, tb, ri).await;
    let moved: Vec<i16> = before_tr
        .iter()
        .filter(|b| !after_tr.contains(b))
        .map(|b| b.1)
        .collect();
    let mut moved_sorted = moved.clone();
    moved_sorted.sort_unstable();
    assert_eq!(
        moved_sorted,
        vec![2, id("status")],
        "status と updated_at だけ"
    );
    let status: String = sqlx::query_scalar(
        "SELECT val FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3 AND field_id = $4",
    )
    .bind(ti)
    .bind(tb)
    .bind(ri)
    .bind(id("status"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "submitted");

    // 新索引の unique も効く（同値は 409）。削除で索引行が全て消える。
    let dup = store
        .create_record(&c, table.id, json!({"title": "y", "amount": 250}), None)
        .await;
    assert!(matches!(dup, Err(DataError::Conflict(_))), "{dup:?}");
    let cur = store.get_record(&c, table.id, rec.id, None).await.unwrap();
    store
        .delete_record(&c, table.id, rec.id, cur.rev, None)
        .await
        .unwrap();
    assert!(index_xmins(&pool, ti, tb, ri).await.is_empty());
    let links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_index_text WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(links, 0);
    // 削除後は同じ amount を再利用できる（unique 行も消えている）。
    store
        .create_record(&c, table.id, json!({"title": "z", "amount": 250}), None)
        .await
        .unwrap();
    let report = store.verify_index_parity(&tenant, table.id).await.unwrap();
    assert!(report.is_consistent(), "{report:?}");
}

/// 受け入れ条件: 多値要素数の上限超過が 422（Invalid）で拒否される（設定可能）。
#[tokio::test]
async fn multi_value_limit_is_enforced() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone()).with_max_multi_values(2);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = create_table(&store, &c, "multi", rich_schema()).await;
    let r = store
        .create_record(
            &c,
            table.id,
            json!({"title": "x", "tags": ["a", "b", "c"]}),
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    let ok = store
        .create_record(
            &c,
            table.id,
            json!({"title": "x", "tags": ["a", "b"]}),
            None,
        )
        .await
        .unwrap();
    let r = store
        .update_record(
            &c,
            table.id,
            ok.id,
            json!({"tags": ["a", "b", "c"]}),
            1,
            None,
        )
        .await;
    assert!(
        matches!(r, Err(DataError::Invalid(_))),
        "update も同じ上限: {r:?}"
    );
    // 既定（50）でも同様。
    let default_store = store_with(pool);
    let tags: Vec<String> = (0..51).map(|i| format!("o{i}")).collect();
    let mut many = field("many", FieldType::MultiSelect);
    many.options = tags.clone();
    many.indexed = true;
    let t2 = create_table(&default_store, &c, "multi-default", schema(vec![many])).await;
    let r = default_store
        .create_record(&c, t2.id, json!({ "many": tags }), None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
}

/// ランダムな英数字（圧縮で縮まない＝btree キー上限を本当に超える値）。
fn random_text(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes);
    while s.len() < bytes {
        s.push_str(&Uuid::new_v4().simple().to_string());
    }
    s.truncate(bytes);
    s
}

/// 受け入れ条件: MAX_TEXT_LEN（10,000B）の値が indexed / unique / searchable 列でも書ける。
#[tokio::test]
async fn max_text_len_values_are_writable_in_indexed_columns() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let mut body = field("body", FieldType::Text);
    body.indexed = true;
    body.searchable = true;
    let mut code = field("code", FieldType::Text);
    code.unique = true;
    let table = create_table(&store, &c, "long", schema(vec![body, code])).await;
    let long = random_text(10_000);
    let rec = store
        .create_record(&c, table.id, json!({"body": long, "code": long}), None)
        .await
        .expect("btree キー上限で落ちない");
    // 同じプレフィクスで末尾だけ違う値は別値として書ける（unique はハッシュ付き正規化キー）。
    let mut other = long[..9_999].to_string();
    other.push(if long.ends_with('0') { '1' } else { '0' });
    store
        .create_record(&c, table.id, json!({"body": other, "code": other}), None)
        .await
        .expect("別値");
    // 完全に同じ値は unique 違反（旧方式の md5 一意索引と新方式の data_unique の両方が見る）。
    let dup = store
        .create_record(&c, table.id, json!({"code": long}), None)
        .await;
    assert!(matches!(dup, Err(DataError::Conflict(_))), "{dup:?}");

    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let ri = record_int_of(&pool, ti, tb, rec.id).await;
    let (len, truncated): (i32, bool) = sqlx::query_as(
        "SELECT octet_length(val), truncated FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3 AND field_id = $4",
    )
    .bind(ti)
    .bind(tb)
    .bind(ri)
    .bind(i16::try_from(table.schema.field("body").unwrap().id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (usize::try_from(len).unwrap(), truncated),
        (MAX_INDEX_KEY_BYTES, true)
    );
    // 部分一致索引は全文を持つ。
    let search_len: i32 = sqlx::query_scalar(
        "SELECT octet_length(val) FROM data_index_search \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(ri)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(search_len, 10_000);
    // 旧経路（二重書込期間の読取）でも等値フィルタが正しく効く。
    let page = store
        .list_records(
            &c,
            table.id,
            &data::ListRecordsOptions {
                filter: Some(data::RecordFilter {
                    field: "body".into(),
                    value: json!(long),
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, rec.id);
    let report = store.verify_index_parity(&tenant, table.id).await.unwrap();
    assert!(report.is_consistent(), "{report:?}");
}

/// EXPLAIN (FORMAT JSON) の結果。
async fn explain(pool: &sqlx::PgPool, sql: &str) -> Value {
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query_scalar(&format!("EXPLAIN (FORMAT JSON) {sql}"))
        .fetch_one(&mut *conn)
        .await
        .unwrap()
}

/// 受け入れ条件: data_record の枝刈り・リンクの順引き/逆引きがそれぞれ 1 パーティション。
/// あわせてリンクのカーディナリティが DB 制約で強制され、`single_valued` の偽装は FK で落ちる。
#[tokio::test]
async fn partition_pruning_and_link_constraints() {
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
    let mut cust_ref = field("customer", FieldType::RecordRef);
    cust_ref.ref_table = Some(customer.id);
    let order = create_table(&store, &c, "order", schema(vec![cust_ref])).await;
    let cust = store
        .create_record(&c, customer.id, json!({"name": "A 社"}), None)
        .await
        .unwrap();
    let cust2 = store
        .create_record(&c, customer.id, json!({"name": "B 社"}), None)
        .await
        .unwrap();
    let ord = store
        .create_record(&c, order.id, json!({"customer": cust.id.to_string()}), None)
        .await
        .unwrap();
    let (ti, cust_tb) = keys_of(&pool, &tenant, customer.id).await;
    let (_, ord_tb) = keys_of(&pool, &tenant, order.id).await;
    let cust_ri = record_int_of(&pool, ti, cust_tb, cust.id).await;
    let cust2_ri = record_int_of(&pool, ti, cust_tb, cust2.id).await;
    let ord_ri = record_int_of(&pool, ti, ord_tb, ord.id).await;
    let fid = i16::try_from(order.schema.field("customer").unwrap().id).unwrap();

    // 書込経路が順方向行を書いている（single_valued は宣言から写す）。
    let fwd: (i64, i64, bool) = sqlx::query_as(
        "SELECT dst_table_int, dst_record, single_valued FROM data_link \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 AND src_record = $4",
    )
    .bind(ti)
    .bind(ord_tb)
    .bind(fid)
    .bind(ord_ri)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fwd, (cust_tb, cust_ri, true));

    // ① data_record の主キー引き・② リンクの順引きは 1 パーティション。
    let p = explain(
        &pool,
        &format!(
            "SELECT data FROM data_record WHERE tenant_int = {ti} AND table_int = {ord_tb} \
             AND id = '{}'",
            ord.id
        ),
    )
    .await;
    assert_eq!(relations_in_plan(&p, "data_record").len(), 1, "{p}");
    // v1 の読取経路の形（tenant_id/table_id＋内部キー）も 1 パーティション。
    let p = explain(
        &pool,
        &format!(
            "SELECT r.id FROM data_record r WHERE r.tenant_id = '{tenant}' \
             AND r.table_id = '{}' AND r.tenant_int = {ti} AND r.table_int = {ord_tb} \
             ORDER BY r.updated_at DESC, r.id DESC LIMIT 10",
            order.id
        ),
    )
    .await;
    assert_eq!(relations_in_plan(&p, "data_record").len(), 1, "{p}");
    let p = explain(
        &pool,
        &format!(
            "SELECT dst_record FROM data_link WHERE tenant_int = {ti} AND table_int = {ord_tb} \
             AND field_id = {fid} AND src_record = {ord_ri}"
        ),
    )
    .await;
    assert_eq!(relations_in_plan(&p, "data_link").len(), 1, "{p}");

    // ③ 逆引き: 対称列（customer 側の f40・Task 15.4 で宣言されるもの）を宣言し、逆方向行を書く。
    //    逆方向行は customer 側の table_int に載るので、逆引きも 1 パーティション。
    sqlx::query(
        "INSERT INTO data_link_field (tenant_int, table_int, field_id, single_valued) \
         VALUES ($1, $2, 40, true)",
    )
    .bind(ti)
    .bind(cust_tb)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO data_link (tenant_int, table_int, field_id, src_record, ord, \
           dst_table_int, dst_record, is_reverse, single_valued) \
         VALUES ($1, $2, 40, $3, 0, $4, $5, true, true)",
    )
    .bind(ti)
    .bind(cust_tb)
    .bind(cust_ri)
    .bind(ord_tb)
    .bind(ord_ri)
    .execute(&pool)
    .await
    .unwrap();
    let p = explain(
        &pool,
        &format!(
            "SELECT dst_record FROM data_link WHERE tenant_int = {ti} AND table_int = {cust_tb} \
             AND field_id = 40 AND src_record = {cust_ri}"
        ),
    )
    .await;
    assert_eq!(relations_in_plan(&p, "data_link").len(), 1, "{p}");
    let back: i64 = sqlx::query_scalar(
        "SELECT dst_record FROM data_link WHERE tenant_int = $1 AND table_int = $2 \
         AND field_id = 40 AND src_record = $3",
    )
    .bind(ti)
    .bind(cust_tb)
    .bind(cust_ri)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(back, ord_ri, "逆引きで参照元が引ける");

    // ④ カーディナリティの DB 強制（アプリ検証を迂回した生 SQL でも書けない）。
    let insert_link =
        |table_int: i64, field: i16, src: i64, ord: i16, dst_tb: i64, dst: i64, sv: bool| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO data_link (tenant_int, table_int, field_id, src_record, ord, \
                   dst_table_int, dst_record, is_reverse, single_valued) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, false, $8)",
                )
                .bind(ti)
                .bind(table_int)
                .bind(field)
                .bind(src)
                .bind(ord)
                .bind(dst_tb)
                .bind(dst)
                .bind(sv)
                .execute(&pool)
                .await
            }
        };
    let code = |r: Result<sqlx::postgres::PgQueryResult, sqlx::Error>| match r {
        Err(sqlx::Error::Database(db)) => db.code().map(|c| c.to_string()),
        other => panic!("制約違反になるはず: {other:?}"),
    };
    // 単一値の列（v1 の record_ref）に 2 本目のエッジ → 部分一意違反（23505）。
    let r = insert_link(ord_tb, fid, ord_ri, 1, cust_tb, cust2_ri, true).await;
    assert_eq!(code(r).as_deref(), Some("23505"));
    // single_valued = false を明示して部分一意を迂回しようとしても、宣言が true なので FK 違反（23503）。
    let r = insert_link(ord_tb, fid, ord_ri, 1, cust_tb, cust2_ri, false).await;
    assert_eq!(code(r).as_deref(), Some("23503"));
    // 宣言の無い列へは書けない（FK 違反）。
    let r = insert_link(ord_tb, 999, ord_ri, 0, cust_tb, cust2_ri, false).await;
    assert_eq!(code(r).as_deref(), Some("23503"));

    // OneToOne（order → customer・順方向も逆方向も single_valued）: 2 つの order が同じ customer を
    // 指すと、逆方向行（customer 側）の部分一意で弾かれる。
    for (tb, f, sv) in [(ord_tb, 41_i16, true), (cust_tb, 42_i16, true)] {
        declare(&pool, ti, tb, f, sv).await;
    }
    let ord2 = store
        .create_record(&c, order.id, json!({}), None)
        .await
        .unwrap();
    let ord2_ri = record_int_of(&pool, ti, ord_tb, ord2.id).await;
    insert_link(ord_tb, 41, ord_ri, 0, cust_tb, cust_ri, true)
        .await
        .unwrap();
    insert_link(cust_tb, 42, cust_ri, 0, ord_tb, ord_ri, true)
        .await
        .unwrap();
    // 別の order が同じ customer を指す: 順方向は通っても逆方向（customer 側の一意）で弾かれる。
    insert_link(ord_tb, 41, ord2_ri, 0, cust_tb, cust_ri, true)
        .await
        .unwrap();
    let r = insert_link(cust_tb, 42, cust_ri, 1, ord_tb, ord2_ri, true).await;
    assert_eq!(
        code(r).as_deref(),
        Some("23505"),
        "one_to_one: dst 側の一意"
    );
    let r = insert_link(cust_tb, 42, cust_ri, 1, ord_tb, ord2_ri, false).await;
    assert_eq!(
        code(r).as_deref(),
        Some("23503"),
        "one_to_one: フラグ偽装は FK 違反"
    );
    // 同じ order が 2 つ目の customer を指す: 順方向（order 側の一意）で弾かれる。
    let r = insert_link(ord_tb, 41, ord_ri, 1, cust_tb, cust2_ri, true).await;
    assert_eq!(
        code(r).as_deref(),
        Some("23505"),
        "one_to_one: src 側の一意"
    );

    // OneToMany（src = one 側 = customer。customer は複数の order を持てるが、各 order は 1 つの
    // customer からしか指されない）: 順方向（customer 側）は複数値・逆方向（order 側）は単一値。
    declare(&pool, ti, cust_tb, 43, false).await;
    declare(&pool, ti, ord_tb, 44, true).await;
    insert_link(cust_tb, 43, cust_ri, 0, ord_tb, ord_ri, false)
        .await
        .unwrap();
    insert_link(cust_tb, 43, cust_ri, 1, ord_tb, ord2_ri, false)
        .await
        .expect("one 側は複数を持てる");
    insert_link(ord_tb, 44, ord_ri, 0, cust_tb, cust_ri, true)
        .await
        .unwrap();
    insert_link(ord_tb, 44, ord2_ri, 0, cust_tb, cust_ri, true)
        .await
        .unwrap();
    // 2 つ目の customer が同じ order を持とうとする: 順方向は通っても逆方向（order 側の一意）で弾かれる。
    insert_link(cust_tb, 43, cust2_ri, 0, ord_tb, ord_ri, false)
        .await
        .unwrap();
    let r = insert_link(ord_tb, 44, ord_ri, 1, cust_tb, cust2_ri, true).await;
    assert_eq!(
        code(r).as_deref(),
        Some("23505"),
        "one_to_many: dst 側の一意"
    );
    let r = insert_link(ord_tb, 44, ord_ri, 1, cust_tb, cust2_ri, false).await;
    assert_eq!(
        code(r).as_deref(),
        Some("23503"),
        "one_to_many: フラグ偽装は FK 違反"
    );
    // 宣言 false の列に true を名乗っても FK 違反（宣言どおりの値しか書けない）。
    let r = insert_link(cust_tb, 43, cust2_ri, 1, ord_tb, ord2_ri, true).await;
    assert_eq!(code(r).as_deref(), Some("23503"));
}

async fn declare(pool: &sqlx::PgPool, ti: i32, tb: i64, field: i16, single: bool) {
    sqlx::query(
        "INSERT INTO data_link_field (tenant_int, table_int, field_id, single_valued) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(ti)
    .bind(tb)
    .bind(field)
    .bind(single)
    .execute(pool)
    .await
    .unwrap();
}

/// GIN（部分一致索引）の Bitmap Index Scan の共有バッファ読み（hit + read）と、ヒープで返った行数。
fn gin_scan_cost(plan: &Value) -> (Option<i64>, i64) {
    fn walk(v: &Value, out: &mut (Option<i64>, i64)) {
        match v {
            Value::Object(m) => {
                let node = m.get("Node Type").and_then(Value::as_str);
                let index = m.get("Index Name").and_then(Value::as_str).unwrap_or("");
                if node == Some("Bitmap Index Scan") && index.ends_with("_val_idx") {
                    let blocks = m
                        .get("Shared Hit Blocks")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        + m.get("Shared Read Blocks")
                            .and_then(Value::as_i64)
                            .unwrap_or(0);
                    out.0 = Some(out.0.unwrap_or(0) + blocks);
                }
                if node == Some("Bitmap Heap Scan") {
                    out.1 += m.get("Actual Rows").and_then(Value::as_i64).unwrap_or(0);
                }
                for x in m.values() {
                    walk(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = (None, 0);
    walk(plan, &mut out);
    out
}

/// 受け入れ条件: 部分一致検索が他テナントのデータ量に影響されない（GIN がスコープ列を含む）。
#[tokio::test]
async fn substring_search_is_scoped_inside_the_gin() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let mut title = field("title", FieldType::Text);
    title.searchable = true;
    let table = create_table(&store, &c, "search", schema(vec![title])).await;
    for i in 0..20 {
        store
            .create_record(
                &c,
                table.id,
                json!({ "title": format!("見積 needle {i}") }),
                None,
            )
            .await
            .unwrap();
    }
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let fid = i16::try_from(table.schema.field("title").unwrap().id).unwrap();
    // 自テナントの索引行を厚くする（GIN が選ばれる規模にする・語を含まない行）。索引の振る舞いだけを
    // 見るので本体は作らない（このテーブルの parity は検証しない）。
    let own_base: i64 = 9_000_000_000_000;
    sqlx::query(
        "INSERT INTO data_index_search (tenant_int, table_int, field_id, record_int, ord, val) \
         SELECT $1, $2, $3, $4 + g, 0, 'hay ' || md5(g::text) FROM generate_series(1, 50000) g",
    )
    .bind(ti)
    .bind(tb)
    .bind(fid)
    .bind(own_base)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE data_index_search")
        .execute(&pool)
        .await
        .unwrap();
    let query = format!(
        "SELECT record_int FROM data_index_search WHERE tenant_int = {ti} AND table_int = {tb} \
         AND field_id = {fid} AND val LIKE '%needle%'"
    );
    let measure = || {
        let pool = pool.clone();
        let query = query.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            for set in ["SET enable_seqscan = off", "SET enable_indexscan = off"] {
                sqlx::query(set).execute(&mut *conn).await.unwrap();
            }
            let plan: Value =
                sqlx::query_scalar(&format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {query}"))
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap();
            plan
        }
    };
    let before = measure().await;
    assert_eq!(
        relations_in_plan(&before, "data_index_search").len(),
        1,
        "{before}"
    );

    // 同じパーティション（同じ table_int）に**別テナント**の行を大量に積む（同じ語を含む）。
    // 本来 table_int はテナントを跨がないが、ここでは「同じ GIN の中に他テナントの posting がある」
    // 最悪の同居を作るために同じ table_int を使う。
    let noise = |tenant_int: i32, n: i64| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO data_index_search (tenant_int, table_int, field_id, record_int, ord, val) \
                 SELECT $1, $2, $3, g, 0, 'noise needle ' || g FROM generate_series(1, $4) g",
            )
            .bind(tenant_int)
            .bind(tb)
            .bind(fid)
            .bind(n)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("ANALYZE data_index_search")
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    let (noise_a, noise_b) = (i32::MAX - 7, i32::MAX - 8);
    noise(noise_a, 50_000).await;
    let after1 = measure().await;
    noise(noise_b, 150_000).await;
    let after2 = measure().await;
    // 対照: スコープ（tenant_int）を条件に持たない検索は他テナントの posting list を丸ごと読む。
    let unscoped: Value = {
        let mut conn = pool.acquire().await.unwrap();
        for set in ["SET enable_seqscan = off", "SET enable_indexscan = off"] {
            sqlx::query(set).execute(&mut *conn).await.unwrap();
        }
        sqlx::query_scalar(&format!(
            "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT record_int FROM data_index_search \
             WHERE table_int = {tb} AND field_id = {fid} AND val LIKE '%needle%'"
        ))
        .fetch_one(&mut *conn)
        .await
        .unwrap()
    };
    for (cond, t) in [
        ("tenant_int = $1", noise_a),
        ("tenant_int = $1", noise_b),
        ("record_int > $2 AND tenant_int = $1", ti),
    ] {
        sqlx::query(&format!(
            "DELETE FROM data_index_search WHERE {cond} AND table_int = $3"
        ))
        .bind(t)
        .bind(own_base)
        .bind(tb)
        .execute(&pool)
        .await
        .unwrap();
    }
    let cost = |plan: &Value| {
        let (blocks, rows) = gin_scan_cost(plan);
        (
            blocks.unwrap_or_else(|| {
                panic!("部分一致は trigram GIN（スコープ列つき）で引く: {plan}")
            }),
            rows,
        )
    };
    let (b0, r0) = cost(&before);
    let (b1, r1) = cost(&after1);
    let (b2, r2) = cost(&after2);
    let (bu, ru) = cost(&unscoped);
    eprintln!("GIN blocks: 0 件={b0} / +5 万={b1} / +20 万={b2}（スコープなし対照={bu}・{ru} 行）");
    assert_eq!((r0, r1, r2), (20, 20, 20), "他テナントの行は返らない");
    assert!(ru >= 200_000, "対照はノイズを読む: {ru}");
    // ノイズを 5 万 → 20 万に増やしても GIN で読むページは増えない（posting tree の深さ分の
    // 定数だけ）。スコープ列を GIN に含めない対照はノイズに比例して読む。
    assert!(b1 <= b0 + 32, "GIN のスコープ: 0 件={b0} +5 万={b1}");
    assert!(b2 <= b1 + 8, "ノイズ量に比例しない: +5 万={b1} +20 万={b2}");
    assert!(
        bu > b2 * 3,
        "対照（スコープなし）={bu} は桁で多い（スコープあり={b2}）"
    );
}

/// 受け入れ条件: 拡張が無い環境で初回マイグレーションが明示的に失敗する。
#[tokio::test]
async fn missing_extensions_fail_the_migration_explicitly() {
    let Some(pool) = setup().await else { return };
    // マイグレーションは拡張を作る前に事前確認関数を呼ぶ（順序を固定）。
    let sql = include_str!("../../../migrations/0076_data_pivot_index.sql");
    let check = sql
        .find("select data_require_extensions(array['pg_trgm', 'btree_gin']);")
        .expect("事前確認がある");
    let create = sql
        .find("create extension if not exists pg_trgm;")
        .expect("拡張の作成");
    assert!(check < create, "拡張を作る前に確認する");
    // 欠けた拡張を名指しして feature_not_supported で落ちる。
    let err = sqlx::query("SELECT data_require_extensions(ARRAY['pg_trgm', 'shiki_no_such_ext'])")
        .execute(&pool)
        .await
        .expect_err("無い拡張は失敗");
    let sqlx::Error::Database(db) = err else {
        panic!("DB エラーのはず: {err:?}")
    };
    assert_eq!(db.code().as_deref(), Some("0A000"));
    assert!(
        db.message().contains("shiki_no_such_ext"),
        "{}",
        db.message()
    );
    assert!(
        !db.message().contains("pg_trgm"),
        "在るものは挙げない: {}",
        db.message()
    );
    // 揃っていれば通る。
    sqlx::query("SELECT data_require_extensions(ARRAY['pg_trgm', 'btree_gin'])")
        .execute(&pool)
        .await
        .unwrap();
}

/// 受け入れ条件: `FieldId` が 32,767 を超える採番要求が 422（Invalid）で拒否される。
#[tokio::test]
async fn field_id_above_int2_is_rejected() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    // 明示の id が上限超え。
    let mut f = field("x", FieldType::Text);
    f.id = 32_768;
    let r = store
        .create_table(
            &c,
            data::NewDataTable {
                name: "too-big".into(),
                schema: schema(vec![f]),
            },
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // 採番が上限に達した（next_field_id = 32,767）テーブルへ 2 列足すと、2 列目で拒否される。
    let table = create_table(
        &store,
        &c,
        "near-limit",
        schema(vec![field("a", FieldType::Text)]),
    )
    .await;
    sqlx::query("UPDATE data_table SET next_field_id = 32767 WHERE tenant_id = $1 AND id = $2")
        .bind(&tenant)
        .bind(table.id)
        .execute(&pool)
        .await
        .unwrap();
    let mut next = table.schema.clone();
    next.fields.push(field("b", FieldType::Text));
    let ok = store
        .update_table_schema(&c, table.id, next.clone(), None, None)
        .await
        .expect("32,767 までは採番できる");
    assert_eq!(ok.schema.field("b").unwrap().id, 32_767);
    let mut over = ok.schema.clone();
    over.fields.push(field("c", FieldType::Text));
    let r = store
        .update_table_schema(&c, table.id, over, None, None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
}

/// NumericSpec（data-platform §4.2）: 本体・索引・unique が正規化後の同一値を使う。
#[tokio::test]
async fn numeric_spec_normalizes_body_and_index_identically() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let mut price = field("price", FieldType::Number);
    price.indexed = true;
    price.unique = true;
    price.numeric_spec = Some(NumericSpec {
        scale: 2,
        on_excess_scale: ExcessScale::Round(RoundingMode::HalfUp),
        min: Some(0.0),
        max: Some(1000.0),
    });
    let mut qty = field("qty", FieldType::Number);
    qty.numeric_spec = Some(NumericSpec {
        scale: 0,
        on_excess_scale: ExcessScale::Reject,
        min: None,
        max: None,
    });
    let table = create_table(&store, &c, "numeric", schema(vec![price, qty])).await;
    let rec = store
        .create_record(&c, table.id, json!({"price": 1.005, "qty": 3}), None)
        .await
        .unwrap();
    assert_eq!(
        rec.data["price"],
        json!(1.01),
        "10 進で HalfUp（二進誤差を持ち込まない）"
    );
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let ri = record_int_of(&pool, ti, tb, rec.id).await;
    let (idx, body): (String, String) = sqlx::query_as(
        "SELECT i.val::text, (SELECT r.data ->> 'f16' FROM data_record r \
            WHERE r.tenant_int = $1 AND r.table_int = $2 AND r.record_int = $3) \
         FROM data_index_num i \
         WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.record_int = $3 AND i.field_id = 16",
    )
    .bind(ti)
    .bind(tb)
    .bind(ri)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(idx, "1.01");
    assert_eq!(body, "1.01");
    // 丸め後に同じ値になる入力は unique 違反。
    let dup = store
        .create_record(&c, table.id, json!({"price": 1.0149}), None)
        .await;
    assert!(matches!(dup, Err(DataError::Conflict(_))), "{dup:?}");
    // 正規化後の範囲外・reject の超過桁は 422。
    for bad in [
        json!({"price": 1000.005}),
        json!({"price": -0.01}),
        json!({"qty": 1.5}),
    ] {
        let r = store.create_record(&c, table.id, bad.clone(), None).await;
        assert!(matches!(r, Err(DataError::Invalid(_))), "{bad} -> {r:?}");
    }
    // 宣言は不変（改訂で変えられない）。
    let mut changed = table.schema.clone();
    changed.fields[0].numeric_spec.as_mut().unwrap().scale = 3;
    let r = store
        .update_table_schema(&c, table.id, changed, None, None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // number 以外への指定はスキーマ検証で 422。
    let mut bad = field("t", FieldType::Text);
    bad.numeric_spec = Some(NumericSpec {
        scale: 2,
        on_excess_scale: ExcessScale::Reject,
        min: None,
        max: None,
    });
    let r = store
        .create_table(
            &c,
            data::NewDataTable {
                name: "bad-spec".into(),
                schema: schema(vec![bad]),
            },
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    let report = store.verify_index_parity(&tenant, table.id).await.unwrap();
    assert!(report.is_consistent(), "{report:?}");
}
