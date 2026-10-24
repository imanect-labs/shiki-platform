//! 一括書込パス（Task 15.11・data-platform §16.7）の結合テスト。
//!
//! 受け入れ条件:
//! - 一括経路が通常書込と同一の不変条件を満たす（認可・型・required・unique・リンクのカーディナリティ・
//!   多値要素数上限・`rev`・revision・`Invariant` 材料化）。索引行・本体・revision を 1 件の create で
//!   作った表と行単位で突き合わせる
//! - 中断・再開でき、再実行が冪等（チャンクの途中で落とす・同じ取込を 2 ワーカーで同時に回す・
//!   ジョブを二重に積む・完了後に再実行する、のいずれでも本体と索引が 1 行ずつ）
//! - 取込後に索引 parity 検証が通る（差分ゼロ）
//! - 遅延構築（`building`）中の並行書込が完成後の索引に反映される
//!
//! 10 万行の所要時間と他テナントの p95 は `DATA_BENCH=1` のときだけ測る（`bench_100k_import`）。
//!
//! 取込ジョブのキューは DB で共有なので、このファイルのテストは**互いに直列**に走らせる（[`serial`]）。
//! 途中の状態を確かめるテストは自分のジョブをキューから外し、ステップを手で進める。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::print_stderr,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::many_single_char_names
)]

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, ReadTupleKey,
    Relation, Subject,
};
use common::{
    create_table, ctx, field, keys_of, ledger, rich_schema, schema, setup, store_with,
    unique_tenant, FixedResolver,
};
use data::{
    BulkImportOptions, BulkImportRow, BulkIndexMode, CondOp, Condition, DataError, DataQuery,
    DataStore, FieldDef, FieldType, LinkCardinality, LinkDef, Materialization, NewDataTable,
    RollupDef, RollupOp, SortKey,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

/// 取込ジョブのキューを共有するため、このファイルのテストを直列にする。
static SERIAL: Mutex<()> = Mutex::const_new(());

async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

/// 取込のジョブをキューから外す（ステップを手で進めて途中の状態を確かめるため・テスト専用）。
async fn detach_job(pool: &PgPool, import_id: Uuid) {
    sqlx::query("DELETE FROM job_queue WHERE queue = $1 AND payload->>'import_id' = $2")
        .bind(data::BULK_IMPORT_QUEUE)
        .bind(import_id.to_string())
        .execute(pool)
        .await
        .unwrap();
}

fn rows(v: Vec<Value>) -> Vec<BulkImportRow> {
    v.into_iter()
        .enumerate()
        .map(|(i, data)| BulkImportRow {
            key: Some(format!("k{}", i + 1)),
            data,
        })
        .collect()
}

async fn count(store: &DataStore, c: &AuthContext, table: Uuid, filter: Option<Condition>) -> u64 {
    let q = DataQuery {
        filter,
        limit: Some(1),
        with_count: true,
        ..DataQuery::default()
    };
    store
        .run_query(c, table, &q, None)
        .await
        .unwrap()
        .count
        .unwrap()
        .value()
}

fn eq(f: &str, v: Value) -> Condition {
    Condition::cmp(f, CondOp::Eq, Some(v))
}

/// 1 行分のデータ（`rich_schema` ＋参照列）。
fn rich_row(i: usize, customer: Option<Uuid>) -> Value {
    let status = ["draft", "submitted", "approved"][i % 3];
    let tags: Vec<&str> = ["a", "b", "c"].iter().take(i % 3 + 1).copied().collect();
    let mut v = json!({
        "title": format!("title-{i:05}"),
        "amount": i,
        "tags": tags,
        "status": status,
        "due": format!("2026-{:02}-{:02}", i % 12 + 1, i % 28 + 1),
        "memo": format!("memo\t{i}\n\\改行"),
    });
    if let Some(c) = customer {
        v["customer"] = json!([c.to_string()]);
        v["assignee"] = json!(if i.is_multiple_of(2) { "alice" } else { "bob" });
    }
    v
}

fn rich_with_refs(customers: Uuid) -> data::TableSchema {
    let mut s = rich_schema();
    s.fields
        .iter_mut()
        .find(|f| f.key == "title")
        .unwrap()
        .required = true;
    let mut cust = field("customer", FieldType::RecordRef);
    cust.ref_table = Some(customers);
    s.fields.push(cust);
    let mut assignee = field("assignee", FieldType::UserRef);
    assignee.indexed = true;
    s.fields.push(assignee);
    s
}

/// 索引行の多重集合（タイトル → 表ごとの (field_id, ord, 値) の並び）。record_int と時刻の擬似列は除く。
async fn index_rows_by_title(
    pool: &PgPool,
    tenant: &str,
    table: Uuid,
    title_field: u16,
) -> BTreeMap<String, Vec<String>> {
    let (ti, tb) = keys_of(pool, tenant, table).await;
    let title_key = format!("f{title_field}");
    let rows: Vec<(String, String)> = sqlx::query_as(
        "WITH r AS (SELECT record_int, data ->> $3 AS title FROM data_record \
                    WHERE tenant_int = $1 AND table_int = $2) \
         SELECT r.title, x.v FROM r JOIN ( \
           SELECT record_int, 'text:' || field_id || ':' || ord || ':' || val || ':' || truncated AS v \
             FROM data_index_text WHERE tenant_int = $1 AND table_int = $2 AND field_id NOT IN (1, 2) \
           UNION ALL SELECT record_int, 'num:' || field_id || ':' || ord || ':' || val \
             FROM data_index_num WHERE tenant_int = $1 AND table_int = $2 \
           UNION ALL SELECT record_int, 'search:' || field_id || ':' || ord || ':' || val \
             FROM data_index_search WHERE tenant_int = $1 AND table_int = $2 \
           UNION ALL SELECT record_int, 'unique:' || field_id || ':' || val_norm \
             FROM data_unique WHERE tenant_int = $1 AND table_int = $2 \
           UNION ALL SELECT src_record, 'link:' || field_id || ':' || ord || ':' || dst_table_int \
                   || ':' || dst_record || ':' || is_reverse || ':' || single_valued \
             FROM data_link WHERE tenant_int = $1 AND table_int = $2 \
         ) x ON x.record_int = r.record_int",
    )
    .bind(ti)
    .bind(tb)
    .bind(&title_key)
    .fetch_all(pool)
    .await
    .unwrap();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (t, v) in rows {
        out.entry(t).or_default().push(v);
    }
    for v in out.values_mut() {
        v.sort();
    }
    out
}

/// 本体（タイトル → (data, rev, owner)）。
async fn bodies_by_title(
    pool: &PgPool,
    tenant: &str,
    table: Uuid,
    title_field: u16,
) -> BTreeMap<String, (Value, i64, String)> {
    let (ti, tb) = keys_of(pool, tenant, table).await;
    let rows: Vec<(Value, i64, String)> = sqlx::query_as(
        "SELECT data, rev, owner FROM data_record WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_all(pool)
    .await
    .unwrap();
    let key = format!("f{title_field}");
    rows.into_iter()
        .map(|(d, rev, owner)| (d[&key].as_str().unwrap().to_string(), (d, rev, owner)))
        .collect()
}

/// リビジョン（タイトル → (rev, change_kind, patch)）。
async fn revisions_by_title(
    pool: &PgPool,
    tenant: &str,
    table: Uuid,
) -> BTreeMap<String, Vec<(i64, String, Value)>> {
    let rows: Vec<(i64, String, Value)> = sqlx::query_as(
        "SELECT rev, change_kind, patch FROM data_record_revision \
         WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(tenant)
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap();
    let mut out: BTreeMap<String, Vec<(i64, String, Value)>> = BTreeMap::new();
    for (rev, kind, patch) in rows {
        let title = patch
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["field"] == "title")
            .map(|p| p["new"].as_str().unwrap().to_string())
            .unwrap();
        out.entry(title).or_default().push((rev, kind, patch));
    }
    out
}

/// 受け入れ条件: 一括経路が通常書込と同一の不変条件を満たす。
#[tokio::test]
async fn bulk_import_matches_single_creates_and_rejects_invalid_rows() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone()).with_max_multi_values(3);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let customers = create_table(
        &store,
        &c,
        "customers",
        schema(vec![field("name", FieldType::Text)]),
    )
    .await;
    let mut cust_ids = Vec::new();
    for i in 0..3 {
        cust_ids.push(
            store
                .create_record(&c, customers.id, json!({"name": format!("c{i}")}), None)
                .await
                .unwrap()
                .id,
        );
    }
    let single = create_table(&store, &c, "single", rich_with_refs(customers.id)).await;
    let bulk = create_table(&store, &c, "bulk", rich_with_refs(customers.id)).await;
    // 既存の行（unique の衝突相手）。
    store
        .create_record(
            &c,
            bulk.id,
            json!({"title": "existing", "amount": 999_999}),
            None,
        )
        .await
        .unwrap();

    let n = 300;
    let mut data: Vec<Value> = (0..n).map(|i| rich_row(i, Some(cust_ids[i % 3]))).collect();
    // 不合格の行（行番号 = 添字 + 1）。
    data[4] = json!({"amount": 4}); // required 欠落
    data[9]["amount"] = json!("x"); // 型
    data[14]["amount"] = json!(13); // 取込内の unique 重複（13 行目の値が先）
    data[19]["amount"] = json!(999_999); // 既存行との unique 重複
    data[24]["tags"] = json!(["a", "b", "c", "d"]); // 多値要素数上限（3）
    data[29]["customer"] = json!([Uuid::new_v4().to_string()]); // 参照先が無い
    data[34]["assignee"] = json!("carol"); // 存在しないユーザー
    data[39]["due"] = json!("2026-13-45"); // 日付でない（未宣言の列は取込全体を 403・下で確かめる）
    data[44]["status"] = json!("unknown"); // 選択肢外
    data[149]["amount"] = json!(2); // チャンクを跨ぐ取込内の unique 重複（3 行目の値が先）
    let bad_rows = [5, 10, 15, 20, 25, 30, 35, 40, 45, 150];
    // 未宣言の列を含む作成行は、読めない列と同じく取込全体を 403 `field_unavailable`（列の存在オラクルを
    // 作らない・検証 2-B-P2-1）。
    let mut with_unknown = data.clone();
    with_unknown[39]["nope"] = json!(1);
    let r = store
        .start_bulk_import(
            &c,
            bulk.id,
            rows(with_unknown),
            BulkImportOptions::default(),
            None,
        )
        .await;
    assert!(matches!(r, Err(data::DataError::FieldUnavailable)), "{r:?}");

    let id = store
        .start_bulk_import(
            &c,
            bulk.id,
            rows(data.clone()),
            BulkImportOptions::default(),
            Some("trace-bulk"),
        )
        .await
        .unwrap();
    detach_job(&pool, id).await;
    // 段ごとに 100 行のチャンクで進める（チャンク境界を跨ぐ unique 重複も検出する）。
    let mut seen_states = Vec::new();
    loop {
        let step = store
            .bulk_import_step(&tenant, bulk.id, id, 100)
            .await
            .unwrap();
        seen_states.push(step.status.clone());
        if step.done {
            break;
        }
    }
    assert!(seen_states.contains(&"applying".to_string()));
    assert!(seen_states.contains(&"indexing".to_string()));
    let st = store.bulk_import_status(&c, bulk.id, id).await.unwrap();
    assert_eq!(st.status, "done", "{st:?}");
    assert_eq!(st.rows_total, n as i64);
    assert_eq!(st.rows_invalid, bad_rows.len() as i64);
    assert_eq!(st.rows_valid, (n - bad_rows.len()) as i64);
    assert_eq!(st.rows_applied, st.rows_valid);
    assert_eq!(st.rows_rejected, 0);
    assert_eq!(st.parity_mismatches, Some(0), "取込後の parity が一致");
    assert!(!st.index_deferred);

    let errors = store
        .bulk_import_errors(&c, bulk.id, id, 0, 100)
        .await
        .unwrap();
    let got: Vec<i64> = errors.iter().map(|e| e.row_no).collect();
    assert_eq!(got, bad_rows.to_vec(), "{errors:?}");
    let msg = |row: i64| {
        errors
            .iter()
            .find(|e| e.row_no == row)
            .unwrap()
            .message
            .clone()
    };
    assert!(msg(5).contains("必須"), "{}", msg(5));
    assert!(msg(10).contains("数値"), "{}", msg(10));
    assert!(msg(15).contains("unique"), "{}", msg(15));
    assert!(msg(20).contains("unique"), "{}", msg(20));
    assert!(
        msg(25).contains("最大") || msg(25).contains("多"),
        "{}",
        msg(25)
    );
    assert!(msg(30).contains("見つかりません"), "{}", msg(30));
    assert!(msg(150).contains("unique"), "{}", msg(150));
    assert_eq!(errors[0].key.as_deref(), Some("k5"));
    // 2 ページ目（keyset）。
    let page2 = store
        .bulk_import_errors(&c, bulk.id, id, 30, 2)
        .await
        .unwrap();
    assert_eq!(
        page2.iter().map(|e| e.row_no).collect::<Vec<_>>(),
        vec![35, 40]
    );

    // 同じ合格行を 1 件ずつ create した表と突き合わせる（本体・rev・owner・索引行・revision）。
    for (i, d) in data.iter().enumerate() {
        if bad_rows.contains(&(i as i64 + 1)) {
            continue;
        }
        store
            .create_record(&c, single.id, d.clone(), None)
            .await
            .unwrap();
    }
    let title_id = bulk.schema.field("title").unwrap().id;
    assert_eq!(single.schema.field("title").unwrap().id, title_id);
    let mut b_idx = index_rows_by_title(&pool, &tenant, bulk.id, title_id).await;
    b_idx.remove("existing");
    let s_idx = index_rows_by_title(&pool, &tenant, single.id, title_id).await;
    assert_eq!(b_idx.len(), n - bad_rows.len());
    assert_eq!(b_idx, s_idx, "索引行が 1 件ずつの create と同一");
    let mut b_body = bodies_by_title(&pool, &tenant, bulk.id, title_id).await;
    b_body.remove("existing");
    let s_body = bodies_by_title(&pool, &tenant, single.id, title_id).await;
    assert_eq!(b_body, s_body, "本体・rev・owner が同一");
    assert!(b_body
        .values()
        .all(|(_, rev, owner)| *rev == 1 && owner == "alice"));
    let mut b_rev = revisions_by_title(&pool, &tenant, bulk.id).await;
    b_rev.remove("existing");
    let s_rev = revisions_by_title(&pool, &tenant, single.id).await;
    assert_eq!(b_rev, s_rev, "revision（rev=1・create・差分）が同一");
    let traced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_record_revision WHERE tenant_id = $1 AND table_id = $2 \
           AND trace_id = 'trace-bulk'",
    )
    .bind(&tenant)
    .bind(bulk.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(traced, st.rows_applied);

    // 索引で引ける（取込直後からクエリに使える）。
    let approved = data
        .iter()
        .enumerate()
        .filter(|(i, d)| !bad_rows.contains(&(*i as i64 + 1)) && d["status"] == "approved")
        .count() as u64;
    assert_eq!(
        count(&store, &c, bulk.id, Some(eq("status", json!("approved")))).await,
        approved
    );
    let p = store.verify_index_parity(&tenant, bulk.id).await.unwrap();
    assert!(p.is_consistent(), "{p:?}");
    // 監査: 開始 1 行＋チャンクごとの適用。作成したレコードの外部 ID が載る。
    let audited: Vec<Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_log WHERE tenant_id = $1 AND action = 'data.record.bulk_apply'",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    let audited_ids: usize = audited
        .iter()
        .map(|m| m["record_ids"].as_array().unwrap().len())
        .sum();
    assert_eq!(audited_ids as i64, st.rows_applied);
    // 作業領域は片付き、エラー行だけが残る。
    let (ti, _) = keys_of(&pool, &tenant, bulk.id).await;
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_bulk_staging WHERE tenant_int = $1 AND import_id = $2",
    )
    .bind(ti)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, bad_rows.len() as i64);
}

