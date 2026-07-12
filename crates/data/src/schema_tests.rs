//! data スキーマ検証（validate_table_schema / validate_field / validate_schema_update）の
//! 検証マトリクス。schema.rs の 500 行上限を守るため #[path] で分離する（純粋・DB 不要）。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use super::*;
use crate::model::{ComputedDef, LookupDef};

fn field(name: &str, ty: FieldType) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type: ty,
        required: false,
        unique: false,
        indexed: false,
        options: vec![],
        ref_table: None,
        lookup: None,
        computed: None,
    }
}

fn schema(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
    }
}

#[test]
fn field_name_rules() {
    assert!(is_valid_field_name("title"));
    assert!(is_valid_field_name("a1_b2"));
    assert!(!is_valid_field_name(""));
    assert!(!is_valid_field_name("Title")); // 大文字
    assert!(!is_valid_field_name("1abc")); // 数字始まり
    assert!(!is_valid_field_name("a'; drop")); // 記号
    assert!(!is_valid_field_name(&"x".repeat(65))); // 長すぎ
}

#[test]
fn rejects_duplicate_and_empty() {
    assert!(validate_table_schema(&schema(vec![])).is_err());
    let dup = schema(vec![
        field("a", FieldType::Text),
        field("a", FieldType::Text),
    ]);
    assert!(validate_table_schema(&dup).is_err());
}

#[test]
fn select_requires_options() {
    let mut f = field("status", FieldType::Select);
    assert!(validate_table_schema(&schema(vec![f.clone()])).is_err());
    f.options = vec!["open".into(), "closed".into()];
    assert!(validate_table_schema(&schema(vec![f])).is_ok());
}

#[test]
fn record_ref_requires_ref_table() {
    let f = field("parent", FieldType::RecordRef);
    assert!(validate_table_schema(&schema(vec![f])).is_err());
}

#[test]
fn lookup_requires_record_ref_via() {
    let mut via = field("customer", FieldType::RecordRef);
    via.ref_table = Some(uuid::Uuid::new_v4());
    let mut lk = field("customer_name", FieldType::Lookup);
    lk.lookup = Some(LookupDef {
        via_field: "customer".into(),
        target_field: "name".into(),
    });
    assert!(validate_table_schema(&schema(vec![via.clone(), lk.clone()])).is_ok());
    // via が record_ref でないと拒否。
    lk.lookup = Some(LookupDef {
        via_field: "title".into(),
        target_field: "name".into(),
    });
    let s = schema(vec![field("title", FieldType::Text), via, lk]);
    assert!(validate_table_schema(&s).is_err());
}

#[test]
fn computed_type_must_match_op() {
    let mut c = field("total", FieldType::Computed);
    c.computed = Some(ComputedDef {
        op: ComputedOp::Sum,
        fields: vec!["price".into()],
    });
    // 対象が text だと拒否。
    let bad = schema(vec![field("price", FieldType::Text), c.clone()]);
    assert!(validate_table_schema(&bad).is_err());
    let ok = schema(vec![field("price", FieldType::Number), c]);
    assert!(validate_table_schema(&ok).is_ok());
}

#[test]
fn derived_fields_reject_flags() {
    let mut via = field("customer", FieldType::RecordRef);
    via.ref_table = Some(uuid::Uuid::new_v4());
    let mut lk = field("customer_name", FieldType::Lookup);
    lk.lookup = Some(LookupDef {
        via_field: "customer".into(),
        target_field: "name".into(),
    });
    lk.indexed = true;
    assert!(validate_table_schema(&schema(vec![via, lk])).is_err());
}

#[test]
fn unique_rejected_on_multi_select_and_file() {
    let mut ms = field("tags", FieldType::MultiSelect);
    ms.options = vec!["a".into()];
    ms.unique = true;
    assert!(validate_table_schema(&schema(vec![ms])).is_err());
    let mut fr = field("attachment", FieldType::FileRef);
    fr.unique = true;
    assert!(validate_table_schema(&schema(vec![fr])).is_err());
}

#[test]
fn status_field_must_be_select() {
    let mut s = schema(vec![field("state", FieldType::Text)]);
    s.status_field = Some("state".into());
    assert!(validate_table_schema(&s).is_err());
    let mut sel = field("state", FieldType::Select);
    sel.options = vec!["draft".into(), "done".into()];
    let mut s = schema(vec![sel]);
    s.status_field = Some("state".into());
    assert!(validate_table_schema(&s).is_ok());
}

#[test]
fn schema_update_is_additive_only() {
    let cur = schema(vec![field("title", FieldType::Text)]);
    // 追加は OK。
    let next = schema(vec![
        field("title", FieldType::Text),
        field("n", FieldType::Number),
    ]);
    assert!(validate_schema_update(&cur, &next).is_ok());
    // 削除は拒否。
    assert!(validate_schema_update(&cur, &schema(vec![field("n", FieldType::Number)])).is_err());
    // 型変更は拒否。
    assert!(
        validate_schema_update(&cur, &schema(vec![field("title", FieldType::Number)])).is_err()
    );
}

