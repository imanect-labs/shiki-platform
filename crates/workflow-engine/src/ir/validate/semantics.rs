//! 条件の意味論の切替（2026-10・query-optimizer.md §3.1）で振る舞いが変わり得る条件の検出（Task 15.7）。
//!
//! `control/eval.rs` を data の評価規則に揃えたため、配列値に対する `eq` / `neq` / `in` / `contains` /
//! `starts_with` / `ends_with`、
//! 空配列の `exists` / `is_null`、`1.0` のような整数値の小数との等値は、
//! 保存済みのワークフローで分岐の結果が変わり得る。値の型は実行時まで決まらないので、**静的に配列と
//! 分かる**ものだけを検出する:
//!
//! - 比較の左辺・右辺が配列のリテラル
//! - 左辺が `$from input` で、`input_schema` がその位置を `array` と宣言している
//! - 右辺が整数値の小数（`1.0`）のリテラル（数値の等値）
//! - 大小比較（`lt` / `lte` / `gt` / `gte`）の値が、数字を文字列で書いたリテラル（`"50000"`）。数値との
//!   大小比較は黙って偽にせず `expr_type_error` で step を失敗させるようにした（BUG-14）ので、固定値を
//!   文字列で保存した金額のしきい値などは、以前の「黙って偽」から run の失敗に変わる
//!
//! 保存は止めない（警告）。同じ関数を `shiki-admin`（保存済み定義の一覧化）が使い、保存 API・検証 API・
//! チャットの emit_workflow は、ほかの保存を止めない警告（テンプレートの書き誤りなど）と合わせた
//! [`super::save_warnings`] から使う。ここは条件の意味論の切替だけを返す（一覧に別の検出を混ぜない）。

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
    NumericString,
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
                 contains / starts_with / ends_with はいずれかの要素の部分文字列・前方一致・後方一致・\
                 空配列の exists は偽）。分岐の結果が変わる場合があります"
            ),
            Reason::IntegralFloat => format!(
                "小数表記の整数（例: 1.0）と {op} で比較しています。2026-10 から数値は数値として比較します\
                 （1 と 1.0 は等しい）"
            ),
            Reason::NumericString => format!(
                "数字を文字列（引用符つき）で書いた値と {op} で大小比較しています。2026-10 から、数値と文字列の\
                 大小比較は黙って偽にせず step を失敗させます（expr_type_error）。文字列どうしなら辞書順の比較です\
                 （\"9\" は \"50000\" より大きい）。数として比べるなら引用符の無い数値で書いてください"
            ),
        }
    }
}

/// 意味論の切替で振る舞いが変わり得る条件（[`SEMANTICS_CHANGED`]）を返す。
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
        // starts_with / ends_with: 旧評価器は配列の左辺で常に偽・新しくは「いずれかの要素が前方 /
        // 後方一致」（R3-3）。exists / is_null: 空配列の扱いが変わる（空配列は値なし）。
        CmpOp::StartsWith | CmpOp::EndsWith | CmpOp::Exists | CmpOp::IsNull if left_array => {
            Some(Reason::ArrayOperand)
        }
        // 大小比較: 数値と文字列の組は expr_type_error（BUG-14）。数字の文字列のリテラルは、相手が数値なら
        // 失敗し、文字列なら辞書順になる（どちらも数の比較のつもりなら誤り）。
        CmpOp::Lt | CmpOp::Lte | CmpOp::Gt | CmpOp::Gte
            if numeric_string(&cmp.left) || right.is_some_and(numeric_string) =>
        {
            Some(Reason::NumericString)
        }
        _ => None,
    }
}

/// 数字だけを文字列で書いたリテラル（`"50000"`・`"-1.5"`。日付の `"2026-10-25"` などは含めない）。
fn numeric_string(e: &ValueExpr) -> bool {
    let ValueExpr::Literal(Value::String(s)) = e else {
        return false;
    };
    let t = s.trim();
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    let mut parts = t.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next();
    !int.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
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

    /// R3-3: 配列の左辺への starts_with / ends_with（旧は常に偽・新はいずれかの要素）を検出する。
    #[test]
    fn flags_starts_with_and_ends_with_on_arrays() {
        let w = ir(json!({
            "ir_version": 1, "name": "sw",
            "input_schema": { "type": "object", "properties": {
                "tags": { "type": "array", "items": { "type": "string" } },
                "name": { "type": "string" } } },
            "nodes": [ branch(json!({ "any": [
                { "cmp": { "left": { "$from": "input", "path": "/tags" }, "op": "starts_with", "right": "ur" } },
                { "cmp": { "left": ["urgent", "x"], "op": "ends_with", "right": "nt" } },
                { "cmp": { "left": { "$from": "input", "path": "/name" }, "op": "starts_with", "right": "a" } }
            ] })) ],
            "edges": [],
            "triggers": [ { "kind": "interactive" } ]
        }));
        let w = semantics_warnings(&w);
        assert_eq!(w.len(), 2, "{w:#?}");
        assert!(w[0].message.contains("starts_with"), "{}", w[0].message);
        assert!(w[1].message.contains("ends_with"), "{}", w[1].message);
    }

    /// BUG-14: 大小比較の値を数字の文字列で書いた条件（UI の固定値が文字列で保存したしきい値）を検出する。
    #[test]
    fn flags_numeric_strings_in_ordering_comparisons() {
        let w = ir(json!({
            "ir_version": 1, "name": "amount",
            "nodes": [ branch(json!({ "any": [
                { "cmp": { "left": { "$from": "input", "path": "/amount" }, "op": "gte", "right": "50000" } },
                { "cmp": { "left": { "$from": "input", "path": "/t" }, "op": "lt", "right": " -1.5 " } },
                { "cmp": { "left": "10", "op": "gt", "right": { "$from": "input", "path": "/n" } } },
                // 数値・日付の文字列・数字でない文字列・等値の比較は対象外。
                { "cmp": { "left": { "$from": "input", "path": "/amount" }, "op": "gt", "right": 50000 } },
                { "cmp": { "left": { "$from": "input", "path": "/d" }, "op": "lte", "right": "2026-10-25" } },
                { "cmp": { "left": { "$from": "input", "path": "/s" }, "op": "gt", "right": "abc" } },
                { "cmp": { "left": { "$from": "input", "path": "/v" }, "op": "gt", "right": "1." } },
                { "cmp": { "left": { "$from": "input", "path": "/amount" }, "op": "eq", "right": "50000" } }
            ] })) ],
            "edges": [],
            "triggers": [
                { "kind": "interactive" },
                { "kind": "event", "source": "storage.write", "scope": {},
                  "filter": { "cmp": { "left": { "$from": "trigger", "path": "/size" }, "op": "gt", "right": "1000" } } }
            ]
        }));
        let w = semantics_warnings(&w);
        assert_eq!(w.len(), 4, "{w:#?}");
        assert!(w.iter().all(|e| e.code == SEMANTICS_CHANGED));
        assert!(w[0].message.contains("大小比較"), "{}", w[0].message);
        assert_eq!(w[0].path.as_deref(), Some("/triggers/1/filter"));
        assert_eq!(
            w.iter()
                .filter(|e| e.node_id.as_deref() == Some("br"))
                .count(),
            3
        );
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
