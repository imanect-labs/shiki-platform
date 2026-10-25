//! 索引の自動昇格と降格のジョブ化（Phase 15 Task 15.6・data-platform §10・PIT-72）。
//!
//! - 2 万行未満のテーブルは、並べ替え・絞り込みから索引の有効化までがリクエストの中で同期に終わる
//! - テーブルあたりの索引列数は既定 20 列で、21 列目は owner の明示の承認を要する
//! - 大きなテーブルは裏でバックフィルし、完了までその列は「準備中」（クエリ対象にならない）。
//!   `building` 中の書込は完成後の索引に反映される（write-through）
//! - 大きなテーブルの降格はスキーマ改訂の Tx で索引行を消さず、ジョブで片付ける
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）。承認の検証だけ実 OpenFGA（`OPENFGA_TEST_URL`）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Relation};
use common::{create_table, ctx, field, keys_of, ledger, schema, setup, unique_tenant};
use data::{
    DataError, DataQuery, DataStore, FieldType, IndexStatus, ListRecordsOptions, RecordSort,
    RefResolver, SortKey, SYNC_INDEX_MAX_ROWS,
};
use serde_json::json;
use uuid::Uuid;

struct AllowRefs;

#[async_trait]
impl RefResolver for AllowRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(true)
    }
}

async fn fga() -> Option<Arc<dyn AuthzClient>> {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-data-promote-test-{}", Uuid::new_v4()),
    };
    Some(Arc::new(
        OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
            .await
            .unwrap(),
    ))
}

fn by(field: &str) -> DataQuery {
    DataQuery {
        sort: vec![SortKey {
            field: field.into(),
            descending: false,
        }],
        limit: Some(200),
        ..DataQuery::default()
    }
}