/// 入力の形の検査・全件か無か・観測 API の越境。
#[tokio::test]
async fn bulk_import_input_checks_and_abort_on_invalid() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let t = create_table(&store, &c, "t", rich_schema()).await;
    let other = create_table(&store, &c, "other", rich_schema()).await;
    let start = |r: Vec<BulkImportRow>, o: BulkImportOptions| {
        let store = store.clone();
        let c = c.clone();
        async move { store.start_bulk_import(&c, t.id, r, o, None).await }
    };
    let invalid =
        |r: Result<Uuid, DataError>| assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");
    invalid(start(vec![], BulkImportOptions::default()).await);
    invalid(
        start(
            vec![BulkImportRow {
                key: None,
                data: json!([1]),
            }],
            BulkImportOptions::default(),
        )
        .await,
    );
    let dup = vec![
        BulkImportRow {
            key: Some("a".into()),
            data: json!({"title": "x"}),
        },
        BulkImportRow {
            key: Some("a".into()),
            data: json!({"title": "y"}),
        },
    ];
    invalid(start(dup, BulkImportOptions::default()).await);
    // 失敗した開始は何も残さない（COPY と取込行は同じ Tx）。
    let (ti, tb) = keys_of(&pool, &tenant, t.id).await;
    let imports: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_bulk_import WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(imports, 0);

    // 全件か無か: 1 行でも不合格なら何も適用しない。
    let id = start(
        rows(vec![
            json!({"title": "ok", "amount": 1}),
            json!({"title": "bad", "amount": "x"}),
        ]),
        BulkImportOptions {
            abort_on_invalid: true,
            ..BulkImportOptions::default()
        },
    )
    .await
    .unwrap();
    detach_job(&pool, id).await;
    let step = store.run_bulk_import(&tenant, t.id, id).await.unwrap();
    assert_eq!(step.status, "failed");
    let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
    assert_eq!(st.rows_applied, 0);
    assert!(st.last_error.unwrap().contains("1 行"));
    assert_eq!(count(&store, &c, t.id, None).await, 0);
    let errs = store.bulk_import_errors(&c, t.id, id, 0, 10).await.unwrap();
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].row_no, 2);
    // 別のテーブルの取込 ID は見えない（同一応答の NotFound）。
    assert!(matches!(
        store.bulk_import_status(&c, other.id, id).await,
        Err(DataError::NotFound)
    ));
    assert!(matches!(
        store.bulk_import_errors(&c, other.id, id, 0, 10).await,
        Err(DataError::NotFound)
    ));
    assert!(matches!(
        store.bulk_import_status(&c, t.id, Uuid::new_v4()).await,
        Err(DataError::NotFound)
    ));
}

