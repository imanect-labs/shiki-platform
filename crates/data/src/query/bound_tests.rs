//! 束縛・検証（索引到達可能性・マスク 403・型）の単体テスト。
#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;
use crate::model::{FieldDef, FieldType};
use serde_json::json;

fn schema() -> TableSchema {
    let mut title = FieldDef::new("title", FieldType::Text);
    title.indexed = true;
    title.searchable = true;
    let mut amount = FieldDef::new("amount", FieldType::Number);
    amount.indexed = true;
    let mut tags = FieldDef::new("tags", FieldType::MultiSelect);
    tags.indexed = true;
    tags.options = vec!["a".into()];
    let mut secret = FieldDef::new("secret", FieldType::Text);
    secret.indexed = true;
    secret.searchable = true;
    let mut due = FieldDef::new("due", FieldType::DateTime);
    due.indexed = true;
    let memo = FieldDef::new("memo", FieldType::Text);
    let mut s = TableSchema {
        fields: vec![title, amount, tags, secret, due, memo],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
    };
    crate::field_key::assign_field_ids(None, &mut s).unwrap();
    s
}

fn q(v: serde_json::Value) -> DataQuery {
    serde_json::from_value(v).unwrap()
}

fn masked() -> HashSet<String> {
    ["secret".to_string()].into()
}

#[test]
fn masked_columns_are_forbidden_everywhere() {
    let s = schema();
    let m = masked();
    for body in [
        json!({"filter": {"cmp": {"field": "secret", "op": "eq", "value": "x"}}}),
        json!({"sort": [{"field": "secret"}]}),
        json!({"group_by": ["secret"], "aggregate": [{"metric": "count"}]}),
        json!({"aggregate": [{"metric": "sum", "field": "secret"}]}),
        json!({"search": "x", "search_fields": ["secret"]}),
    ] {
        let r = bind_query(&s, &m, &q(body.clone()));
        assert!(matches!(r, Err(DataError::Forbidden)), "{body}: {r:?}");
    }
    // 暗黙の検索対象からはマスク列を外す（403 にはしない）。
    let b = bind_query(&s, &m, &q(json!({"search": "x"}))).unwrap();
    let mut atoms = Vec::new();
    b.filter.atoms(&mut atoms);
    assert_eq!(atoms.len(), 1, "title だけ");
}

#[test]
fn reachability_and_types_are_enforced_before_execution() {
    let s = schema();
    let m = HashSet::new();
    for body in [
        json!({"filter": {"cmp": {"field": "memo", "op": "eq", "value": "x"}}}),
        json!({"filter": {"cmp": {"field": "nope", "op": "eq", "value": "x"}}}),
        json!({"filter": {"cmp": {"field": "title", "op": "matches", "value": ".*"}}}),
        json!({"filter": {"cmp": {"field": "tags", "op": "lt", "value": "a"}}}),
        json!({"filter": {"cmp": {"field": "amount", "op": "eq", "value": "1"}}}),
        json!({"filter": {"cmp": {"field": "tags", "op": "contains", "value": "a"}}}),
        json!({"sort": [{"field": "tags"}]}),
        json!({"offset": 10}),
        json!({"group_by": ["title"]}),
    ] {
        let r = bind_query(&s, &m, &q(body.clone()));
        assert!(matches!(r, Err(DataError::Invalid(_))), "{body}: {r:?}");
    }
}

#[test]
fn neq_and_is_null_are_negations_and_values_are_normalized() {
    let s = schema();
    let b = bind_query(
        &s,
        &HashSet::new(),
        &q(json!({"filter": {"all": [
            {"cmp": {"field": "title", "op": "neq", "value": "x"}},
            {"cmp": {"field": "amount", "op": "is_null"}},
            {"cmp": {"field": "due", "op": "eq", "value": "2026-01-01T09:00:00+09:00"}}
        ]}})),
    )
    .unwrap();
    let Expr::And(v) = &b.filter else {
        panic!("{:?}", b.filter)
    };
    assert!(matches!(v[0], Expr::Not(_)));
    assert!(matches!(v[1], Expr::Not(_)));
    assert_eq!(
        v[2],
        Expr::atom(
            b.fields.values().find(|f| f.key == "due").unwrap().id,
            Leaf::Eq(Val::Text("2026-01-01T00:00:00.000000Z".into()))
        )
    );
}

#[test]
fn default_sort_and_precision() {
    let s = schema();
    let b = bind_query(&s, &HashSet::new(), &q(json!({}))).unwrap();
    assert_eq!(
        b.sort,
        vec![BSort {
            field: SYSTEM_FIELD_UPDATED_AT,
            desc: true
        }]
    );
    assert_eq!(b.sort_precision, SortPrecision::Exact);
    let b = bind_query(
        &s,
        &HashSet::new(),
        &q(json!({"sort": [{"field": "title"}]})),
    )
    .unwrap();
    assert_eq!(b.sort_precision, SortPrecision::Prefix);
    let b = bind_query(
        &s,
        &HashSet::new(),
        &q(json!({"sort": [{"field": "_created_at"}], "limit": 100_000})),
    )
    .unwrap();
    assert_eq!(b.limit, MAX_LIMIT);
    assert_eq!(b.sort[0].field, SYSTEM_FIELD_CREATED_AT);
}
