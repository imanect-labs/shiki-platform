//! workflow の条件評価器（[`super::eval`]）が data の評価規則と同じ結果を返すことを、data と共通の
//! テスト表（`data::CONDITION_SEMANTICS_TABLE`・演算子 × 型 × 値なし／空／多値／数値）で固定する
//! （query-optimizer.md §3.1・Task 15.7）。data 側は `crates/data/src/query/semantics_tests.rs` が同じ表を読む。
#![allow(clippy::unwrap_used, clippy::panic)]

use serde_json::{json, Value};

use super::eval::{eval_condition, ValueResolver};
use crate::ir::expr::{CmpOp, Comparison, Condition, FromRef, ValueExpr};

/// `$from input` を表の 1 行のレコード（`{ "f": 値 }`・値なしはキーごと無い）として解決する。
struct Row(Value);

impl ValueResolver for Row {
    fn resolve_from(&self, from: &FromRef) -> Option<Value> {
        match &from.path {
            Some(p) => self.0.pointer(p).cloned(),
            None => Some(self.0.clone()),
        }
    }
}

#[test]
fn workflow_evaluator_matches_the_shared_semantics_table() {
    let table: Value = serde_json::from_str(data::CONDITION_SEMANTICS_TABLE).unwrap();
    let cases = table["cases"].as_array().unwrap();
    assert!(cases.len() >= 60, "表が欠けている: {}", cases.len());
    let mut failures = Vec::new();
    for c in cases {
        let op: CmpOp = serde_json::from_value(c["op"].clone()).unwrap();
        let record = match c.get("value") {
            Some(v) => json!({ "f": v }),
            None => json!({}),
        };
        let cond = Condition::Cmp(Comparison {
            left: ValueExpr::From(FromRef {
                from: "input".into(),
                path: Some("/f".into()),
                default: None,
            }),
            op,
            right: c.get("operand").cloned().map(ValueExpr::Literal),
        });
        let got = eval_condition(&cond, &Row(record));
        if got != c["expect"].as_bool().unwrap() {
            failures.push(format!("{c} → {got}"));
        }
    }
    assert!(
        failures.is_empty(),
        "data の評価規則と食い違う:\n{}",
        failures.join("\n")
    );
}

/// `not` は補集合（2 値論理）: `not(eq c) ≡ neq c`・`not(exists) ≡ is_null` が全型で成り立つ。
#[test]
fn negation_is_the_complement_for_every_case() {
    let table: Value = serde_json::from_str(data::CONDITION_SEMANTICS_TABLE).unwrap();
    for c in table["cases"].as_array().unwrap() {
        let op: CmpOp = serde_json::from_value(c["op"].clone()).unwrap();
        let record = c
            .get("value")
            .map_or_else(|| json!({}), |v| json!({ "f": v }));
        let leaf = |op| {
            Condition::Cmp(Comparison {
                left: ValueExpr::From(FromRef {
                    from: "input".into(),
                    path: Some("/f".into()),
                    default: None,
                }),
                op,
                right: c.get("operand").cloned().map(ValueExpr::Literal),
            })
        };
        let r = Row(record);
        let not = eval_condition(&Condition::Not(Box::new(leaf(op))), &r);
        assert_eq!(not, !eval_condition(&leaf(op), &r), "{c}");
        if op == CmpOp::Eq {
            assert_eq!(not, eval_condition(&leaf(CmpOp::Neq), &r), "{c}");
        }
    }
}
