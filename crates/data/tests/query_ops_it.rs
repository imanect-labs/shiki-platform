//! 運用まわりの受け入れ条件（Task 15.3）。
//!
//! - **件数の上限はテナント設定**（data-platform §5.4）: 既定 10,000・テナントごとに変更でき、他テナントに効かない
//! - **索引がバックフィル中の列は「準備中」**（data-platform §10: building の間は駆動にもフィルタにも使わせない）
//! - **一意性に空白期間を作らない**（0084 で旧方式の一意な式インデックスを撤去した後も）:
//!   0083 の状態でバックフィル途中だったテーブルに 0084 / 0085 を当てても、重複は 409 になる

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::many_single_char_names
)]

mod common;

use common::{create_table, ctx, field, keys_of, schema, setup, store_with, unique_tenant};
use data::{CondOp, Condition, CountResult, DataError, DataQuery, FieldType};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
async fn count_cap_is_a_tenant_setting() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let a = ctx(&unique_tenant(), "alice");
    let b = ctx(&unique_tenant(), "alice");
    let mut st = field("status", FieldType::Select);
    st.indexed = true;
    st.options = vec!["x".into()];
    let ta = create_table(&store, &a, "cap", schema(vec![st.clone()])).await;
    let tb = create_table(&store, &b, "cap", schema(vec![st])).await;
    for _ in 0..6 {
        store
            .create_record(&a, ta.id, json!({"status": "x"}), None)
            .await
            .unwrap();
        store
            .create_record(&b, tb.id, json!({"status": "x"}), None)
            .await
            .unwrap();
    }
    let q = DataQuery {
        filter: Some(Condition::cmp("status", CondOp::Eq, Some(json!("x")))),
        with_count: true,
        limit: Some(1),
        ..Default::default()
    };
    // 既定（10,000）。
    let s = store.tenant_settings(&a).await.unwrap();
    assert_eq!(
        (s.count_cap, s.effective_count_cap),
        (None, data::DEFAULT_COUNT_CAP)
    );
    let r = store.run_query(&a, ta.id, &q, None).await.unwrap();
    assert_eq!(r.count, Some(CountResult::Exact(6)));
    // テナント A だけ上限 4 に。
    let s = store
        .update_tenant_count_cap(&a, Some(4), None)
        .await
        .unwrap();
    assert_eq!(s.effective_count_cap, 4);
    let r = store.run_query(&a, ta.id, &q, None).await.unwrap();
    assert_eq!(r.count, Some(CountResult::AtLeast(4)));
    let r = store.run_query(&b, tb.id, &q, None).await.unwrap();
    assert_eq!(
        r.count,
        Some(CountResult::Exact(6)),
        "他テナントには効かない"
    );
    // 範囲外は 422、既定へ戻せる。
    assert!(matches!(
        store.update_tenant_count_cap(&a, Some(0), None).await,
        Err(DataError::Invalid(_))
    ));
    store.update_tenant_count_cap(&a, None, None).await.unwrap();
    let r = store.run_query(&a, ta.id, &q, None).await.unwrap();
    assert_eq!(r.count, Some(CountResult::Exact(6)));
    // 変更は監査に残る。
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'data.tenant_settings.update'",
    )
    .bind(&a.tenant_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 2);
}

