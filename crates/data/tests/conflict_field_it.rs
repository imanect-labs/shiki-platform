//! 409 `unique_violation` が違反した列を名指しする（参照名と表示名・値は含めない）。実 Postgres
//! （`STORAGE_TEST_DATABASE_URL`）で実行する。
//!
//! - unique 宣言の違反: 作成・更新のどちらでも、違反した列（複数の unique 列のうちの当たった方）を返す
//! - 一対一 / 一対多のリンクの参照先の一意の違反は `derived_it::link_cardinality_violations_are_422`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::many_single_char_names
)]

mod common;

use common::{create_table, ctx, field, schema, setup, store_with, unique_tenant};
use data::{DataError, FieldType};
use serde_json::json;

fn named(r: &Result<data::DataRecord, DataError>) -> (String, String, String) {
    match r {
        Err(DataError::UniqueViolation { field: Some(f) }) => {
            assert!(!f.link, "{r:?}");
            (f.key.clone(), f.display_name.clone(), f.message())
        }
        _ => panic!("列つきの unique_violation のはず: {r:?}"),
    }
}

#[tokio::test]
async fn unique_violations_name_the_column() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let c = ctx(&unique_tenant(), "alice");
    let mut code = field("code", FieldType::Text);
    code.unique = true;
    code.display_name = "社員番号".into();
    let mut email = field("email", FieldType::Text);
    email.unique = true;
    email.display_name = "メール".into();
    let t = create_table(
        &store,
        &c,
        "unique の列",
        schema(vec![field("title", FieldType::Text), code, email]),
    )
    .await;
    let a = store
        .create_record(
            &c,
            t.id,
            json!({"title": "a", "code": "E-100", "email": "a@example.com"}),
            None,
        )
        .await
        .unwrap();
    // 作成: 社員番号が重なる。
    let r = store
        .create_record(&c, t.id, json!({"title": "b", "code": "E-100"}), None)
        .await;
    let (key, display, message) = named(&r);
    assert_eq!((key.as_str(), display.as_str()), ("code", "社員番号"));
    assert!(message.contains("社員番号"), "{message}");
    assert!(!message.contains("E-100"), "値を含めない: {message}");
    // 作成: 社員番号は新しく、メールが重なる（当たった方の列を返す）。
    let r = store
        .create_record(
            &c,
            t.id,
            json!({"title": "c", "code": "E-200", "email": "a@example.com"}),
            None,
        )
        .await;
    assert_eq!(named(&r).0, "email");
    // 更新: 別の行の社員番号を既存の値にする。
    let b = store
        .create_record(&c, t.id, json!({"title": "d", "code": "E-300"}), None)
        .await
        .unwrap();
    let r = store
        .update_record(&c, t.id, b.id, json!({"code": "E-100"}), b.rev, None)
        .await;
    assert_eq!(named(&r).1, "社員番号");
    // 同じ行が自分の値を書き直すのは違反ではない。
    store
        .update_record(&c, t.id, a.id, json!({"code": "E-100"}), a.rev, None)
        .await
        .unwrap();
}
