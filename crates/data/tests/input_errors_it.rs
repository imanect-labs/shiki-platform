//! 入力の誤りの構造化（400 の `code`・安全な文言・列の参照名・検証 C-P2-4 / A-3 / A-9 / D-8 / D-9 / D-12 /
//! A-2 / C P3）。実 Postgres（`STORAGE_TEST_DATABASE_URL`）で実行する。
//!
//! - NUL（U+0000）を含む入力は 500 ではなく `invalid_input`（レコード・テーブル名・絞り込み・検索・SQL・取込）
//! - 不正なカーソルは `cursor_invalid`・`offset` は `offset_not_supported`・スナップショット＋並べ替えは
//!   `snapshot_unsupported`・索引の無い列は `field_not_indexed`
//! - 日付の列に日付でない比較値は `value_type_mismatch`（文字列比較で誤った結果を黙って返さない）
//! - テキストとテーブル名の上限は**文字数**（日本語 10,000 字は通り、10,001 字は `value_too_long`）
//! - 紀元前・5 桁の年・閏秒は `value_out_of_range`
//! - `limit: 0`（件数だけでない）は `invalid_input`・最上位の未知のキーは `invalid_input`（`field` にそのキー）
//! - 同名テーブルは 409 `name_taken`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use common::{create_table, ctx, field, schema, setup, store_with, unique_tenant};
use data::{
    CondOp, Condition, DataError, DataQuery, FieldType, InvalidCode, NewDataTable, SortKey,
    SqlRequest,
};
use serde_json::json;

fn code_of(r: &Result<impl std::fmt::Debug, DataError>) -> Option<InvalidCode> {
    r.as_ref().err().and_then(DataError::invalid_code)
}

fn field_of(e: &DataError) -> Option<String> {
    match e {
        DataError::InvalidInput(ie) => ie.field_key().map(str::to_string),
        _ => None,
    }
}