#[tokio::test]
async fn small_table_sort_enables_the_index_synchronously() {
    let Some(pool) = setup().await else { return };
    let Some(authz) = fga().await else { return };
    let tenant = unique_tenant();
    let (alice, bob) = (ctx(&tenant, "alice"), ctx(&tenant, "bob"));
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let table = create_table(
        &store,
        &alice,
        "small",
        schema(vec![
            field("title", FieldType::Text),
            field("memo", FieldType::Text),
        ]),
    )
    .await;
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &alice.ns().data_table(&table.id.to_string()),
        )
        .await
        .unwrap();
    for i in (0..60).rev() {
        store
            .create_record(
                &alice,
                table.id,
                json!({"title": format!("t{i}"), "memo": format!("m{i:03}")}),
                None,
            )
            .await
            .unwrap();
    }
    // v1 の一覧 API は従来どおり未索引を 422（自動昇格の契機はグリッドとビューの経路だけ）。
    let v1 = store
        .list_records(
            &bob,
            table.id,
            &ListRecordsOptions {
                sort: Some(RecordSort {
                    field: "memo".into(),
                    descending: false,
                }),
                ..Default::default()
            },
            None,
        )
        .await;
    assert!(matches!(&v1, Err(e) if e.is_invalid()), "{v1:?}");
    // チャット・ワークフロー・ミニアプリの経路（昇格の要求なし）は従来どおり拒否し、昇格もしない
    // （昇格の契機はビューの保存とグリッドの明示の要求だけ・data-platform §10・監査 B1-10）。
    let plain = store.run_query(&bob, table.id, &by("memo"), None).await;
    assert!(matches!(&plain, Err(e) if e.is_invalid()), "{plain:?}");
    let t = store.get_table(&alice, table.id, None).await.unwrap();
    assert!(
        !t.schema.field("memo").unwrap().indexed,
        "要求なしでは昇格しない"
    );
    // グリッドの並べ替え（viewer・昇格の明示の要求）: 403 / 422 にせず、その場で有効化して結果を返す。
    let t0 = Instant::now();
    let r = store
        .run_query_promoting(&bob, table.id, &by("memo"), None)
        .await
        .unwrap();
    eprintln!(
        "並べ替えから索引の有効化まで（60 行・同期）: {:?}",
        t0.elapsed()
    );
    let memos: Vec<String> = r
        .items
        .unwrap()
        .iter()
        .map(|x| x.data["memo"].as_str().unwrap().to_string())
        .collect();
    let mut sorted = memos.clone();
    sorted.sort();
    assert_eq!(memos, sorted);
    assert_eq!(memos.len(), 60);
    let t = store.get_table(&alice, table.id, None).await.unwrap();
    assert!(t.schema.field("memo").unwrap().indexed, "宣言に載る");
    let overview = store.table_indexes(&bob, table.id, None).await.unwrap();
    assert_eq!(
        overview
            .fields
            .iter()
            .find(|f| f.field == "memo")
            .unwrap()
            .status,
        IndexStatus::Active
    );
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    assert!(ledger(&pool, ti, tb)
        .await
        .iter()
        .all(|(_, _, s)| s == "active"));
    // 昇格は監査に残る。
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'data.index.promote'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn twenty_first_index_requires_explicit_owner_approval() {
    let Some(pool) = setup().await else { return };
    let Some(authz) = fga().await else { return };
    let tenant = unique_tenant();
    let (alice, bob) = (ctx(&tenant, "alice"), ctx(&tenant, "bob"));
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let mut fields: Vec<_> = (1..=20)
        .map(|i| {
            let mut f = field(&format!("c{i:02}"), FieldType::Text);
            f.indexed = true;
            f
        })
        .collect();
    fields.push(field("c21", FieldType::Text));
    fields.push(field("c22", FieldType::Text));
    // リンク・擬似列は数えない（record_ref は常に索引を持つ）。
    let table = create_table(&store, &alice, "wide", schema(fields)).await;
    authz
        .write_tuple(
            &bob.subject(),
            Relation::Editor,
            &alice.ns().data_table(&table.id.to_string()),
        )
        .await
        .unwrap();
    store
        .create_record(&alice, table.id, json!({"c21": "x"}), None)
        .await
        .unwrap();
    // 21 列目: 自動昇格は止まり、承認を求める（件数・値は出さない）。
    let r = store
        .run_query_promoting(&bob, table.id, &by("c21"), None)
        .await;
    match r {
        Err(DataError::IndexApprovalRequired {
            field,
            indexed,
            cap,
        }) => {
            assert_eq!((field.as_str(), indexed, cap), ("c21", 20, 20));
        }
        other => panic!("承認待ちになるはず: {other:?}"),
    }
    // 承認は owner だけ（editor の bob は 403）。
    assert!(matches!(
        store.promote_index(&bob, table.id, "c21", true, None).await,
        Err(DataError::Forbidden)
    ));
    let p = store
        .promote_index(&alice, table.id, "c21", true, None)
        .await
        .unwrap();
    assert_eq!(p.status, IndexStatus::Active);
    assert!(store
        .run_query_promoting(&bob, table.id, &by("c21"), None)
        .await
        .is_ok());
    // テナント設定で上限を上げれば承認なしで作れる。
    store
        .update_tenant_index_cap(&alice, Some(30), None)
        .await
        .unwrap();
    assert!(store
        .run_query_promoting(&bob, table.id, &by("c22"), None)
        .await
        .is_ok());
    let o = store.table_indexes(&bob, table.id, None).await.unwrap();
    assert_eq!((o.indexed_fields, o.max_indexed_fields), (22, 30));
}