/// 受け入れ条件: 中断・再開でき、再実行が冪等（チャンク境界で二重適用が起きない）。
#[tokio::test]
async fn bulk_import_survives_interruptions_and_duplicate_workers() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let t = create_table(&store, &c, "t", rich_schema()).await;
    let n = 2_000;
    let data: Vec<Value> = (0..n).map(|i| rich_row(i, None)).collect();
    let id = store
        .start_bulk_import(
            &c,
            t.id,
            rows(data),
            BulkImportOptions {
                index_mode: BulkIndexMode::Immediate,
                ..BulkImportOptions::default()
            },
            None,
        )
        .await
        .unwrap();
    detach_job(&pool, id).await;
    // チャンクの途中で落とす（タスクを中断＝Tx を捨てる）。どこで落ちても丸ごとロールバックされる。
    for delay_ms in [0u64, 1, 3, 5, 8, 13, 21, 34, 2, 4, 6, 9, 15, 25] {
        let s = store.clone();
        let tn = tenant.clone();
        let h = tokio::spawn(async move { s.bulk_import_step(&tn, t.id, id, 300).await });
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        h.abort();
        let _ = h.await;
    }
    // 同じ取込を 2 つのワーカーで同時に回す（片方は進捗の確認で何もしない）。
    let (a, b) = tokio::join!(
        store.run_bulk_import(&tenant, t.id, id),
        store.run_bulk_import(&tenant, t.id, id)
    );
    assert_eq!(a.unwrap().status, "done");
    assert_eq!(b.unwrap().status, "done");
    // 完了後の再実行・ジョブの二重投入は何もしない。
    let again = store
        .bulk_import_step(&tenant, t.id, id, 300)
        .await
        .unwrap();
    assert!(again.done);
    assert_eq!(again.processed, 0);
    let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
    assert_eq!(st.rows_applied, n as i64, "{st:?}");
    assert_eq!(st.parity_mismatches, Some(0));
    let (ti, tb) = keys_of(&pool, &tenant, t.id).await;
    let (records, distinct_titles): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(DISTINCT data->>'f16') FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((records, distinct_titles), (n as i64, n as i64));
    let revisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_record_revision WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(&tenant)
    .bind(t.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(revisions, n as i64);
    // 擬似列（owner）の索引行もレコードごとに 1 行。
    let owner_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_index_text WHERE tenant_int = $1 AND table_int = $2 AND field_id = 0",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner_rows, n as i64);

    // ジョブ経路: ジョブを失った取込はスイープが積み直し、ワーカーが最後まで進める（二重投入も冪等）。
    let t2 = create_table(&store, &c, "t2", rich_schema()).await;
    let id2 = store
        .start_bulk_import(
            &c,
            t2.id,
            rows((0..700).map(|i| rich_row(i, None)).collect()),
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    // ジョブを失った状態にする（このファイルのテストは直列なので、キューの取込ジョブを全部外してよい）。
    sqlx::query("DELETE FROM job_queue WHERE queue = $1")
        .bind(data::BULK_IMPORT_QUEUE)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.enqueue_bulk_imports_if_needed().await.unwrap() >= 1);
    // もう 1 本積んで二重に配信させる。
    sqlx::query(
        "INSERT INTO job_queue (queue, tenant_id, payload, max_attempts) \
         SELECT queue, tenant_id, payload, max_attempts FROM job_queue \
         WHERE queue = $1 AND payload->>'import_id' = $2",
    )
    .bind(data::BULK_IMPORT_QUEUE)
    .bind(id2.to_string())
    .execute(&pool)
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_mins(2);
    loop {
        let st = store.bulk_import_status(&c, t2.id, id2).await.unwrap();
        if st.status == "done" {
            assert_eq!(st.rows_applied, 700);
            break;
        }
        assert!(Instant::now() < deadline, "ジョブ経路で完了しない: {st:?}");
        if !store.consume_bulk_job_once().await.unwrap() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    while store.consume_bulk_job_once().await.unwrap() {}
    assert_eq!(count(&store, &c, t2.id, None).await, 700);
}

/// 受け入れ条件: 遅延構築（building）中の並行書込が完成後の索引に反映され、parity がゼロ差分。
#[tokio::test]
async fn deferred_index_build_keeps_concurrent_writes() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let t = create_table(&store, &c, "t", rich_schema()).await;
    // 既存の行（保持の前に索引が揃っている）。
    let mut existing = Vec::new();
    for i in 0..40 {
        existing.push(
            store
                .create_record(&c, t.id, rich_row(100_000 + i, None), None)
                .await
                .unwrap(),
        );
    }
    let n = 1_500;
    let id = store
        .start_bulk_import(
            &c,
            t.id,
            rows((0..n).map(|i| rich_row(i, None)).collect()),
            BulkImportOptions {
                index_mode: BulkIndexMode::Deferred,
                ..BulkImportOptions::default()
            },
            None,
        )
        .await
        .unwrap();
    detach_job(&pool, id).await;
    // 検証を終えて適用に入る（ここで値索引を building に保持する）。
    loop {
        let s = store
            .bulk_import_step(&tenant, t.id, id, 500)
            .await
            .unwrap();
        if s.status != "validating" {
            break;
        }
    }
    let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
    assert_eq!(st.status, "applying");
    assert!(st.index_deferred);
    let (ti, tb) = keys_of(&pool, &tenant, t.id).await;
    let status_id = i16::try_from(t.schema.field("status").unwrap().id).unwrap();
    let amount_id = i16::try_from(t.schema.field("amount").unwrap().id).unwrap();
    let states = ledger(&pool, ti, tb).await;
    let state_of = |f: i16, tbl: i16| {
        states
            .iter()
            .find(|(x, y, _)| *x == f && *y == tbl)
            .map(|s| s.2.clone())
    };
    assert_eq!(state_of(status_id, 1).as_deref(), Some("building"));
    assert_eq!(state_of(amount_id, 2).as_deref(), Some("building"));
    assert_eq!(
        state_of(amount_id, 4).as_deref(),
        Some("active"),
        "unique は遅延させない"
    );
    assert_eq!(
        state_of(0, 1).as_deref(),
        Some("active"),
        "擬似列は遅延させない"
    );
    // building の列ではクエリできない（準備中・409 の構造化された理由と進捗・Task 15.6）。
    // 擬似列の駆動（フィルタなし）はできる。
    let q = DataQuery {
        filter: Some(eq("status", json!("approved"))),
        ..DataQuery::default()
    };
    // 進捗は取込の適用の進み具合（準備中に 1.0＝完了と見せない・検証 2-D-P3-4）。
    match store.run_query(&c, t.id, &q, None).await {
        Err(DataError::IndexPreparing { progress, .. }) => {
            assert!(progress.is_some_and(|p| p < 1.0), "{progress:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(count(&store, &c, t.id, None).await, 40);

    // 適用と並行して書く: 別タスクが作成・更新・削除を続ける。
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let store = store.clone();
        let c = c.clone();
        let stop = Arc::clone(&stop);
        let existing = existing.clone();
        tokio::spawn(async move {
            let mut i = 0usize;
            let mut created = Vec::new();
            while !stop.load(Ordering::SeqCst) || i < 30 {
                let r = store
                    .create_record(&c, t.id, rich_row(200_000 + i, None), None)
                    .await;
                match r {
                    Ok(rec) => created.push(rec),
                    Err(DataError::UniqueViolation) => {}
                    Err(e) => panic!("{e:?}"),
                }
                if let Some(e) = existing.get(i % existing.len()) {
                    let cur = store.get_record(&c, t.id, e.id, None).await.unwrap();
                    let _ = store
                        .update_record(
                            &c,
                            t.id,
                            e.id,
                            json!({"status": "approved", "title": format!("upd-{i}")}),
                            cur.rev,
                            None,
                        )
                        .await;
                }
                i += 1;
                tokio::task::yield_now().await;
            }
            created
        })
    };
    // 取込の行も並行して更新・削除する（適用済みのレコードを本体から拾う）。
    let mut touched = 0;
    loop {
        let s = store
            .bulk_import_step(&tenant, t.id, id, 200)
            .await
            .unwrap();
        let recent: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT id, rev FROM data_record WHERE tenant_int = $1 AND table_int = $2 \
               AND data->>'f16' LIKE 'title-%' ORDER BY record_int DESC LIMIT 2",
        )
        .bind(ti)
        .bind(tb)
        .fetch_all(&pool)
        .await
        .unwrap();
        if let [(a, arev), (b, brev)] = recent.as_slice() {
            if store
                .update_record(
                    &c,
                    t.id,
                    *a,
                    json!({"status": "draft", "tags": ["d"]}),
                    *arev,
                    None,
                )
                .await
                .is_ok()
            {
                touched += 1;
            }
            let _ = store.delete_record(&c, t.id, *b, *brev, None).await;
        }
        if s.done {
            break;
        }
    }
    stop.store(true, Ordering::SeqCst);
    let created = writer.await.unwrap();
    assert!(touched > 0 && !created.is_empty());
    let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
    assert_eq!(st.status, "done", "{st:?}");
    assert!(!st.index_deferred);
    assert_eq!(st.parity_mismatches, Some(0), "{st:?}");
    assert!(
        ledger(&pool, ti, tb)
            .await
            .iter()
            .all(|(_, _, s)| s == "active"),
        "全索引が active へ戻る"
    );
    // 本体から数えた件数と、索引で引いた件数が一致する（並行書込が欠落していない）。
    for v in ["draft", "submitted", "approved"] {
        let truth: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM data_record WHERE tenant_int = $1 AND table_int = $2 \
               AND data->>$3 = $4",
        )
        .bind(ti)
        .bind(tb)
        .bind(format!("f{status_id}"))
        .bind(v)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            count(&store, &c, t.id, Some(eq("status", json!(v)))).await,
            truth as u64,
            "{v}"
        );
    }
    let p = store.verify_index_parity(&tenant, t.id).await.unwrap();
    assert!(p.is_consistent(), "{p:?}");
}