// ---- 追加: 未被覆ブランチ（options/lookup/computed/status/aggregate/上限） ----

fn opt_select(name: &str, opts: &[&str]) -> FieldDef {
    let mut f = field(name, FieldType::Select);
    f.options = opts.iter().map(|s| (*s).to_string()).collect();
    f
}

#[test]
fn rejects_too_many_fields() {
    let many: Vec<FieldDef> = (0..201)
        .map(|i| field(&format!("f{i}"), FieldType::Text))
        .collect();
    assert!(validate_table_schema(&schema(many)).is_err());
}

#[test]
fn select_option_limits_and_dups() {
    // 選択肢が多すぎる。
    let too_many: Vec<String> = (0..201).map(|i| format!("o{i}")).collect();
    let mut f = field("s", FieldType::Select);
    f.options = too_many;
    assert!(validate_table_schema(&schema(vec![f])).is_err());
    // 空の選択肢。
    assert!(validate_table_schema(&schema(vec![opt_select("s", &[""])])).is_err());
    // 長すぎる選択肢。
    let long = "x".repeat(129);
    assert!(validate_table_schema(&schema(vec![opt_select("s", &[&long])])).is_err());
    // 重複選択肢。
    assert!(validate_table_schema(&schema(vec![opt_select("s", &["a", "a"])])).is_err());
}

#[test]
fn lookup_missing_def_and_bad_target() {
    // lookup 定義なし。
    assert!(validate_table_schema(&schema(vec![field("x", FieldType::Lookup)])).is_err());
    // target_field が不正。
    let mut via = field("cust", FieldType::RecordRef);
    via.ref_table = Some(uuid::Uuid::new_v4());
    let mut lk = field("cust_name", FieldType::Lookup);
    lk.lookup = Some(LookupDef {
        via_field: "cust".into(),
        target_field: "Bad Name".into(),
    });
    assert!(validate_table_schema(&schema(vec![via, lk])).is_err());
    // via_field が存在しない。
    let mut lk2 = field("y", FieldType::Lookup);
    lk2.lookup = Some(LookupDef {
        via_field: "nope".into(),
        target_field: "name".into(),
    });
    assert!(validate_table_schema(&schema(vec![lk2])).is_err());
}

#[test]
fn computed_missing_def_empty_and_missing_src() {
    // computed 定義なし。
    assert!(validate_table_schema(&schema(vec![field("t", FieldType::Computed)])).is_err());
    // fields が空。
    let mut c = field("t", FieldType::Computed);
    c.computed = Some(ComputedDef {
        op: ComputedOp::Sum,
        fields: vec![],
    });
    assert!(validate_table_schema(&schema(vec![c])).is_err());
    // src が存在しない。
    let mut c2 = field("t", FieldType::Computed);
    c2.computed = Some(ComputedDef {
        op: ComputedOp::Sum,
        fields: vec!["nope".into()],
    });
    assert!(validate_table_schema(&schema(vec![c2])).is_err());
    // Concat は text 対象で OK。
    let mut c3 = field("full", FieldType::Computed);
    c3.computed = Some(ComputedDef {
        op: ComputedOp::Concat,
        fields: vec!["a".into()],
    });
    let ok = schema(vec![field("a", FieldType::Text), c3]);
    assert!(validate_table_schema(&ok).is_ok());
}

#[test]
fn derived_reject_required_and_unique() {
    let mut via = field("cust", FieldType::RecordRef);
    via.ref_table = Some(uuid::Uuid::new_v4());
    let mut lk = field("cust_name", FieldType::Lookup);
    lk.lookup = Some(LookupDef {
        via_field: "cust".into(),
        target_field: "name".into(),
    });
    lk.required = true;
    assert!(validate_table_schema(&schema(vec![via.clone(), lk])).is_err());
    let mut lk2 = field("cust_name", FieldType::Lookup);
    lk2.lookup = Some(LookupDef {
        via_field: "cust".into(),
        target_field: "name".into(),
    });
    lk2.unique = true;
    assert!(validate_table_schema(&schema(vec![via, lk2])).is_err());
}

#[test]
fn status_field_and_aggregate_rules() {
    // status_field が fields にない。
    let mut s = schema(vec![field("a", FieldType::Text)]);
    s.status_field = Some("missing".into());
    assert!(validate_table_schema(&s).is_err());
    // status_field が select でない。
    let mut s2 = schema(vec![field("st", FieldType::Text)]);
    s2.status_field = Some("st".into());
    assert!(validate_table_schema(&s2).is_err());
    // status_field が select なら OK。
    let mut s3 = schema(vec![opt_select("st", &["open", "closed"])]);
    s3.status_field = Some("st".into());
    assert!(validate_table_schema(&s3).is_ok());
    // aggregate_min_rows < 1 は拒否。
    let mut s4 = schema(vec![field("a", FieldType::Text)]);
    s4.aggregate_min_rows = Some(0);
    assert!(validate_table_schema(&s4).is_err());
}