#[tokio::test]
async fn building_index_on_a_requested_field_is_not_ready() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let c = ctx(&unique_tenant(), "alice");
    let mut st = field("status", FieldType::Select);
    st.indexed = true;
    st.options = vec!["x".into(), "y".into()];
    let mut other = field("other", FieldType::Text);
    other.indexed = true;
    let t = create_table(&store, &c, "building", schema(vec![st, other])).await;
    store
        .create_record(&c, t.id, json!({"status": "x"}), None)
        .await
        .unwrap();
    let (ti, tb) = keys_of(&pool, &c.tenant_id, t.id).await;
    let sid = i16::try_from(t.schema.field("status").unwrap().id).unwrap();
    sqlx::query(
        "UPDATE data_index_state SET state = 'building' \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(sid)
    .execute(&pool)
    .await
    .unwrap();
    for q in [
        DataQuery {
            filter: Some(Condition::cmp("status", CondOp::Eq, Some(json!("x")))),
            ..Default::default()
        },
        DataQuery {
            sort: vec![data::SortKey {
                field: "status".into(),
                descending: false,
            }],
            ..Default::default()
        },
    ] {
        let r = store.run_query(&c, t.id, &q, None).await;
        // 準備中は 409 の構造化された理由（列と進捗・Task 15.6）。
        assert!(
            matches!(r, Err(DataError::IndexPreparing { ref field, .. }) if field == "status"),
            "{r:?}"
        );
        assert!(r.unwrap_err().to_string().contains("準備中"));
    }
    // 準備中の列を使わないクエリは通る（他の列・初期グリッド）。
    let r = store
        .run_query(&c, t.id, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(r.items.unwrap().len(), 1);
}

/// 0083 の状態（旧方式の一意な式インデックスがまだ一意性を守り、data_unique はバックフィル途中）から
/// 0084（旧方式の撤去）・0085（一意索引の作り直し）を当てても、一意性は途切れない。
#[tokio::test]
async fn uniqueness_survives_the_upgrade_from_a_mid_backfill_state() {
    let Some(admin) = setup().await else { return };
    let db = format!("shiki_upg_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {db}"))
        .execute(&admin)
        .await
        .unwrap();
    let base = std::env::var("STORAGE_TEST_DATABASE_URL").unwrap();
    let url = format!("{}/{db}", base.rsplit_once('/').unwrap().0);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    // 0084 / 0085 の手前の状態を作る（0083 まで＋ 0086 以降の独立した追加テーブル。
    // テストの書込は現行コードで行うため、0084 / 0085 と無関係な新しいテーブルは先に置く）。
    let full = sqlx::migrate!("../../migrations");
    let mut before = sqlx::migrate!("../../migrations");
    before.migrations = full
        .migrations
        .iter()
        .filter(|m| !(84..=85).contains(&m.version))
        .cloned()
        .collect::<Vec<_>>()
        .into();
    before.run(&pool).await.unwrap();

    let store = store_with(pool.clone());
    let c = ctx(&unique_tenant(), "alice");
    let mut code = field("code", FieldType::Text);
    code.unique = true;
    let t = create_table(
        &store,
        &c,
        "upg",
        schema(vec![code, field("memo", FieldType::Text)]),
    )
    .await;
    for i in 0..5 {
        store
            .create_record(&c, t.id, json!({"code": format!("C-{i}")}), None)
            .await
            .unwrap();
    }
    // バックフィル途中の再現: 一意索引の台帳は building、data_unique はまだ空。
    let (ti, tb) = keys_of(&pool, &c.tenant_id, t.id).await;
    sqlx::query("DELETE FROM data_unique WHERE tenant_int = $1 AND table_int = $2")
        .bind(ti)
        .bind(tb)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE data_index_state SET state = 'building' \
         WHERE tenant_int = $1 AND table_int = $2 AND index_table = 4",
    )
    .bind(ti)
    .bind(tb)
    .execute(&pool)
    .await
    .unwrap();

    // 0084 / 0085 を当てる。
    full.run(&pool).await.unwrap();

    // 重複は 409（初回アクセスで台帳を作り直し、unique を同期構築してから書込を通す）。
    let dup = store
        .create_record(&c, t.id, json!({"code": "C-3"}), None)
        .await;
    assert!(matches!(dup, Err(DataError::UniqueViolation)), "{dup:?}");
    // 重複でなければ書ける。一意索引は active。
    store
        .create_record(&c, t.id, json!({"code": "C-9"}), None)
        .await
        .unwrap();
    let state: String = sqlx::query_scalar(
        "SELECT state FROM data_index_state WHERE tenant_int = $1 AND table_int = $2 AND index_table = 4",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "active");
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_unique WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 6, "既存 5 行＋新規 1 行が一意索引に載る");

    // 書込経路の保険: 一意索引が active でなければ書込を通さない（0085 を経ない経路でも空白期間を作らない）。
    sqlx::query(
        "UPDATE data_index_state SET state = 'building' \
         WHERE tenant_int = $1 AND table_int = $2 AND index_table = 4",
    )
    .bind(ti)
    .bind(tb)
    .execute(&pool)
    .await
    .unwrap();
    let r = store
        .create_record(&c, t.id, json!({"code": "C-10"}), None)
        .await;
    assert!(
        matches!(r, Err(DataError::Conflict(ref m)) if m.contains("一意")),
        "{r:?}"
    );

    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {db} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
}

/// 集計クエリは監査に残る（PIT-17: 反復差分の検知可能性・監査 B1-3）。IR 直投入と SQL 前段面の両方。
/// 集計しない読取は監査しない（読取ごとに監査を積まない）。
#[tokio::test]
async fn aggregate_queries_are_audited() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let a = ctx(&unique_tenant(), "alice");
    let mut st = field("status", FieldType::Select);
    st.indexed = true;
    st.options = vec!["x".into(), "y".into()];
    let t = create_table(&store, &a, "agg_audit", schema(vec![st])).await;
    for s in ["x", "x", "y"] {
        store
            .create_record(&a, t.id, json!({ "status": s }), None)
            .await
            .unwrap();
    }
    let audits = |pool: sqlx::PgPool, tenant: String| async move {
        sqlx::query_as::<_, (String, String, String, serde_json::Value, Option<String>)>(
            "SELECT actor, object_type, object_id, metadata, trace_id FROM audit_log \
             WHERE tenant_id = $1 AND action = 'data.query.aggregate' ORDER BY id",
        )
        .bind(tenant)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    // 集計しない読取は監査に残さない。
    store
        .run_query(&a, t.id, &DataQuery::default(), None)
        .await
        .unwrap();
    assert!(audits(pool.clone(), a.tenant_id.clone()).await.is_empty());

    let q = DataQuery {
        group_by: vec!["status".into()],
        aggregate: vec![data::MetricSpec {
            metric: data::Metric::Count,
            field: None,
        }],
        ..Default::default()
    };
    store
        .run_query(&a, t.id, &q, Some("trace-agg"))
        .await
        .unwrap();
    let rows = audits(pool.clone(), a.tenant_id.clone()).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (actor, otype, oid, meta, trace) = &rows[0];
    assert_eq!(actor, "alice");
    assert_eq!(otype, "data_table");
    assert_eq!(oid, &t.id.to_string());
    assert_eq!(meta["group_by"], json!(["status"]));
    assert_eq!(trace.as_deref(), Some("trace-agg"));

    // SQL 前段面の集計も同じ経路で監査に残る。
    store
        .run_sql(
            &a,
            &data::SqlRequest {
                sql: "SELECT status, count(*) FROM agg_audit GROUP BY status".into(),
                cursor: None,
                with_count: false,
            },
            None,
        )
        .await
        .unwrap();
    let rows = audits(pool.clone(), a.tenant_id.clone()).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1].2, t.id.to_string());
}
