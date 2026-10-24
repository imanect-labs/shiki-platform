use super::*;
use crate::derived::types::{LinkCardinality, LinkDef, RollupDef};
use crate::model::{ComputedDef, LookupDef};
use serde_json::json;
use std::collections::HashSet;

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

fn f(key: &str, ty: FieldType) -> FieldDef {
    FieldDef::new(key, ty)
}

fn ids(n: u128) -> Vec<Uuid> {
    (1..=n).map(Uuid::from_u128).collect()
}

fn refs(amounts: &[serde_json::Value], k: i64) -> RefRows {
    RefRows {
        rows: ids(amounts.len() as u128)
            .into_iter()
            .zip(amounts)
            .map(|(id, a)| {
                let mut m = Map::new();
                m.insert("amount".into(), a.clone());
                m.insert("name".into(), json!(format!("n{id}")));
                (id, m)
            })
            .collect(),
        types: [
            ("amount".to_string(), FieldType::Number),
            ("name".to_string(), FieldType::Text),
        ]
        .into(),
        min_rows: k,
        title_key: None,
    }
}

#[test]
fn rollup_sum_is_exact_decimal() {
    let r = refs(&[json!(0.1), json!(0.2), json!(0.3)], 1);
    let v = rollup_value(&ids(3), Some(&r), Some("amount"), RollupOp::Sum);
    // f64 なら 0.6000000000000001。
    assert_eq!(v.to_json(), json!(0.6));
    let v = rollup_value(&ids(3), Some(&r), Some("amount"), RollupOp::Avg);
    assert_eq!(v.to_json(), json!(0.2));
    let v = rollup_value(&ids(3), Some(&r), Some("amount"), RollupOp::Max);
    assert_eq!(v.to_json(), json!(0.3));
    let v = rollup_value(&ids(3), Some(&r), Some("amount"), RollupOp::Min);
    assert_eq!(v.to_json(), json!(0.1));
    let v = rollup_value(&ids(3), Some(&r), None, RollupOp::Count);
    assert_eq!(v.to_json(), json!(3));
    let v = rollup_value(&ids(2), Some(&r), Some("name"), RollupOp::Concat);
    assert_eq!(
        v.to_json(),
        json!(format!("n{}, n{}", Uuid::from_u128(1), Uuid::from_u128(2)))
    );
}

#[test]
fn rollup_suppresses_small_cells_and_ignores_invisible_rows() {
    // 参照先の K = 3。見える行が 2 件なら抑制（null）。
    let r = refs(&[json!(100), json!(200)], 3);
    let v = rollup_value(&ids(2), Some(&r), Some("amount"), RollupOp::Sum);
    assert_eq!(v, FVal::Null);
    // 見えない（RefRows に無い）参照先は件数にも合計にも入らない（PIT-20）。
    let r = refs(&[json!(100), json!(200)], 1);
    let mut linked = ids(2);
    linked.push(Uuid::from_u128(99));
    assert_eq!(
        rollup_value(&linked, Some(&r), None, RollupOp::Count).to_json(),
        json!(2)
    );
    assert_eq!(
        rollup_value(&linked, Some(&r), Some("amount"), RollupOp::Sum).to_json(),
        json!(300)
    );
    // 参照先が丸ごと見えない（テーブル viewer なし）→ null。
    let empty = RefRows {
        min_rows: 1,
        ..RefRows::default()
    };
    assert_eq!(
        rollup_value(&linked, Some(&empty), None, RollupOp::Count),
        FVal::Null
    );
}

fn link_field(key: &str, card: Option<LinkCardinality>) -> FieldDef {
    let mut d = f(key, FieldType::RecordRef);
    d.ref_table = Some(Uuid::nil());
    d.link = card.map(|c| LinkDef {
        cardinality: c,
        symmetric_field: None,
        symmetric_key: None,
        reverse: false,
    });
    d
}

