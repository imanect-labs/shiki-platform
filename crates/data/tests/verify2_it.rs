//! 2 回目の実操作検証の指摘（データの読み書き）の回帰テスト。実 Postgres（`STORAGE_TEST_DATABASE_URL`）で実行する。
//!
//! - 2-B-P1-1: `searchable` だけ（`indexed` でない）の列でも横断検索・`contains`・`ends_with` が使える
//! - 2-A-P1-1 ①: 値の無い行がある列の必須化・使われている選択肢の削除は `schema_conflicts_with_data`
//! - 2-A-P1-1 ②: 既存の不整合（ある列の必須の欠落）が別の列の更新を妨げない
//! - 2-A-P2-3: ユーザー・部署の列の比較値は ID だけ（表示名・存在しない ID は `value_type_mismatch`）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]

mod common;

use common::{create_table, ctx, field, schema, setup, store_with, unique_tenant};
use data::{CondOp, Condition, DataError, DataQuery, FieldType, InvalidCode};
use serde_json::json;

fn code_of(r: &Result<impl std::fmt::Debug, DataError>) -> Option<InvalidCode> {
    r.as_ref().err().and_then(DataError::invalid_code)
}

#[tokio::test]
async fn searchable_only_columns_can_be_searched() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let c = ctx(&unique_tenant(), "alice");
    let mut note = field("note", FieldType::Text);
    note.searchable = true; // indexed ではない
    let t = create_table(
        &store,
        &c,
        "検索だけ",
        schema(vec![field("title", FieldType::Text), note]),
    )
    .await;
    for n in ["りんごの箱", "みかん", "青りんご"] {
        store
            .create_record(&c, t.id, json!({"title": n, "note": n}), None)
            .await
            .unwrap();
    }
    let count = |q: DataQuery| {
        let store = &store;
        let c = &c;
        async move {
            store
                .run_query(c, t.id, &q, None)
                .await
                .map(|r| r.items.map_or(0, |v| v.len()))
        }
    };
    let n = count(DataQuery {
        search: Some("りんご".into()),
        ..DataQuery::default()
    })
    .await
    .unwrap();
    assert_eq!(n, 2);
    let n = count(DataQuery {
        search: Some("りんご".into()),
        search_fields: Some(vec!["note".into()]),
        ..DataQuery::default()
    })
    .await
    .unwrap();
    assert_eq!(n, 2);
    let n = count(DataQuery {
        filter: Some(Condition::cmp(
            "note",
            CondOp::Contains,
            Some(json!("みかん")),
        )),
        ..DataQuery::default()
    })
    .await
    .unwrap();
    assert_eq!(n, 1);
    let n = count(DataQuery {
        filter: Some(Condition::cmp(
            "note",
            CondOp::EndsWith,
            Some(json!("りんご")),
        )),
        ..DataQuery::default()
    })
    .await
    .unwrap();
    assert_eq!(n, 1);
    // 値索引の要る比較（等値・並べ替え）は従来どおり field_not_indexed。
    let r = count(DataQuery {
        filter: Some(Condition::cmp("note", CondOp::Eq, Some(json!("みかん")))),
        ..DataQuery::default()
    })
    .await;
    assert_eq!(code_of(&r), Some(InvalidCode::FieldNotIndexed), "{r:?}");
}