/// 遅延構築で保持するのは取込が値を書く列の索引だけ（検証 D-5）。取込が触れない列の既存の索引は
/// 取込の間も使える（以前はテーブルの全ての値索引が building になり、取込の間ずっと 409 だった）。
#[tokio::test]
async fn deferred_import_keeps_untouched_indexes_active() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let t = create_table(&store, &c, "t", rich_schema()).await;
    for i in 0..10 {
        store
            .create_record(&c, t.id, rich_row(100_000 + i, None), None)
            .await
            .unwrap();
    }
    // 取込は title と amount だけを書く（status・due・tags は値を持たない）。
    let n = 1_200;
    let id = store
        .start_bulk_import(
            &c,
            t.id,
            rows(
                (0..n)
                    .map(|i| json!({"title": format!("imp-{i:05}"), "amount": i}))
                    .collect(),
            ),
            BulkImportOptions {
                index_mode: BulkIndexMode::Deferred,
                ..BulkImportOptions::default()
            },
            None,
        )
        .await
        .unwrap();
    detach_job(&pool, id).await;
    loop {
        let s = store
            .bulk_import_step(&tenant, t.id, id, 500)
            .await
            .unwrap();
        if s.status != "validating" {
            break;
        }
    }
    let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
    assert_eq!(st.status, "applying");
    assert!(st.index_deferred);
    let (ti, tb) = keys_of(&pool, &tenant, t.id).await;
    let fid = |k: &str| i16::try_from(t.schema.field(k).unwrap().id).unwrap();
    let states = ledger(&pool, ti, tb).await;
    let state_of = |f: i16, tbl: i16| {
        states
            .iter()
            .find(|(x, y, _)| *x == f && *y == tbl)
            .map(|s| s.2.clone())
    };
    assert_eq!(state_of(fid("title"), 1).as_deref(), Some("building"));
    assert_eq!(state_of(fid("amount"), 2).as_deref(), Some("building"));
    for k in ["status", "due", "tags"] {
        assert_eq!(
            state_of(fid(k), 1).as_deref(),
            Some("active"),
            "取込が書かない列 {k} は保持しない"
        );
    }
    // 取込が書かない列では、取込の途中でも絞り込み・並べ替えできる。
    let q = DataQuery {
        filter: Some(eq("status", json!("approved"))),
        sort: vec![SortKey {
            field: "due".into(),
            descending: false,
        }],
        ..DataQuery::default()
    };
    store.run_query(&c, t.id, &q, None).await.unwrap();
    let q = DataQuery {
        filter: Some(eq("title", json!("imp-00001"))),
        ..DataQuery::default()
    };
    assert!(matches!(
        store.run_query(&c, t.id, &q, None).await,
        Err(DataError::IndexPreparing { .. })
    ));
    // 適用の途中で取込の行が増えても、取込が書かない列の結果は本体と一致する（取込の行は値なし）。
    store
        .bulk_import_step(&tenant, t.id, id, 300)
        .await
        .unwrap();
    let approved = count(&store, &c, t.id, Some(eq("status", json!("approved")))).await;
    let truth: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND data->>$3 = 'approved'",
    )
    .bind(ti)
    .bind(tb)
    .bind(format!("f{}", fid("status")))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(approved, truth as u64);
    loop {
        if store
            .bulk_import_step(&tenant, t.id, id, 500)
            .await
            .unwrap()
            .done
        {
            break;
        }
    }
    let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
    assert_eq!(st.status, "done", "{st:?}");
    assert_eq!(st.parity_mismatches, Some(0), "{st:?}");
    let p = store.verify_index_parity(&tenant, t.id).await.unwrap();
    assert!(p.is_consistent(), "{p:?}");
    assert_eq!(
        count(&store, &c, t.id, Some(eq("title", json!("imp-00001")))).await,
        1
    );
}

