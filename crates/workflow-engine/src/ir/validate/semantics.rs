//! 条件の意味論の切替（2026-10・query-optimizer.md §3.1）で振る舞いが変わり得る条件の検出（Task 15.7）。
//!
//! `control/eval.rs` を data の評価規則に揃えたため、配列値に対する `eq` / `neq` / `in` / `contains`、
//! 空配列の `exists` / `is_null`、`1.0` のような整数値の小数との等値は、
//! 保存済みのワークフローで分岐の結果が変わり得る。値の型は実行時まで決まらないので、**静的に配列と
//! 分かる**ものだけを検出する:
//!
//! - 比較の左辺・右辺が配列のリテラル
//! - 左辺が `$from input` で、`input_schema` がその位置を `array` と宣言している
//! - 右辺が整数値の小数（`1.0`）のリテラル（数値の等値）
//!
//! 保存は止めない（警告）。同じ関数を保存 API・検証 API・`shiki-admin`（保存済み定義の一覧化）が使う。

use serde_json::Value;

use super::ValidationError;
use crate::ir::expr::{CmpOp, Comparison, Condition, ValueExpr};
use crate::ir::params::{BranchParams, WaitParams};
use crate::ir::{Trigger, WorkflowIr};
use crate::vocab::NodeType;

/// 警告のコード（エディタ・管理ツールが同じ語彙で扱う）。
pub const SEMANTICS_CHANGED: &str = "ir.condition_semantics_changed";

/// 検出の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    ArrayOperand,
    IntegralFloat,
}

impl Reason {
    fn message(self, op: CmpOp) -> String {
        let op = serde_json::to_value(op)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        match self {
            Reason::ArrayOperand => format!(
                "配列値に {op} を使っています。2026-10 から配列は「値の集合」として比較します\
                 （eq は要素を含むか・neq は含まないか・in はいずれかの要素が候補にあるか・\
                 contains はいずれかの要素の部分文字列・空配列の exists は偽）。分岐の結果が変わる場合があります"
            ),
            Reason::IntegralFloat => format!(
                "小数表記の整数（例: 1.0）と {op} で比較しています。2026-10 から数値は数値として比較します\
                 （1 と 1.0 は等しい）"
            ),
        }
    }
}

/// IR の条件のうち、意味論の切替で振る舞いが変わり得るものを警告として返す。
#[must_use]
pub fn semantics_warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let schema = ir.input_schema.as_ref();
    for (i, t) in ir.triggers.iter().enumerate() {
        if let Trigger::Event(ev) = t {
            if let Some(c) = &ev.filter {
                // イベント filter は trigger しか解決できない（input_schema は当てはまらない）。
                scan(c, None, &mut |cmp, reason| {
                    out.push(
                        ValidationError::new(SEMANTICS_CHANGED, reason.message(cmp.op))
                            .at_path(format!("/triggers/{i}/filter")),
                    );
                });
            }
        }
    }
    for node in &ir.nodes {
        let (cond, path) = match NodeType::parse(&node.node_type) {
            Some(NodeType::ControlBranch) => (
                serde_json::from_value::<BranchParams>(node.params.clone())
                    .ok()
                    .map(|p| p.condition),
                "/params/condition",
            ),
            Some(NodeType::ControlWait) => (
                serde_json::from_value::<WaitParams>(node.params.clone())
                    .ok()
                    .and_then(|p| p.filter),
                "/params/filter",
            ),
            _ => continue,
        };
        let Some(cond) = cond else { continue };
        scan(&cond, schema, &mut |cmp, reason| {
            out.push(
                ValidationError::new(SEMANTICS_CHANGED, reason.message(cmp.op))
                    .at_node(&node.id)
                    .at_path(path),
            );
        });
    }
    out
}

/// 条件木の比較ごとに検出する（1 つの比較につき理由は 1 つまで）。
fn scan(cond: &Condition, schema: Option<&Value>, emit: &mut dyn FnMut(&Comparison, Reason)) {
    cond.for_each_cmp(&mut |cmp| {
        if let Some(reason) = classify(cmp, schema) {
            emit(cmp, reason);
        }
    });
}

fn classify(cmp: &Comparison, schema: Option<&Value>) -> Option<Reason> {
    let left_array = is_array(&cmp.left, schema);
    let right = cmp.right.as_ref();
    let right_lit = match right {
        Some(ValueExpr::Literal(v)) => Some(v),
        _ => None,
    };
    match cmp.op {
        CmpOp::Eq | CmpOp::Neq | CmpOp::Contains => {
            if left_array || right.is_some_and(|r| is_array(r, schema)) {
                return Some(Reason::ArrayOperand);
            }
            match right_lit {
                Some(Value::Number(n)) if cmp.op != CmpOp::Contains && integral_float(n) => {
                    Some(Reason::IntegralFloat)
                }
                _ => None,
            }
        }
        CmpOp::In => {
            if left_array {
                return Some(Reason::ArrayOperand);
            }
            match right_lit {
                Some(Value::Array(items))
                    if items
                        .iter()
                        .any(|v| matches!(v, Value::Number(n) if integral_float(n))) =>
                {
                    Some(Reason::IntegralFloat)
                }
                _ => None,
            }
        }
        // 空配列の扱いが変わる（空配列は値なし）。
        CmpOp::Exists | CmpOp::IsNull if left_array => Some(Reason::ArrayOperand),
        _ => None,
    }
}