/// 大きなテーブル（2 万行以上）: 裏でバックフィルし、完了までその列はクエリ対象にならない。
/// `building` 中の書込は完成後の索引に反映される。降格はジョブで片付ける。
#[tokio::test]
async fn large_table_promotes_in_background_with_write_through_and_demotes_by_job() {
    let Some(pool) = setup().await else { return };
    // 投入データの assignee（u{n}）を参照検証で弾かない。
    let store = DataStore::new(
        pool.clone(),
        Arc::new(common::AllowAll),
        Arc::new(AllowRefs),
    );
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let n = SYNC_INDEX_MAX_ROWS + 500;
    let table = common::bulk::seed(&pool, &store, &c, "big", n, false, false).await;
    // 未索引の列を足す（既存行は値を持たない）。
    let t = store.get_table(&c, table.id, None).await.unwrap();
    let mut s = t.schema.clone();
    s.fields.push(field("memo", FieldType::Text));
    store
        .update_table_schema(&c, table.id, s, None, None)
        .await
        .unwrap();
    // 並べ替えを要求すると準備中（バックフィルが積まれる）。
    let r = store
        .run_query_promoting(&c, table.id, &by("memo"), None)
        .await;
    let Err(DataError::IndexPreparing { field, progress }) = r else {
        panic!("準備中になるはず: {r:?}");
    };
    assert_eq!(field, "memo");
    assert!(progress.is_some_and(|p| p < 1.0), "{progress:?}");
    // 準備中の列はクエリに使わない（半端な索引で結果を欠落させない）。
    let filtered = DataQuery {
        filter: Some(data::Condition::cmp(
            "memo",
            data::CondOp::Eq,
            Some(json!("wt")),
        )),
        ..DataQuery::default()
    };
    assert!(matches!(
        store.run_query(&c, table.id, &filtered, None).await,
        Err(DataError::IndexPreparing { .. })
    ));
    // building 中の書込（バックフィル前後のどちらの領域にも）: 先頭と末尾の既存行の更新・新規作成。
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let edge: Vec<Uuid> = sqlx::query_scalar(
        "(SELECT id FROM data_record WHERE tenant_int = $1 AND table_int = $2 ORDER BY record_int LIMIT 1) \
         UNION ALL \
         (SELECT id FROM data_record WHERE tenant_int = $1 AND table_int = $2 ORDER BY record_int DESC LIMIT 1)",
    )
    .bind(ti)
    .bind(tb)
    .fetch_all(&pool)
    .await
    .unwrap();
    // バックフィルを途中まで進めてから書く（走査済み領域への書込が欠落しないことを見る）。
    store
        .index_backfill_step(&tenant, table.id, 1_000)
        .await
        .unwrap();
    for id in &edge {
        let rev: i64 = sqlx::query_scalar(
            "SELECT rev FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND id = $3",
        )
        .bind(ti)
        .bind(tb)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        store
            .update_record(&c, table.id, *id, json!({"memo": "wt"}), rev, None)
            .await
            .unwrap();
    }
    let created = store
        .create_record(&c, table.id, json!({"status": "完了", "memo": "wt"}), None)
        .await
        .unwrap();
    // ジョブでバックフィルを終える。
    while store.consume_index_job_once().await.unwrap() {}
    let r = store
        .run_query(&c, table.id, &filtered, None)
        .await
        .unwrap();
    let mut got: Vec<Uuid> = r.items.unwrap().iter().map(|x| x.id).collect();
    let mut want = edge.clone();
    want.push(created.id);
    got.sort();
    want.sort();
    assert_eq!(got, want, "building 中の書込が完成後の索引に入っている");
    let parity = store.verify_index_parity(&tenant, table.id).await.unwrap();
    assert!(parity.is_consistent(), "{parity:?}");

    // 降格: スキーマ改訂の Tx では行を消さず（台帳は即 absent）、ジョブが片付ける。
    let memo_id = i16::try_from(
        store
            .get_table(&c, table.id, None)
            .await
            .unwrap()
            .schema
            .field("memo")
            .unwrap()
            .id,
    )
    .unwrap();
    let rows = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM data_index_text WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3",
        )
        .bind(ti)
        .bind(tb)
        .bind(memo_id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(rows(pool.clone()).await, 3);
    let t = store.get_table(&c, table.id, None).await.unwrap();
    let mut s = t.schema.clone();
    s.fields
        .iter_mut()
        .find(|f| f.key == "memo")
        .unwrap()
        .indexed = false;
    store
        .update_table_schema(&c, table.id, s, None, None)
        .await
        .unwrap();
    assert!(
        ledger(&pool, ti, tb)
            .await
            .iter()
            .all(|(f, _, st)| *f != memo_id || st == "absent"),
        "台帳は改訂の Tx で absent"
    );
    assert_eq!(
        rows(pool.clone()).await,
        3,
        "行はまだ消えていない（ジョブ待ち）"
    );
    // 後片付けが終わるまで同じ列の再昇格は準備中。
    assert!(matches!(
        store.promote_index(&c, table.id, "memo", false, None).await,
        Err(DataError::IndexPreparing { .. })
    ));
    while store.consume_index_job_once().await.unwrap() {}
    assert_eq!(rows(pool.clone()).await, 0, "ジョブが片付けた");
    let p = store
        .promote_index(&c, table.id, "memo", false, None)
        .await
        .unwrap();
    assert_eq!(p.status, IndexStatus::Preparing, "大きいので再び裏で作る");
    while store.consume_index_job_once().await.unwrap() {}
    assert_eq!(rows(pool.clone()).await, 3);
}