#[tokio::test]
async fn schema_changes_that_conflict_with_data_are_rejected() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let c = ctx(&unique_tenant(), "alice");
    let mut kind = field("kind", FieldType::Select);
    kind.options = vec!["a".into(), "b".into(), "c".into()];
    kind.display_name = "種別".into();
    let mut tags = field("tags", FieldType::MultiSelect);
    tags.options = vec!["x".into(), "y".into()];
    let mut memo = field("memo", FieldType::Text);
    memo.display_name = "メモ".into();
    let t = create_table(
        &store,
        &c,
        "改訂と既存データ",
        schema(vec![field("title", FieldType::Text), memo, kind, tags]),
    )
    .await;
    let r1 = store
        .create_record(
            &c,
            t.id,
            json!({"title": "1", "kind": "a", "tags": ["x"]}),
            None,
        )
        .await
        .unwrap();
    store
        .create_record(
            &c,
            t.id,
            json!({"title": "2", "memo": "m", "kind": "b"}),
            None,
        )
        .await
        .unwrap();
    let cur = store.get_table(&c, t.id, None).await.unwrap();
    let with = |f: &dyn Fn(&mut data::TableSchema)| {
        let mut s = cur.schema.clone();
        f(&mut s);
        s
    };
    // 値の無い行がある列を必須にする → 400（列の表示名と件数）。
    let r = store
        .update_table_schema(
            &c,
            t.id,
            with(&|s| {
                s.fields
                    .iter_mut()
                    .find(|f| f.key == "memo")
                    .unwrap()
                    .required = true
            }),
            None,
            None,
        )
        .await;
    assert_eq!(
        code_of(&r),
        Some(InvalidCode::SchemaConflictsWithData),
        "{r:?}"
    );
    let msg = r.unwrap_err().invalid_message().unwrap();
    assert!(msg.contains("メモ") && msg.contains("1 件"), "{msg}");
    // 使われている選択肢を外す → 400（単一選択・複数選択とも）。
    let r = store
        .update_table_schema(
            &c,
            t.id,
            with(&|s| {
                s.fields
                    .iter_mut()
                    .find(|f| f.key == "kind")
                    .unwrap()
                    .options = vec!["b".into(), "c".into()];
            }),
            None,
            None,
        )
        .await;
    assert_eq!(
        code_of(&r),
        Some(InvalidCode::SchemaConflictsWithData),
        "{r:?}"
    );
    let r = store
        .update_table_schema(
            &c,
            t.id,
            with(&|s| {
                s.fields
                    .iter_mut()
                    .find(|f| f.key == "tags")
                    .unwrap()
                    .options = vec!["y".into()];
            }),
            None,
            None,
        )
        .await;
    assert_eq!(
        code_of(&r),
        Some(InvalidCode::SchemaConflictsWithData),
        "{r:?}"
    );
    // 使われていない選択肢は外せる・値の揃った列は必須にできる。
    store
        .update_table_schema(
            &c,
            t.id,
            with(&|s| {
                s.fields
                    .iter_mut()
                    .find(|f| f.key == "kind")
                    .unwrap()
                    .options = vec!["a".into(), "b".into()];
                s.fields
                    .iter_mut()
                    .find(|f| f.key == "title")
                    .unwrap()
                    .required = true;
            }),
            None,
            None,
        )
        .await
        .unwrap();
    // 値を入れれば必須にできる。
    store
        .update_record(&c, t.id, r1.id, json!({"memo": "入れた"}), r1.rev, None)
        .await
        .unwrap();
    let cur = store.get_table(&c, t.id, None).await.unwrap();
    let mut s = cur.schema.clone();
    s.fields
        .iter_mut()
        .find(|f| f.key == "memo")
        .unwrap()
        .required = true;
    store
        .update_table_schema(&c, t.id, s, None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn existing_inconsistency_does_not_block_editing_other_columns() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let c = ctx(&unique_tenant(), "alice");
    let t = create_table(
        &store,
        &c,
        "既存の不整合",
        schema(vec![
            field("title", FieldType::Text),
            field("memo", FieldType::Text),
        ]),
    )
    .await;
    let r = store
        .create_record(&c, t.id, json!({"title": "a"}), None)
        .await
        .unwrap();
    // 改訂の検査より前に作られた不整合を再現する（memo を必須にした定義を直接書く）。
    sqlx::query(
        "UPDATE data_table SET schema = jsonb_set(schema, '{fields,1,required}', 'true'::jsonb), \
         schema_version = schema_version + 1 WHERE id = $1",
    )
    .bind(t.id)
    .execute(&pool)
    .await
    .unwrap();
    // 別の列（title）の更新は通る（memo の欠落は title の編集を妨げない）。
    let u = store
        .update_record(&c, t.id, r.id, json!({"title": "b"}), r.rev, None)
        .await
        .unwrap();
    assert_eq!(u.data["title"], json!("b"));
    // 必須の列そのものを消す更新は従来どおり required_missing（列名つき）。
    let e = store
        .update_record(
            &c,
            t.id,
            r.id,
            json!({"memo": null, "title": "c"}),
            u.rev,
            None,
        )
        .await;
    assert_eq!(code_of(&e), Some(InvalidCode::RequiredMissing), "{e:?}");
}

#[tokio::test]
async fn user_and_role_filters_take_ids_only() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let c = ctx(&unique_tenant(), "alice");
    let mut who = field("who", FieldType::UserRef);
    who.indexed = true;
    let mut dept = field("dept", FieldType::RoleRef);
    dept.indexed = true;
    let t = create_table(&store, &c, "担当と部署", schema(vec![who, dept])).await;
    store
        .create_record(&c, t.id, json!({"who": "alice", "dept": "sales"}), None)
        .await
        .unwrap();
    let run = |f: &str, op: CondOp, v: serde_json::Value| {
        let store = &store;
        let c = &c;
        let q = DataQuery {
            filter: Some(Condition::cmp(f, op, Some(v))),
            ..DataQuery::default()
        };
        async move {
            store
                .run_query(c, t.id, &q, None)
                .await
                .map(|r| r.items.map_or(0, |v| v.len()))
        }
    };
    assert_eq!(run("who", CondOp::Eq, json!("alice")).await.unwrap(), 1);
    assert_eq!(run("dept", CondOp::Eq, json!("sales")).await.unwrap(), 1);
    for (f, op, v) in [
        ("who", CondOp::Eq, json!("山田 太郎")),
        ("who", CondOp::In, json!(["alice", "nobody"])),
        ("who", CondOp::Neq, json!("nobody")),
        ("dept", CondOp::Eq, json!("営業部")),
    ] {
        let r = run(f, op, v.clone()).await;
        assert_eq!(
            code_of(&r),
            Some(InvalidCode::ValueTypeMismatch),
            "{f} {v}: {r:?}"
        );
    }
}

/// 2-D-P2-1: 取込の完了時に、行が入ったパーティションの Postgres の統計を取り直す（取込の前の「空に近い」
/// 統計のまま候補集めが値索引を使わない計画にならない）。
#[tokio::test]
async fn bulk_import_refreshes_partition_statistics() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let t = create_table(&store, &c, "取込と統計", schema(vec![amount])).await;
    let (ti, tb) = common::keys_of(&pool, &tenant, t.id).await;
    let started: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let rows: Vec<data::BulkImportRow> = (0..1_200)
        .map(|i| data::BulkImportRow {
            key: None,
            data: json!({ "amount": i }),
        })
        .collect();
    let id = store
        .start_bulk_import(&c, t.id, rows, data::BulkImportOptions::default(), None)
        .await
        .unwrap();
    let st = store.run_bulk_import(&tenant, t.id, id).await.unwrap();
    assert_eq!(st.status, "done");
    let rel: String = sqlx::query_scalar(
        "SELECT tableoid::regclass::text FROM data_index_num WHERE tenant_int = $1 AND table_int = $2 LIMIT 1",
    )
    .bind(ti)
    .bind(tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    let analyzed: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_analyze FROM pg_stat_user_tables WHERE relname = $1")
            .bind(&rel)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        analyzed.is_some_and(|a| a >= started),
        "{rel}: {analyzed:?} < {started}"
    );
}
