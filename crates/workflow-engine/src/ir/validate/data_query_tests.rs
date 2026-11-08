//! 「データを検索」の列の保存時検証（BUG-244）の単体テスト。
#![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]

use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::ir::validate::CatalogTable;

const ITEMS: &str = "00000000-0000-0000-0000-0000000000c4";
const HIDDEN: &str = "00000000-0000-0000-0000-0000000000b3";
const BARE: &str = "00000000-0000-0000-0000-0000000000d5";

/// 保存者の見え方の定義（`secret` は読めないので入っていない）。
fn schema() -> TableSchema {
    let mut s: TableSchema = serde_json::from_value(json!({ "fields": [
        { "key": "sku", "type": "text", "unique": true },
        { "key": "name", "type": "text", "indexed": true, "searchable": true },
        { "key": "memo", "type": "text" },
        { "key": "stock", "type": "number", "indexed": true },
        { "key": "price", "type": "number" },
        { "key": "tags", "type": "multi_select", "indexed": true, "options": ["a"] },
        { "key": "orders", "type": "record_ref", "indexed": true,
          "ref_table": "00000000-0000-0000-0000-0000000000e6",
          "link": { "cardinality": "one_to_many", "reverse": true } }
    ] }))
    .unwrap();
    for (i, f) in s.fields.iter_mut().enumerate() {
        f.id = 10 + u16::try_from(i).unwrap();
    }
    s
}

fn catalog() -> Catalog {
    Catalog {
        tables: Some(BTreeMap::from([
            (
                ITEMS.to_string(),
                CatalogTable {
                    schema: Some(schema()),
                    ..CatalogTable::default()
                },
            ),
            // 定義の無い材料（承認の照合だけに引いたもの）は照合しない。
            (BARE.to_string(), CatalogTable::default()),
        ])),
        ..Catalog::default()
    }
}

fn ir(nodes: serde_json::Value) -> WorkflowIr {
    WorkflowIr::from_json(&json!({
        "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
        "nodes": nodes, "edges": []
    }))
    .unwrap()
}

fn query_node(id: &str, table: serde_json::Value, query: serde_json::Value) -> serde_json::Value {
    json!({ "id": id, "type": "data.query", "params": { "table": table, "query": query } })
}

fn located(errs: &[ValidationError]) -> Vec<(&str, &str, &str)> {
    errs.iter()
        .map(|e| {
            (
                e.code.as_str(),
                e.node_id.as_deref().unwrap_or_default(),
                e.path.as_deref().unwrap_or_default(),
            )
        })
        .collect()
}

/// 索引の無い列で絞る（all / any / not の中も）・並べる・まとめる・集計する IR は、書いた位置を指して止める。
/// 索引のある列・重複を許さない列・システムの列は通る。
#[test]
fn columns_without_an_index_are_refused_where_written() {
    let ir = ir(json!([
        query_node(
            "ok",
            json!(ITEMS),
            json!({
                "filter": { "all": [
                    { "cmp": { "field": "sku", "op": "eq", "value": { "$from": "input", "path": "/sku" } } },
                    { "any": [ { "cmp": { "field": "stock", "op": "lt", "value": 3 } },
                               { "not": { "cmp": { "field": "_created_at", "op": "gte", "value": "2026-01-01T00:00:00Z" } } } ] },
                    { "cmp": { "field": "name", "op": "contains", "value": "ねじ" } }
                ] },
                "sort": [ { "field": "_updated_at", "descending": true }, { "field": "stock" } ]
            })
        ),
        query_node(
            "bad",
            json!(ITEMS),
            json!({
                "filter": { "all": [
                    { "cmp": { "field": "sku", "op": "eq", "value": "A" } },
                    { "any": [ { "not": { "cmp": { "field": "memo", "op": "eq", "value": "x" } } } ] }
                ] },
                "sort": [ { "field": "price" } ]
            })
        ),
        query_node(
            "sum",
            json!(ITEMS),
            json!({
                "group_by": ["memo", "j.name"],
                "aggregate": [ { "metric": "count" }, { "metric": "sum", "field": "price" } ]
            })
        )
    ]));
    let mut errors = Vec::new();
    v4(&ir, &catalog(), &mut errors);
    assert_eq!(
        located(&errors),
        vec![
            (
                FIELD_NOT_INDEXED,
                "bad",
                "/params/query/filter/all/1/any/0/not/cmp/field"
            ),
            (FIELD_NOT_INDEXED, "bad", "/params/query/sort/0/field"),
            (FIELD_NOT_INDEXED, "sum", "/params/query/group_by/0"),
            (FIELD_NOT_INDEXED, "sum", "/params/query/aggregate/1/field"),
        ]
    );
    let m = &errors[0].message;
    assert!(
        m.contains("「memo」は索引の無い列なので、絞り込みの条件に使えません")
            && m.contains("索引のある列か重複を許さない列を使う"),
        "{m}"
    );
}

