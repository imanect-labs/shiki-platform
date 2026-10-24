//! クエリ条件の 2 値論理を共通の表（`semantics_table.json`）で固定する（query-optimizer.md §3.1・Task 15.7）。
//!
//! 同じ表を workflow の評価器（`crates/workflow-engine/src/control/eval.rs`）のテストも読む。両方が
//! 表の `expect` と一致すれば、ワークフローの分岐と `data.query` は同じ条件で同じ行を選ぶ。
//!
//! data 側は「束縛（[`super::bound::compare`]）→ 2 値論理の参照評価器（[`super::eval::eval`]）」で評価する。
//! 束縛は索引到達可能性（`searchable` 等）の検査も兼ねるが、ここで見たいのは意味論なので、
//! フィールドは検査を通る形（索引済み・部分一致可）で作る。
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use serde_json::Value;

use super::bound::{compare, BField, FieldKind, Val};
use super::eval::{eval, Rec};
use super::ir::CondOp;
use crate::model::FieldType;
use crate::numeric::Dec;

const FIELD: crate::model::FieldId = 16;

fn field(ty: &str) -> BField {
    let (ty, kind) = match ty {
        "text" => (FieldType::Text, FieldKind::Text),
        "number" => (FieldType::Number, FieldKind::Num),
        "multi_select" => (FieldType::MultiSelect, FieldKind::Multi),
        other => panic!("表の未知の型: {other}"),
    };
    BField {
        id: FIELD,
        key: "f".into(),
        ty: Some(ty),
        kind,
        searchable: true,
        ref_table: None,
        derived: false,
    }
}

/// 表の値（外部形）を評価器のレコード（値の集合）へ。値なし・空配列は空集合。
fn record(f: &BField, value: Option<&Value>) -> Rec {
    let one = |v: &Value| match f.kind {
        FieldKind::Num => Val::Num(Dec::parse(&v.to_string()).expect("数値")),
        _ => Val::Text(v.as_str().expect("文字列").to_string()),
    };
    let vals: Vec<Val> = match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(one).collect(),
        Some(v) => vec![one(v)],
    };
    [(FIELD, vals)].into_iter().collect()
}

#[test]
fn data_evaluator_matches_the_shared_semantics_table() {
    let table: Value = serde_json::from_str(crate::CONDITION_SEMANTICS_TABLE).unwrap();
    let cases = table["cases"].as_array().unwrap();
    assert!(cases.len() >= 60, "表が欠けている: {}", cases.len());
    let mut failures = Vec::new();
    for c in cases {
        let f = field(c["type"].as_str().unwrap());
        let op: CondOp = serde_json::from_value(c["op"].clone()).unwrap();
        let expr = compare(&f, op, c.get("operand")).unwrap_or_else(|e| {
            panic!("{c}: data が束縛を拒否した（表には有効な組だけを載せる）: {e}")
        });
        let got = eval(&expr, &record(&f, c.get("value")));
        if got != c["expect"].as_bool().unwrap() {
            failures.push(format!("{c} → {got}"));
        }
    }
    assert!(
        failures.is_empty(),
        "表と食い違う:\n{}",
        failures.join("\n")
    );
}