#[tokio::test]
async fn input_errors_are_structured_with_safe_codes() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let c = ctx(&unique_tenant(), "alice");
    let mut day = field("day", FieldType::Date);
    day.indexed = true;
    let mut at = field("at", FieldType::DateTime);
    at.indexed = true;
    let mut title = field("title", FieldType::Text);
    title.searchable = true;
    let t = create_table(
        &store,
        &c,
        "入力の誤り",
        schema(vec![title, field("memo", FieldType::Text), day, at]),
    )
    .await;
    store
        .create_record(&c, t.id, json!({"title": "a", "day": "2025-01-05"}), None)
        .await
        .unwrap();

    // NUL（U+0000）: レコード・テーブル名・絞り込み・検索・SQL。500 にしない。
    let r = store
        .create_record(&c, t.id, json!({"title": "a\u{0}b"}), None)
        .await;
    assert_eq!(code_of(&r), Some(InvalidCode::InvalidInput), "{r:?}");
    assert_eq!(field_of(r.as_ref().unwrap_err()).as_deref(), Some("title"));
    let r = store
        .create_table(
            &c,
            NewDataTable {
                name: "x\u{0}y".into(),
                schema: schema(vec![field("a", FieldType::Text)]),
            },
            None,
        )
        .await;
    assert_eq!(code_of(&r), Some(InvalidCode::InvalidInput), "{r:?}");
    let q = DataQuery {
        filter: Some(Condition::cmp("day", CondOp::Eq, Some(json!("2025\u{0}")))),
        ..DataQuery::default()
    };
    assert_eq!(
        code_of(&store.run_query(&c, t.id, &q, None).await),
        Some(InvalidCode::InvalidInput)
    );
    let q = DataQuery {
        search: Some("a\u{0}".into()),
        ..DataQuery::default()
    };
    assert_eq!(
        code_of(&store.run_query(&c, t.id, &q, None).await),
        Some(InvalidCode::InvalidInput)
    );
    let r = store
        .run_sql(
            &c,
            &SqlRequest {
                sql: "SELECT title FROM \"入力の誤り\" WHERE title = 'a\u{0}'".into(),
                cursor: None,
                with_count: false,
            },
            None,
        )
        .await;
    assert_eq!(code_of(&r), Some(InvalidCode::InvalidInput), "{r:?}");

    // 取込（JSON・CSV）の NUL も開始で 400。
    let r = store
        .start_bulk_import(
            &c,
            t.id,
            vec![data::BulkImportRow {
                key: None,
                data: json!({"title": "x\u{0}"}),
            }],
            data::BulkImportOptions::default(),
            None,
        )
        .await;
    assert_eq!(code_of(&r), Some(InvalidCode::InvalidInput), "{r:?}");
    let r = store
        .start_bulk_import_csv(
            &c,
            t.id,
            "title\nx\u{0}y\n".as_bytes(),
            data::BulkImportOptions::default(),
            None,
        )
        .await;
    assert!(r.as_ref().is_err_and(DataError::is_invalid), "{r:?}");

    // カーソル・offset・スナップショット＋並べ替え・索引の無い列。
    let q = DataQuery {
        cursor: Some("not-a-cursor".into()),
        ..DataQuery::default()
    };
    assert_eq!(
        code_of(&store.run_query(&c, t.id, &q, None).await),
        Some(InvalidCode::CursorInvalid)
    );
    let q = DataQuery {
        offset: Some(10),
        ..DataQuery::default()
    };
    assert_eq!(
        code_of(&store.run_query(&c, t.id, &q, None).await),
        Some(InvalidCode::OffsetNotSupported)
    );
    let q = DataQuery {
        snapshot: true,
        sort: vec![SortKey {
            field: "day".into(),
            descending: false,
        }],
        ..DataQuery::default()
    };
    assert_eq!(
        code_of(&store.run_query(&c, t.id, &q, None).await),
        Some(InvalidCode::SnapshotUnsupported)
    );
    let q = DataQuery {
        filter: Some(Condition::cmp("memo", CondOp::Eq, Some(json!("x")))),
        ..DataQuery::default()
    };
    let r = store.run_query(&c, t.id, &q, None).await;
    assert_eq!(code_of(&r), Some(InvalidCode::FieldNotIndexed), "{r:?}");
    assert_eq!(field_of(r.as_ref().unwrap_err()).as_deref(), Some("memo"));

    // 日付・日時の列に日付でない比較値（A-9）。
    for (f, v) in [("day", "3月"), ("day", "2025/01/05"), ("at", "2025-01-05")] {
        let q = DataQuery {
            filter: Some(Condition::cmp(f, CondOp::Gte, Some(json!(v)))),
            ..DataQuery::default()
        };
        let r = store.run_query(&c, t.id, &q, None).await;
        assert_eq!(
            code_of(&r),
            Some(InvalidCode::ValueTypeMismatch),
            "{f}={v}: {r:?}"
        );
    }
    let q = DataQuery {
        filter: Some(Condition::cmp(
            "day",
            CondOp::Gte,
            Some(json!("2025-01-01")),
        )),
        ..DataQuery::default()
    };
    assert_eq!(
        store
            .run_query(&c, t.id, &q, None)
            .await
            .unwrap()
            .items
            .unwrap()
            .len(),
        1
    );

    // テキストの上限は文字数（A-3）。
    let ok = "あ".repeat(10_000);
    store
        .create_record(&c, t.id, json!({"memo": ok}), None)
        .await
        .unwrap();
    let r = store
        .create_record(&c, t.id, json!({"memo": "あ".repeat(10_001)}), None)
        .await;
    assert_eq!(code_of(&r), Some(InvalidCode::ValueTooLong), "{r:?}");
    // テーブル名の上限も文字数（A-2・128 字の日本語は通る）。
    let name128 = "長".repeat(128);
    create_table(
        &store,
        &c,
        &name128,
        schema(vec![field("a", FieldType::Text)]),
    )
    .await;
    let r = store
        .create_table(
            &c,
            NewDataTable {
                name: "長".repeat(129),
                schema: schema(vec![field("a", FieldType::Text)]),
            },
            None,
        )
        .await;
    assert_eq!(code_of(&r), Some(InvalidCode::ValueTooLong), "{r:?}");
    // 同名のテーブルは 409 name_taken（A-2）。
    let r = store
        .create_table(
            &c,
            NewDataTable {
                name: name128.clone(),
                schema: schema(vec![field("a", FieldType::Text)]),
            },
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::NameTaken)), "{r:?}");

    // 日付の範囲・閏秒（D-12）。
    for (f, v) in [("day", "-0001-01-01"), ("at", "2016-12-31T23:59:60Z")] {
        let r = store.create_record(&c, t.id, json!({ f: v }), None).await;
        assert_eq!(
            code_of(&r),
            Some(InvalidCode::ValueOutOfRange),
            "{f}={v}: {r:?}"
        );
    }
    store
        .create_record(
            &c,
            t.id,
            json!({"day": "0001-01-01", "at": "9999-12-31T00:00:00Z"}),
            None,
        )
        .await
        .unwrap();

    // limit 0（件数だけでない）と未知のキー（C P3）。
    let q = DataQuery {
        limit: Some(0),
        ..DataQuery::default()
    };
    let r = store.run_query(&c, t.id, &q, None).await;
    assert_eq!(code_of(&r), Some(InvalidCode::InvalidInput), "{r:?}");
    assert_eq!(field_of(r.as_ref().unwrap_err()).as_deref(), Some("limit"));
    let neg: DataQuery = serde_json::from_value(json!({"limit": -1})).unwrap();
    assert_eq!(
        code_of(&store.run_query(&c, t.id, &neg, None).await),
        Some(InvalidCode::InvalidInput)
    );
    let count_only = DataQuery {
        limit: Some(0),
        with_count: true,
        ..DataQuery::default()
    };
    assert!(store.run_query(&c, t.id, &count_only, None).await.is_ok());
    let e = DataQuery::from_request(json!({"limitt": 5})).unwrap_err();
    assert_eq!(e.invalid_code(), Some(InvalidCode::InvalidInput));
    assert_eq!(field_of(&e).as_deref(), Some("limitt"));
    assert!(DataQuery::from_request(json!({"limit": 5, "with_count": true})).is_ok());
}