/// 実行中にスキーマが改訂されたら、未適用の行を新しいスキーマで検証し直す。
#[tokio::test]
async fn schema_change_mid_import_revalidates_unapplied_rows() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let t = create_table(&store, &c, "t", rich_schema()).await;
    let data: Vec<Value> = (0..600)
        .map(|i| {
            let mut r = rich_row(i, None);
            if i >= 400 {
                r["status"] = json!("later");
            }
            r
        })
        .collect();
    let id = store
        .start_bulk_import(&c, t.id, rows(data), BulkImportOptions::default(), None)
        .await
        .unwrap();
    detach_job(&pool, id).await;
    loop {
        let s = store
            .bulk_import_step(&tenant, t.id, id, 200)
            .await
            .unwrap();
        if s.status == "applying" {
            break;
        }
    }
    assert_eq!(
        store
            .bulk_import_status(&c, t.id, id)
            .await
            .unwrap()
            .rows_invalid,
        200,
        "旧スキーマでは later が選択肢に無い"
    );
    // 1 チャンク適用してからスキーマを改訂（status に later を足す）。
    store
        .bulk_import_step(&tenant, t.id, id, 200)
        .await
        .unwrap();
    let mut s = t.schema.clone();
    s.fields
        .iter_mut()
        .find(|f| f.key == "status")
        .unwrap()
        .options
        .push("later".into());
    store
        .update_table_schema(&c, t.id, s, Some(t.schema_version), None)
        .await
        .unwrap();
    let done = store.run_bulk_import(&tenant, t.id, id).await.unwrap();
    assert_eq!(done.status, "done");
    let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
    assert_eq!((st.rows_applied, st.rows_invalid), (600, 0), "{st:?}");
    assert_eq!(st.parity_mismatches, Some(0));
    store.backfill_table_index(&tenant, t.id).await.unwrap();
    assert_eq!(
        count(&store, &c, t.id, Some(eq("status", json!("later")))).await,
        200
    );
}

