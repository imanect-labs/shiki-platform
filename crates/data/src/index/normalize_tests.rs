#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::TimeZone;
use serde_json::json;

use super::*;
use crate::model::{FieldDef, FieldType};

fn field(ty: FieldType) -> FieldDef {
    let mut f = FieldDef::new("f", ty);
    f.id = 16;
    f
}

#[test]
fn prefix_cuts_at_char_boundary_within_limit() {
    let s = "あ".repeat(700); // 2,100 バイト（1 文字 3 バイト）
    let (p, t) = prefix_at_char_boundary(&s, MAX_INDEX_KEY_BYTES);
    assert!(t);
    assert_eq!(p.len(), 1_998, "2,000 を超えない最長の文字境界");
    assert!(s.starts_with(p));
    let (q, t2) = prefix_at_char_boundary("short", MAX_INDEX_KEY_BYTES);
    assert_eq!((q, t2), ("short", false));
    // ちょうど上限は切らない。
    let exact = "a".repeat(MAX_INDEX_KEY_BYTES);
    assert!(!prefix_at_char_boundary(&exact, MAX_INDEX_KEY_BYTES).1);
}

#[test]
fn long_text_is_prefixed_and_flagged() {
    let long = "x".repeat(10_000);
    let keys = field_keys(&field(FieldType::Text), IndexTable::Text, &json!(long), 50)
        .unwrap()
        .unwrap();
    let FieldKeys::Text(rows) = keys else {
        panic!("text")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].val.len(), MAX_INDEX_KEY_BYTES);
    assert!(rows[0].truncated);
}

#[test]
fn multi_select_one_row_per_element_and_limit() {
    let keys = field_keys(
        &field(FieldType::MultiSelect),
        IndexTable::Text,
        &json!(["b", "a"]),
        50,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        keys,
        FieldKeys::Text(vec![text_key(0, "b"), text_key(1, "a")]),
        "ord は配列内の位置"
    );
    let too_many = json!(["a", "b", "c"]);
    let err = field_keys(
        &field(FieldType::MultiSelect),
        IndexTable::Text,
        &too_many,
        2,
    )
    .unwrap_err();
    assert!(matches!(err, DataError::Invalid(_)), "{err:?}");
}

#[test]
fn null_has_no_rows_and_wrong_shape_is_an_error() {
    assert!(
        field_keys(&field(FieldType::Text), IndexTable::Text, &json!(null), 50)
            .unwrap()
            .is_none()
    );
    assert!(field_keys(&field(FieldType::Text), IndexTable::Text, &json!(1), 50).is_err());
    assert!(field_keys(&field(FieldType::Number), IndexTable::Num, &json!("1"), 50).is_err());
}

#[test]
fn unique_norm_hashes_long_values_and_unifies_numbers() {
    assert_eq!(unique_norm("abc"), "abc");
    let a = format!("{}A", "x".repeat(3_000));
    let b = format!("{}B", "x".repeat(3_000));
    let (na, nb) = (unique_norm(&a), unique_norm(&b));
    assert_ne!(na, nb, "プレフィクスが同じでも全体が違えば別キー");
    assert!(na.len() <= MAX_INDEX_KEY_BYTES + 1 + 64);
    assert!(na.starts_with(&"x".repeat(MAX_INDEX_KEY_BYTES)));
    let n = field(FieldType::Number);
    let k1 = field_keys(&n, IndexTable::Unique, &json!(1), 50).unwrap();
    let k2 = field_keys(&n, IndexTable::Unique, &json!(1.0), 50).unwrap();
    assert_eq!(k1, k2, "1 と 1.0 は同じ一意キー");
    // uuid は表記ゆれを揃える。
    let mut r = field(FieldType::RecordRef);
    r.ref_table = Some(uuid::Uuid::nil());
    let up = field_keys(
        &r,
        IndexTable::Unique,
        &json!("6F1B24A0-0000-0000-0000-000000000001"),
        50,
    )
    .unwrap();
    assert_eq!(
        up,
        Some(FieldKeys::Unique(
            "6f1b24a0-0000-0000-0000-000000000001".into()
        ))
    );
}

#[test]
fn link_accepts_single_uuid_and_arrays() {
    let mut r = field(FieldType::RecordRef);
    r.ref_table = Some(uuid::Uuid::nil());
    let id = uuid::Uuid::new_v4();
    assert_eq!(
        field_keys(&r, IndexTable::Link, &json!(id.to_string()), 50).unwrap(),
        Some(FieldKeys::Link(vec![(0, id)]))
    );
    assert!(field_keys(&r, IndexTable::Link, &json!("nope"), 50).is_err());
    let many: Vec<String> = (0..3).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    assert!(
        field_keys(&r, IndexTable::Link, &json!(many), 2).is_err(),
        "多値上限"
    );
}

#[test]
fn system_keys_are_fixed_width_utc() {
    let t = Utc.with_ymd_and_hms(2026, 10, 22, 1, 2, 3).unwrap();
    // 擬似列 created_at / updated_at の表記は date_time の正規化と同じ固定幅（辞書順＝時刻順）。
    assert_eq!(format_timestamp(t), "2026-10-22T01:02:03.000000Z");
    let later = t + chrono::Duration::microseconds(1);
    assert!(format_timestamp(t) < format_timestamp(later));
}