#[test]
fn evaluates_lookup_rollup_formula_and_computed_in_order() {
    let mut lk_one = f("main_name", FieldType::Lookup);
    lk_one.lookup = Some(LookupDef {
        via_field: "main".into(),
        target_field: "name".into(),
    });
    let mut lk_many = f("names", FieldType::Lookup);
    lk_many.lookup = Some(LookupDef {
        via_field: "items".into(),
        target_field: "amount".into(),
    });
    let mut total = f("total", FieldType::Rollup);
    total.rollup = Some(RollupDef {
        link: "items".into(),
        target: Some("amount".into()),
        op: RollupOp::Sum,
    });
    let mut tax: FieldDef = f("tax", FieldType::Formula);
    tax.formula = Some(
        serde_json::from_value(json!({"round": [{"mul": [{"field": "total"}, {"lit": 0.1}]}, 0]}))
            .unwrap(),
    );
    let mut sum = f("sum", FieldType::Computed);
    sum.computed = Some(ComputedDef {
        op: ComputedOp::Sum,
        fields: vec!["a".into(), "b".into()],
    });
    let s = schema(vec![
        tax,
        link_field("main", None),
        link_field("items", Some(LinkCardinality::ManyToMany)),
        lk_one,
        lk_many,
        total,
        f("a", FieldType::Number),
        f("b", FieldType::Number),
        sum,
    ]);
    let mut all = HashMap::new();
    all.insert("items".to_string(), refs(&[json!(1000), json!(980)], 1));
    all.insert("main".to_string(), refs(&[json!(1)], 1));
    let eval = RowEval {
        schema: &s,
        refs: &all,
        now: chrono::Utc::now(),
        only_invariant: false,
        masked: &HashSet::new(),
    };
    let mut row = Map::new();
    row.insert("main".into(), json!(Uuid::from_u128(1).to_string()));
    row.insert(
        "items".into(),
        json!([
            Uuid::from_u128(2).to_string(),
            Uuid::from_u128(1).to_string()
        ]),
    );
    row.insert("a".into(), json!(0.1));
    row.insert("b".into(), json!(0.2));
    let out = eval.evaluate(&row);
    assert_eq!(out["main_name"], json!(format!("n{}", Uuid::from_u128(1))));
    // 多値リンク越しの lookup はリンクの並び順の配列。
    assert_eq!(out["names"], json!([980, 1000]));
    assert_eq!(out["total"], json!(1980));
    assert_eq!(out["tax"], json!(198), "数式は依存（total）の後に評価する");
    assert_eq!(out["sum"], json!(0.3), "computed の合計も 10 進で正確");
}

#[test]
fn only_invariant_skips_viewer_dependent_fields() {
    use crate::derived::types::Materialization;
    let mut x = f("x", FieldType::Formula);
    x.formula = Some(serde_json::from_value(json!({"lit": 1})).unwrap());
    x.materialization = Some(Materialization::Invariant);
    let mut y = f("y", FieldType::Formula);
    y.formula = Some(serde_json::from_value(json!({"lit": 2})).unwrap());
    y.materialization = Some(Materialization::PerViewer);
    let s = schema(vec![x, y]);
    let none = HashMap::new();
    let eval = RowEval {
        schema: &s,
        refs: &none,
        now: chrono::Utc::now(),
        only_invariant: true,
        masked: &HashSet::new(),
    };
    let out = eval.evaluate(&Map::new());
    assert_eq!(out.get("x"), Some(&json!(1)));
    assert!(out.get("y").is_none());
}

#[test]
fn linked_ids_reads_v1_strings_and_arrays() {
    let mut row = Map::new();
    let a = Uuid::from_u128(5);
    row.insert("v1".into(), json!(a.to_string()));
    row.insert("v2".into(), json!([a.to_string(), "bad"]));
    assert_eq!(linked_ids(&row, "v1"), vec![a]);
    assert_eq!(linked_ids(&row, "v2"), vec![a]);
    assert!(linked_ids(&row, "missing").is_empty());
}