/// テーブルが取込中に削除されたら取込を `failed` で閉じ、作業領域を片付ける（適用済みの行は残る）。
#[tokio::test]
async fn table_deletion_mid_import_fails_and_cleans_up() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let t = create_table(&store, &c, "t", rich_schema()).await;
    let id = store
        .start_bulk_import(
            &c,
            t.id,
            rows((0..600).map(|i| rich_row(i, None)).collect()),
            BulkImportOptions {
                index_mode: BulkIndexMode::Immediate,
                ..BulkImportOptions::default()
            },
            None,
        )
        .await
        .unwrap();
    detach_job(&pool, id).await;
    loop {
        let s = store
            .bulk_import_step(&tenant, t.id, id, 300)
            .await
            .unwrap();
        if s.status == "applying" {
            break;
        }
    }
    store
        .bulk_import_step(&tenant, t.id, id, 300)
        .await
        .unwrap();
    store.delete_table(&c, t.id, None).await.unwrap();
    let s = store.run_bulk_import(&tenant, t.id, id).await.unwrap();
    assert_eq!(s.status, "failed");
    let (ti, tb) = keys_of(&pool, &tenant, t.id).await;
    let (status, applied, err): (String, i64, Option<String>) = sqlx::query_as(
        "SELECT status, rows_applied, last_error FROM data_bulk_import \
         WHERE tenant_int = $1 AND table_int = $2 AND id = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((status.as_str(), applied), ("failed", 300));
    assert!(err.unwrap().contains("見つかりません"));
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_bulk_staging WHERE tenant_int = $1 AND import_id = $2",
    )
    .bind(ti)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0, "作業領域が片付く");
    // テナントの撤去で取込の表も消える。
    store.purge_tenant(&tenant).await.unwrap();
    let imports: i64 =
        sqlx::query_scalar("SELECT count(*) FROM data_bulk_import WHERE tenant_int = $1")
            .bind(ti)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(imports, 0);
}

/// 認可を切り替えられるテストダブル（bob は editor でない・`secret` テーブルは誰も見えない・
/// `revoked` が立つと alice の editor も外れる）。
struct Switch {
    secret: std::sync::Mutex<Option<Uuid>>,
    revoked: AtomicBool,
}

#[async_trait]
impl AuthzClient for Switch {
    async fn check(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        let secret = *self.secret.lock().unwrap();
        if secret.is_some_and(|id| o.as_str().ends_with(&id.to_string())) {
            return Ok(false);
        }
        if r == Relation::Editor || r == Relation::Owner {
            if s.as_str().ends_with("|bob") {
                return Ok(false);
            }
            if self.revoked.load(Ordering::SeqCst) {
                return Ok(false);
            }
        }
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// 受け入れ条件（認可）: editor でない主体は始められない・見えない参照先への参照は不合格・
/// 取込中に editor を剥奪されたら次のチャンクから止まる（保持していた索引は解放して作り直す）。
#[tokio::test]
async fn bulk_import_enforces_authorization_per_chunk() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let authz = Arc::new(Switch {
        secret: std::sync::Mutex::new(None),
        revoked: AtomicBool::new(false),
    });
    let store = DataStore::new(
        pool.clone(),
        Arc::clone(&authz) as Arc<dyn AuthzClient>,
        Arc::new(FixedResolver),
    );
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let secret = create_table(
        &store,
        &c,
        "secret",
        schema(vec![field("name", FieldType::Text)]),
    )
    .await;
    let hidden = store
        .create_record(&c, secret.id, json!({"name": "h"}), None)
        .await
        .unwrap()
        .id;
    let t = create_table(&store, &c, "t", rich_with_refs(secret.id)).await;
    *authz.secret.lock().unwrap() = Some(secret.id);
    let bob = ctx(&tenant, "bob");
    assert!(matches!(
        store
            .start_bulk_import(
                &bob,
                t.id,
                rows(vec![json!({"title": "x"})]),
                BulkImportOptions::default(),
                None
            )
            .await,
        Err(DataError::Forbidden)
    ));
    // 見えない参照先への参照は「見つからない」（存在オラクルなし・PIT-20）。
    let mut data: Vec<Value> = (0..900).map(|i| rich_row(i, None)).collect();
    data[0]["customer"] = json!([hidden.to_string()]);
    let id = store
        .start_bulk_import(
            &c,
            t.id,
            rows(data),
            BulkImportOptions {
                index_mode: BulkIndexMode::Deferred,
                ..BulkImportOptions::default()
            },
            None,
        )
        .await
        .unwrap();
    detach_job(&pool, id).await;
    loop {
        let s = store
            .bulk_import_step(&tenant, t.id, id, 300)
            .await
            .unwrap();
        if s.status == "applying" {
            break;
        }
    }
    let errs = store.bulk_import_errors(&c, t.id, id, 0, 10).await.unwrap();
    assert_eq!(errs.len(), 1);
    assert!(errs[0].message.contains("見つかりません"), "{errs:?}");
    // 1 チャンク適用 → 剥奪 → 止まる。
    store
        .bulk_import_step(&tenant, t.id, id, 300)
        .await
        .unwrap();
    let applied = store
        .bulk_import_status(&c, t.id, id)
        .await
        .unwrap()
        .rows_applied;
    assert_eq!(applied, 299);
    authz.revoked.store(true, Ordering::SeqCst);
    let done = store.run_bulk_import(&tenant, t.id, id).await.unwrap();
    assert_eq!(done.status, "failed");
    authz.revoked.store(false, Ordering::SeqCst);
    let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
    assert_eq!(st.rows_applied, 299, "剥奪の後は適用しない");
    assert!(st.last_error.as_deref().unwrap().contains("権限"), "{st:?}");
    assert!(!st.index_deferred, "保持は解放される");
    assert_eq!(st.parity_mismatches, Some(0));
    let (ti, tb) = keys_of(&pool, &tenant, t.id).await;
    assert!(ledger(&pool, ti, tb)
        .await
        .iter()
        .all(|(_, _, s)| s == "active"));
    assert_eq!(
        count(&store, &c, t.id, Some(eq("status", json!("draft")))).await,
        99,
        "1 チャンク目（1〜300 行目・1 行目は不合格）の draft だけ"
    );
}

fn link(key: &str, to: Uuid, card: LinkCardinality, sym: Option<&str>) -> FieldDef {
    let mut f = field(key, FieldType::RecordRef);
    f.ref_table = Some(to);
    f.link = Some(LinkDef {
        cardinality: card,
        symmetric_field: None,
        symmetric_key: sym.map(Into::into),
        symmetric_display_name: None,
        reverse: false,
    });
    f
}

async fn fga_store(pool: PgPool) -> Option<DataStore> {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let model: Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url,
            store_name: format!("shiki-data-bulk-test-{}", Uuid::new_v4()),
        },
        &model,
    )
    .await
    .expect("OpenFGA 接続");
    Some(DataStore::new(pool, Arc::new(fga), Arc::new(FixedResolver)))
}