/// `1.0` のように小数で書かれた整数値（`1` とは JSON の値として異なり、旧評価器では等しくなかった）。
fn integral_float(n: &serde_json::Number) -> bool {
    n.is_f64() && n.as_f64().is_some_and(|f| f.fract() == 0.0)
}

/// 値式が静的に配列と分かるか（配列のリテラル・input_schema が array と宣言した `$from input`）。
fn is_array(e: &ValueExpr, schema: Option<&Value>) -> bool {
    match e {
        ValueExpr::Literal(Value::Array(_)) => true,
        ValueExpr::From(f) if f.from == "input" => schema
            .and_then(|s| schema_at(s, f.path.as_deref().unwrap_or("")))
            .is_some_and(declares_array),
        _ => false,
    }
}

/// JSON Pointer の位置の部分スキーマ（`properties` / `items` を辿る）。
fn schema_at<'a>(schema: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut cur = schema;
    for seg in pointer.split('/').skip(1) {
        let seg = seg.replace("~1", "/").replace("~0", "~");
        cur = if let Some(p) = cur.get("properties").and_then(|p| p.get(&seg)) {
            p
        } else if seg.parse::<usize>().is_ok() {
            cur.get("items")?
        } else {
            return None;
        };
    }
    Some(cur)
}

fn declares_array(s: &Value) -> bool {
    match s.get("type") {
        Some(Value::String(t)) => t == "array",
        Some(Value::Array(ts)) => ts.iter().any(|t| t == "array"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)] // json! をそのまま渡す。
    fn ir(v: Value) -> WorkflowIr {
        WorkflowIr::from_json(&v).unwrap()
    }

    #[allow(clippy::needless_pass_by_value)]
    fn branch(cond: Value) -> Value {
        json!({ "id": "br", "type": "control.branch", "params": { "condition": cond } })
    }

    #[test]
    fn flags_static_arrays_nulls_and_integral_floats_only() {
        let w = ir(json!({
            "ir_version": 1, "name": "sem",
            "input_schema": { "type": "object", "properties": {
                "tags": { "type": "array", "items": { "type": "string" } },
                "name": { "type": "string" } } },
            "nodes": [
                branch(json!({ "cmp": { "left": { "$from": "input", "path": "/tags" }, "op": "eq", "right": "a" } })),
                { "id": "b2", "type": "control.branch", "params": { "condition": { "any": [
                    { "cmp": { "left": { "$from": "input", "path": "/name" }, "op": "eq", "right": "x" } },
                    { "cmp": { "left": { "$from": "input", "path": "/name" }, "op": "in", "right": ["x", "y"] } },
                    { "cmp": { "left": { "$from": "input", "path": "/name" }, "op": "neq", "right": ["a"] } },
                    { "cmp": { "left": { "$from": "input", "path": "/n" }, "op": "eq", "right": 1.0 } },
                    { "cmp": { "left": ["a", "b"], "op": "contains", "right": "a" } },
                    { "cmp": { "left": { "$from": "input", "path": "/tags" }, "op": "exists" } }
                ] } } }
            ],
            "edges": [],
            "triggers": [
                { "kind": "interactive" },
                { "kind": "event", "source": "storage.write", "scope": {},
                  "filter": { "cmp": { "left": { "$from": "trigger", "path": "/x" }, "op": "eq", "right": [1] } } }
            ]
        }));
        let w = semantics_warnings(&w);
        let at: Vec<(Option<&str>, &str)> = w
            .iter()
            .map(|e| (e.node_id.as_deref(), e.path.as_deref().unwrap_or("")))
            .collect();
        assert_eq!(w.len(), 6, "{w:#?}");
        assert!(w.iter().all(|e| e.code == SEMANTICS_CHANGED));
        assert!(at.contains(&(Some("br"), "/params/condition")), "{at:?}");
        assert!(at.contains(&(None, "/triggers/1/filter")), "{at:?}");
        // b2: 配列との neq・1.0・配列リテラルの contains・array 宣言の exists の 4 件（単値の eq / in は対象外）。
        assert_eq!(at.iter().filter(|(n, _)| *n == Some("b2")).count(), 4);
    }

    #[test]
    fn plain_scalar_conditions_are_not_flagged() {
        let w = ir(json!({
            "ir_version": 1, "name": "plain",
            "nodes": [ branch(json!({ "all": [
                { "cmp": { "left": { "$from": "input", "path": "/n" }, "op": "gt", "right": 3 } },
                { "cmp": { "left": { "$from": "input", "path": "/s" }, "op": "contains", "right": "x" } },
                { "cmp": { "left": "b", "op": "in", "right": { "$from": "input", "path": "/list" } } }
            ] })) ],
            "edges": [],
            "triggers": [ { "kind": "interactive" } ]
        }));
        assert!(semantics_warnings(&w).is_empty());
    }
}
