//! フィールド識別子 3 層の採番・照合・JSONB キー変換の単体テスト（DB 不要）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::model::FieldDef;
use serde_json::json;

fn schema(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

#[test]
fn json_key_shape_is_strict() {
    assert_eq!(json_key(16), "f16");
    assert!(is_json_key("f16"));
    assert!(is_json_key("f0"));
    assert!(is_json_key("f32767"));
    for bad in [
        "f32768",
        "f016",
        "f",
        "16",
        "f1a",
        "title",
        "f1'; drop",
        "F16",
        "f99999",
    ] {
        assert!(!is_json_key(bad), "{bad} は拒否");
    }
}

#[test]
fn field_key_identifier_rule() {
    assert!(is_valid_field_key("applicant_2"));
    for bad in [
        "",
        "Applicant",
        "2x",
        "申請者",
        "a-b",
        "a b",
        &"a".repeat(65),
    ] {
        assert!(!is_valid_field_key(bad), "{bad} は拒否");
    }
}

#[test]
fn create_assigns_ids_from_16_in_declaration_order() {
    let mut s = schema(vec![
        FieldDef::new("title", FieldType::Text),
        FieldDef::new("amount", FieldType::Number),
    ]);
    s.fields[1].display_name = String::new();
    let next = assign_field_ids(None, &mut s).unwrap();
    assert_eq!(s.fields[0].id, 16);
    assert_eq!(s.fields[1].id, 17);
    assert_eq!(next, 18);
    assert_eq!(s.fields[1].display_name, "amount", "空の表示名は key");
    assert_eq!((s.fields[0].order, s.fields[1].order), (0, 1));
    validate_identity(&s).unwrap();
}

#[test]
fn create_keeps_explicit_ids_and_rejects_reserved() {
    let mut a = FieldDef::new("a", FieldType::Text);
    a.id = 40;
    let mut s = schema(vec![a, FieldDef::new("b", FieldType::Text)]);
    let next = assign_field_ids(None, &mut s).unwrap();
    assert_eq!(s.fields[1].id, 41, "指定済みの最大値の次から");
    assert_eq!(next, 42);

    let mut reserved = FieldDef::new("x", FieldType::Text);
    reserved.id = 3; // システム擬似列の予約枠
    let mut s = schema(vec![reserved]);
    assert!(assign_field_ids(None, &mut s).is_err());
}

#[test]
fn update_keeps_ids_by_key_and_never_reuses() {
    let mut cur = schema(vec![
        FieldDef::new("a", FieldType::Text),
        FieldDef::new("b", FieldType::Text),
    ]);
    let n = assign_field_ids(None, &mut cur).unwrap();
    // b を削除した履歴があり next_field_id が 30 まで進んでいる想定（再利用しない）。
    let n = n.max(30);
    let mut next = schema(vec![
        FieldDef::new("b", FieldType::Text), // v1 互換: id 省略でも key で照合
        FieldDef::new("a", FieldType::Text),
        FieldDef::new("c", FieldType::Text),
    ]);
    let n2 = assign_field_ids(Some((&cur, n)), &mut next).unwrap();
    assert_eq!(next.fields[0].id, 17);
    assert_eq!(next.fields[1].id, 16);
    assert_eq!(next.fields[2].id, 30, "新規は next_field_id から");
    assert_eq!(n2, 31);
}

#[test]
fn update_rejects_key_change_and_foreign_ids() {
    let mut cur = schema(vec![FieldDef::new("applicant", FieldType::UserRef)]);
    let n = assign_field_ids(None, &mut cur).unwrap();

    // 同じ id で key を変える → 422（key は不変）。
    let mut renamed = FieldDef::new("requester", FieldType::UserRef);
    renamed.id = 16;
    let mut next = schema(vec![renamed]);
    let err = assign_field_ids(Some((&cur, n)), &mut next).unwrap_err();
    assert!(
        matches!(&err, DataError::Invalid(m) if m.contains("key は変更できません")),
        "{err:?}"
    );

    // 存在しない id の指定 → 422（ID はサーバだけが採番する）。
    let mut forged = FieldDef::new("other", FieldType::Text);
    forged.id = 99;
    let mut next = schema(vec![FieldDef::new("applicant", FieldType::UserRef), forged]);
    assert!(assign_field_ids(Some((&cur, n)), &mut next).is_err());
}

#[test]
fn display_name_allows_japanese_and_rejects_control_chars() {
    let mut f = FieldDef::new("applicant", FieldType::UserRef);
    f.display_name = "申請者".into();
    let mut s = schema(vec![f]);
    assign_field_ids(None, &mut s).unwrap();
    validate_identity(&s).unwrap();
    assert_eq!(s.fields[0].display_name, "申請者");

    s.fields[0].display_name = "申請\u{0007}者".into();
    assert!(validate_identity(&s).is_err());
    s.fields[0].display_name = "あ".repeat(129);
    assert!(validate_identity(&s).is_err());
}

#[test]
fn searchable_only_on_text() {
    let mut f = FieldDef::new("n", FieldType::Number);
    f.searchable = true;
    let mut s = schema(vec![f]);
    assign_field_ids(None, &mut s).unwrap();
    assert!(validate_identity(&s).is_err());
}

#[test]
fn legacy_upgrade_is_deterministic() {
    let legacy: TableSchema = serde_json::from_value(json!({
        "fields": [
            { "name": "title", "type": "text" },
            { "name": "amount", "type": "number" }
        ]
    }))
    .unwrap();
    let (a, na) = upgrade_legacy_schema(&legacy).unwrap();
    let (b, nb) = upgrade_legacy_schema(&legacy).unwrap();
    assert_eq!(a, b);
    assert_eq!(na, nb);
    assert_eq!(a.fields[0].id, 16);
    assert_eq!(a.fields[0].display_name, "title");
    assert_eq!(na, 18);
}

/// v1 の row_policy が索引の無い列を参照していても、引き上げ後は §6.3（参照列は indexed 必須）を満たす。
#[test]
fn legacy_upgrade_promotes_row_policy_fields_to_indexed() {
    let legacy: TableSchema = serde_json::from_value(json!({
        "fields": [
            { "name": "title", "type": "text" },
            { "name": "dept", "type": "select", "options": ["a", "b"] },
            { "name": "applicant", "type": "user_ref" },
            { "name": "code", "type": "text", "unique": true }
        ],
        "row_policy": {
            "read": { "any": [
                { "field_cmp": { "field": "applicant", "op": "eq", "value": "user_id" } },
                { "field_cmp": { "field": "code", "op": "eq", "value": { "lit": "x" } } }
            ] },
            "write": { "field_cmp": { "field": "dept", "op": "eq", "value": { "lit": "a" } } }
        }
    }))
    .unwrap();
    // 旧規則のままでは検証を通らない（＝移行・改訂がすべて 422 になる状態の再現）。
    let mut raw = legacy.clone();
    assign_field_ids(None, &mut raw).unwrap();
    assert!(crate::schema::validate_table_schema(&raw).is_err());

    let (s, _) = upgrade_legacy_schema(&legacy).unwrap();
    assert!(s.field("applicant").unwrap().indexed, "read の参照列");
    assert!(s.field("dept").unwrap().indexed, "write の参照列");
    assert!(
        !s.field("title").unwrap().indexed,
        "参照されない列は触らない"
    );
    assert!(
        !s.field("code").unwrap().indexed,
        "unique 列は既に索引を持つので触らない"
    );
    crate::schema::validate_table_schema(&s).unwrap();
}

#[test]
fn encode_decode_roundtrip_and_drop_unknown() {
    let mut s = schema(vec![
        FieldDef::new("title", FieldType::Text),
        FieldDef::new("amount", FieldType::Number),
    ]);
    assign_field_ids(None, &mut s).unwrap();
    let ext = json!({"title": "出張", "amount": 3})
        .as_object()
        .cloned()
        .unwrap();
    let stored = encode_record(&s, &ext);
    assert_eq!(
        Value::Object(stored.clone()),
        json!({"f16": "出張", "f17": 3})
    );
    let back = decode_record(&s, &Value::Object(stored));
    assert_eq!(back, ext);

    // 保存形の未知キー（内部キー）は外へ出さない。
    let back = decode_record(&s, &json!({"f16": "x", "f99": "leak", "title": "v1"}));
    assert_eq!(Value::Object(back), json!({"title": "x"}));
}