/// 受け入れ条件: `Invariant` 材料化とリンクのカーディナリティが一括経路でも同じ規則で効く（実 OpenFGA）。
#[tokio::test]
async fn bulk_import_materializes_invariant_rollups_and_enforces_link_cardinality() {
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let Some(store) = fga_store(pool.clone()).await else {
        return;
    };
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let app = Uuid::new_v4();
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    // ロールアップの小さいセル抑制（K）を 1 にする（件数 2 の合計も値を持つ・PIT-17）。
    let mut lines_schema = schema(vec![amount]);
    lines_schema.aggregate_min_rows = Some(1);
    let lines = store
        .create_table_for_app(
            &c,
            NewDataTable {
                name: "lines".into(),
                schema: lines_schema,
            },
            app,
            None,
        )
        .await
        .unwrap();
    let mut line_ids = Vec::new();
    for a in [100, 250, 300, 50] {
        line_ids.push(
            store
                .create_record(&c, lines.id, json!({"amount": a}), None)
                .await
                .unwrap()
                .id,
        );
    }
    let mut total = field("total", FieldType::Rollup);
    total.rollup = Some(RollupDef {
        link: "items".into(),
        target: Some("amount".into()),
        op: RollupOp::Sum,
    });
    total.indexed = true;
    let mut name = field("name", FieldType::Text);
    name.indexed = true;
    let orders = store
        .create_table_for_app(
            &c,
            NewDataTable {
                name: "orders".into(),
                schema: schema(vec![
                    name,
                    link(
                        "items",
                        lines.id,
                        LinkCardinality::ManyToMany,
                        Some("orders"),
                    ),
                    total,
                ]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        orders.schema.field("total").unwrap().materialization,
        Some(Materialization::Invariant)
    );
    let ids = |v: &[Uuid]| Value::Array(v.iter().map(|u| json!(u.to_string())).collect());
    let id = store
        .start_bulk_import(
            &c,
            orders.id,
            rows(vec![
                json!({"name": "o1", "items": ids(&line_ids[0..2])}),
                json!({"name": "o2", "items": ids(&line_ids[2..3])}),
                json!({"name": "o3", "items": ids(&line_ids[1..4])}),
                json!({"name": "o4"}),
            ]),
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    detach_job(&pool, id).await;
    let done = store.run_bulk_import(&tenant, orders.id, id).await.unwrap();
    assert_eq!(done.status, "done");
    // 材料化した値（索引行）を本体の名前で引く。
    let (ti, tb) = keys_of(&pool, &tenant, orders.id).await;
    let total_id = i16::try_from(orders.schema.field("total").unwrap().id).unwrap();
    let mat: HashMap<String, String> = sqlx::query_as::<_, (String, String)>(
        "SELECT r.data->>$4, i.val::text FROM data_index_num i \
         JOIN data_record r ON r.tenant_int = i.tenant_int AND r.table_int = i.table_int \
          AND r.record_int = i.record_int \
         WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(total_id)
    .bind(format!("f{}", orders.schema.field("name").unwrap().id))
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .collect();
    assert_eq!(mat.get("o1").map(String::as_str), Some("350"));
    assert_eq!(mat.get("o2").map(String::as_str), Some("300"));
    assert_eq!(mat.get("o3").map(String::as_str), Some("600"));
    let by_total = DataQuery {
        sort: vec![SortKey {
            field: "total".into(),
            descending: true,
        }],
        ..DataQuery::default()
    };
    let names: Vec<Value> = store
        .run_query(&c, orders.id, &by_total, None)
        .await
        .unwrap()
        .items
        .unwrap()
        .into_iter()
        .map(|r| r.data["name"].clone())
        .collect();
    assert_eq!(names[..3], [json!("o3"), json!("o1"), json!("o2")]);
    // 参照先の更新で一括取込の行も再計算待ちに載る（依存登録が一括経路でも効く）。
    let l2 = store
        .get_record(&c, lines.id, line_ids[1], None)
        .await
        .unwrap();
    store
        .update_record(
            &c,
            lines.id,
            line_ids[1],
            json!({"amount": 1}),
            l2.rev,
            None,
        )
        .await
        .unwrap();
    store.recompute_derived(&tenant, orders.id).await.unwrap();
    let o1 = store
        .run_query(
            &c,
            orders.id,
            &DataQuery {
                filter: Some(eq("name", json!("o1"))),
                ..DataQuery::default()
            },
            None,
        )
        .await
        .unwrap()
        .items
        .unwrap();
    assert_eq!(o1[0].data["total"], json!(101));

    // 一対一リンク（対称列つき）: 取込内で同じ参照先・既存の参照先・単一値の側の 2 件は不合格。
    let seats = store
        .create_table_for_app(
            &c,
            NewDataTable {
                name: "seats".into(),
                schema: schema(vec![field("no", FieldType::Text)]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    let mut seat_ids = Vec::new();
    for i in 0..4 {
        seat_ids.push(
            store
                .create_record(&c, seats.id, json!({"no": format!("s{i}")}), None)
                .await
                .unwrap()
                .id,
        );
    }
    let people = store
        .create_table_for_app(
            &c,
            NewDataTable {
                name: "people".into(),
                schema: schema(vec![
                    field("who", FieldType::Text),
                    link("seat", seats.id, LinkCardinality::OneToOne, Some("person")),
                ]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    store
        .create_record(
            &c,
            people.id,
            json!({"who": "pre", "seat": ids(&seat_ids[0..1])}),
            None,
        )
        .await
        .unwrap();
    let id = store
        .start_bulk_import(
            &c,
            people.id,
            rows(vec![
                json!({"who": "a", "seat": ids(&seat_ids[1..2])}),
                json!({"who": "b", "seat": ids(&seat_ids[1..2])}), // 取込内の重複
                json!({"who": "c", "seat": ids(&seat_ids[0..1])}), // 既存の参照先
                json!({"who": "d", "seat": ids(&seat_ids[2..4])}), // 単一値の側に 2 件
                json!({"who": "e", "seat": ids(&seat_ids[3..4])}),
            ]),
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    detach_job(&pool, id).await;
    store.run_bulk_import(&tenant, people.id, id).await.unwrap();
    let st = store.bulk_import_status(&c, people.id, id).await.unwrap();
    assert_eq!((st.rows_applied, st.rows_invalid), (2, 3), "{st:?}");
    let errs = store
        .bulk_import_errors(&c, people.id, id, 0, 10)
        .await
        .unwrap();
    assert_eq!(
        errs.iter().map(|e| e.row_no).collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert!(errs[0].message.contains("カーディナリティ"), "{errs:?}");
    assert!(errs[1].message.contains("カーディナリティ"), "{errs:?}");
    // 逆方向（対称列）も同じ Tx で書かれている。
    let seat3 = store
        .get_record(&c, seats.id, seat_ids[3], None)
        .await
        .unwrap();
    assert_eq!(seat3.data["person"].as_array().map(Vec::len), Some(1));
    assert_eq!(st.parity_mismatches, Some(0));
}

fn p95(mut v: Vec<Duration>) -> f64 {
    v.sort();
    let i = ((v.len() as f64) * 0.95).ceil() as usize;
    v[i.clamp(1, v.len()) - 1].as_secs_f64() * 1000.0
}

/// 他テナントの読取を回し続け、レイテンシを集める。
async fn reader_loop(
    store: DataStore,
    c: AuthContext,
    table: Uuid,
    stop: Arc<AtomicBool>,
) -> Vec<Duration> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while !stop.load(Ordering::SeqCst) {
        let q = DataQuery {
            filter: Some(eq(
                "status",
                json!(["draft", "submitted", "approved"][i % 3]),
            )),
            sort: vec![SortKey {
                field: "amount".into(),
                descending: i.is_multiple_of(2),
            }],
            limit: Some(50),
            ..DataQuery::default()
        };
        let s = Instant::now();
        store.run_query(&c, table, &q, None).await.unwrap();
        out.push(s.elapsed());
        i += 1;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    out
}

/// 10 万行の取込の所定時間（Task 15.11 受け入れ条件・定義と根拠は `docs/data/benchmarks.md` §3）。
const IMPORT_100K_LIMIT: Duration = Duration::from_mins(2);

/// 取込中の他テナントの読取 p95 が「悪化しない」の許容（相対 25% ＋ 絶対 2ms・根拠は
/// `docs/data/benchmarks.md` §3）。同じ 4 vCPU の上で DB・取込・読取が CPU を分け合う計測環境の揺れを許す幅。
fn p95_not_worse(base: f64, during: f64) -> bool {
    during <= base * 1.25 + 2.0
}

/// 計測: 10 万行の取込の所要時間（索引の同期／遅延構築）と、取込中の他テナントの読取 p95。
/// 重いので `DATA_BENCH=1` のときだけ走る。読取と取込が同じスレッドを奪い合わないよう（本番の
/// ワーカーも多スレッドのランタイムで別タスク）、マルチスレッドのランタイムで測る。
/// 所定時間（[`IMPORT_100K_LIMIT`]）内に終わることと、他テナントの p95 が悪化しないこと（[`p95_not_worse`]）を
/// 索引の同期・遅延構築の両方で確かめる。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_100k_import() {
    if std::env::var("DATA_BENCH").as_deref() != Ok("1") {
        eprintln!("DATA_BENCH=1 でないためスキップ");
        return;
    }
    let _g = serial().await;
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    // 他テナントの業務テーブル（2 万行）。
    let other_tenant = unique_tenant();
    let oc = ctx(&other_tenant, "alice");
    let ot = create_table(&store, &oc, "other", rich_schema()).await;
    let seed = store
        .start_bulk_import(
            &oc,
            ot.id,
            rows((0..20_000).map(|i| rich_row(i, None)).collect()),
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    detach_job(&pool, seed).await;
    store
        .run_bulk_import(&other_tenant, ot.id, seed)
        .await
        .unwrap();
    let n = 100_000;
    for mode in [BulkIndexMode::Immediate, BulkIndexMode::Deferred] {
        // 取込なしの基準 p95。
        let stop = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(reader_loop(
            store.clone(),
            oc.clone(),
            ot.id,
            Arc::clone(&stop),
        ));
        tokio::time::sleep(Duration::from_secs(8)).await;
        stop.store(true, Ordering::SeqCst);
        let base = reader.await.unwrap();

        let tenant = unique_tenant();
        let c = ctx(&tenant, "alice");
        let t = create_table(&store, &c, "big", rich_schema()).await;
        let data: Vec<Value> = (0..n).map(|i| rich_row(i, None)).collect();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(reader_loop(
            store.clone(),
            oc.clone(),
            ot.id,
            Arc::clone(&stop),
        ));
        let t0 = Instant::now();
        let id = store
            .start_bulk_import(
                &c,
                t.id,
                rows(data),
                BulkImportOptions {
                    index_mode: mode,
                    ..BulkImportOptions::default()
                },
                None,
            )
            .await
            .unwrap();
        detach_job(&pool, id).await;
        let staged = t0.elapsed();
        let mut phase: BTreeMap<String, Duration> = BTreeMap::new();
        let mut last = Instant::now();
        let mut current = "validating".to_string();
        loop {
            let s = store
                .bulk_import_step(&tenant, t.id, id, data::BULK_CHUNK)
                .await
                .unwrap();
            *phase.entry(current.clone()).or_default() += last.elapsed();
            last = Instant::now();
            current = s.status.clone();
            if s.done {
                break;
            }
        }
        let total = t0.elapsed();
        stop.store(true, Ordering::SeqCst);
        let during = reader.await.unwrap();
        let st = store.bulk_import_status(&c, t.id, id).await.unwrap();
        assert_eq!(st.status, "done");
        assert_eq!(st.rows_applied, n as i64);
        assert_eq!(st.parity_mismatches, Some(0));
        eprintln!(
            "| {mode:?} | {n} 行 | 合計 {:.1}s（COPY {:.1}s・段の内訳 {:?}） | 他テナント p95 {:.1}ms → {:.1}ms（{} / {} 回） |",
            total.as_secs_f64(),
            staged.as_secs_f64(),
            phase
                .iter()
                .map(|(k, v)| format!("{k}: {:.1}s", v.as_secs_f64()))
                .collect::<Vec<_>>(),
            p95(base.clone()),
            p95(during.clone()),
            base.len(),
            during.len()
        );
        assert!(
            total <= IMPORT_100K_LIMIT,
            "{mode:?}: 10 万行の取込が所定時間 {IMPORT_100K_LIMIT:?} を超えた: {total:?}"
        );
        let (b95, d95) = (p95(base), p95(during));
        assert!(
            p95_not_worse(b95, d95),
            "{mode:?}: 取込中に他テナントの読取 p95 が悪化した: {b95:.1}ms → {d95:.1}ms"
        );
    }
}