/// 無い列と読めない列は同じ `ir.unknown_field`（同じ文言）。部分一致は検索の対象の列だけ、逆向きのリンク・複数の値の
/// 並べ替え・システムの列のまとめ・数でない列の集計は使えない。
#[test]
fn other_refusals_follow_the_binder() {
    let ir = ir(json!([
        query_node(
            "missing",
            json!(ITEMS),
            json!({ "filter": { "cmp": { "field": "nope", "op": "eq", "value": 1 } } })
        ),
        query_node(
            "unreadable",
            json!(ITEMS),
            json!({ "filter": { "cmp": { "field": "secret", "op": "eq", "value": 1 } } })
        ),
        query_node(
            "partial",
            json!(ITEMS),
            json!({ "filter": { "cmp": { "field": "sku", "op": "contains", "value": "A" } } })
        ),
        query_node(
            "reverse",
            json!(ITEMS),
            json!({ "filter": { "cmp": { "field": "orders", "op": "exists" } } })
        ),
        query_node(
            "multi",
            json!(ITEMS),
            json!({ "sort": [ { "field": "tags" } ] })
        ),
        query_node(
            "system",
            json!(ITEMS),
            json!({ "group_by": ["_created_at"], "aggregate": [ { "metric": "count" } ] })
        ),
        query_node(
            "text_sum",
            json!(ITEMS),
            json!({ "aggregate": [ { "metric": "avg", "field": "sku" } ] })
        )
    ]));
    let mut errors = Vec::new();
    v4(&ir, &catalog(), &mut errors);
    let codes: Vec<(&str, &str)> = located(&errors).iter().map(|(c, n, _)| (*c, *n)).collect();
    assert_eq!(
        codes,
        vec![
            (UNKNOWN_FIELD, "missing"),
            (UNKNOWN_FIELD, "unreadable"),
            (FIELD_NOT_INDEXED, "partial"),
            (FIELD_NOT_USABLE, "reverse"),
            (FIELD_NOT_USABLE, "multi"),
            (FIELD_NOT_USABLE, "system"),
            (FIELD_NOT_USABLE, "text_sum"),
        ]
    );
    assert_eq!(
        errors[0].message.replace("nope", "<K>"),
        errors[1].message.replace("secret", "<K>"),
        "無い列と読めない列を言い分けない"
    );
    assert!(
        errors[2].message.contains("部分一致で検索できる列ではない"),
        "{}",
        errors[2].message
    );
    assert!(
        errors[3].message.contains("逆向きのリンクの列"),
        "{}",
        errors[3].message
    );
}

/// テーブルを参照で決めるノード・見えないテーブル・定義の無い材料・材料の無い構成は照合しない。前の版の書き方
/// （`sort: {field}`）は、その項目の位置を指す。
#[test]
fn only_literal_tables_with_a_definition_are_checked() {
    let bad = json!({ "filter": { "cmp": { "field": "memo", "op": "eq", "value": "x" } } });
    let ir = ir(json!([
        query_node(
            "dynamic",
            json!({ "$from": "input", "path": "/t" }),
            bad.clone()
        ),
        query_node("hidden", json!(HIDDEN), bad.clone()),
        query_node("bare", json!(BARE), bad.clone()),
        query_node(
            "old_form",
            json!(ITEMS.to_uppercase()),
            json!({ "sort": { "field": "memo" } })
        )
    ]));
    let mut errors = Vec::new();
    v4(&ir, &catalog(), &mut errors);
    assert_eq!(
        located(&errors),
        vec![(FIELD_NOT_INDEXED, "old_form", "/params/query/sort")]
    );
    let mut none = Vec::new();
    v4(&ir, &Catalog::default(), &mut none);
    assert!(none.is_empty(), "材料が無い構成は照合しない");
}

/// 保存の検証の全体（`validate`）でも止まる（保存 API・検証 API・emit_workflow が通る口）。
#[test]
fn validate_refuses_queries_on_columns_without_an_index() {
    let wf = json!({
        "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
        "nodes": [query_node("q", json!(ITEMS), json!({ "sort": [ { "field": "memo" } ] }))],
        "edges": []
    });
    let (wf, _) = crate::ir::scopes::complete_declared_scopes(&wf);
    let errs = super::super::validate(&wf, &catalog()).unwrap_err();
    assert_eq!(
        located(&errs),
        vec![(FIELD_NOT_INDEXED, "q", "/params/query/sort/0/field")]
    );
}
