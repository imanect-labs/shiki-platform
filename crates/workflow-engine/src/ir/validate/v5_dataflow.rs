//! V5: データフロー（`$from` 祖先性・default 要否・条件木型整合・regex コンパイル・ir.md §8）。

use std::collections::{HashMap, HashSet};

use super::ValidationError;
use crate::ir::expr::{CmpOp, Comparison, Condition, FromRef, ValueExpr, MAX_REGEX_LEN};
use crate::ir::{Node, WorkflowIr};

/// V5 検証を実行する。
pub(super) fn check(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    let ancestors = compute_ancestors(ir);
    for node in &ir.nodes {
        let ctx = NodeCtx {
            node,
            ancestors: ancestors.get(node.id.as_str()),
        };
        check_value(&node.params, &ctx, errors);
    }
    // トリガ filter・branch/wait の condition の regex/型整合。
    for t in &ir.triggers {
        if let crate::ir::Trigger::Event(ev) = t {
            if let Some(cond) = &ev.filter {
                check_condition(cond, None, errors);
            }
        }
    }
}

struct NodeCtx<'a> {
    node: &'a Node,
    /// このノードの祖先ノード id 集合（`$from: nodes.<id>` の祖先性検証用）。
    ancestors: Option<&'a HashSet<String>>,
}

/// params の JSON を再帰的に走査し、`$from`/`$template`/condition を検証する。
fn check_value(value: &serde_json::Value, ctx: &NodeCtx<'_>, errors: &mut Vec<ValidationError>) {
    match value {
        serde_json::Value::Object(map) => {
            // `$from` オブジェクト。
            if map.contains_key("$from") {
                match serde_json::from_value::<FromRef>(value.clone()) {
                    Ok(from) => check_from(&from, ctx, errors),
                    // params は生 JSON で V1 の deny-unknown が効かないため、型崩れ（path 型違い・
                    // 余分フィールド等）はここで弾く（実行時にリテラル誤解釈されるのを防ぐ）。
                    Err(e) => errors.push(
                        ValidationError::new("ir.bad_ref", format!("不正な $from 式です: {e}"))
                            .at_node(&ctx.node.id),
                    ),
                }
                return;
            }
            // `$template` オブジェクトも同様に型崩れを弾く（untagged で Literal に吸収され保存されるのを防ぐ）。
            if map.contains_key("$template") {
                if let Err(e) =
                    serde_json::from_value::<crate::ir::expr::TemplateExpr>(value.clone())
                {
                    errors.push(
                        ValidationError::new("ir.bad_ref", format!("不正な $template 式です: {e}"))
                            .at_node(&ctx.node.id),
                    );
                }
                return;
            }
            // condition（branch/wait の params.condition）。検証したら再帰対象から外す（二重検証防止）。
            if let Some(cond) = map.get("condition") {
                if let Ok(c) = serde_json::from_value::<Condition>(cond.clone()) {
                    check_condition(&c, Some(ctx), errors);
                }
            }
            for (k, v) in map {
                if k == "condition" {
                    continue; // 上で検証済み（$from の重複エラーを避ける）。
                }
                check_value(v, ctx, errors);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                check_value(v, ctx, errors);
            }
        }
        _ => {}
    }
}

/// `$from` の source を検証する（祖先性・default 要否）。
fn check_from(from: &FromRef, ctx: &NodeCtx<'_>, errors: &mut Vec<ValidationError>) {
    // source は閉集合へ **exact match**（`each.item` や `input.foo` のような「先頭一致だが
    // 実行時リゾルバが解決できない」source を保存時に弾く。中身の指定は path で行う）。
    let source = from.from.as_str();
    match source {
        "input" | "trigger" | "run" => { /* 静的に許可 */ }
        "each" => {
            // each.* は map 領域内（node.parent が map ノード）でのみ有効。領域外は実行時に
            // 現在要素が無く失敗するため保存時に弾く。
            if ctx.node.parent.is_none() {
                errors.push(
                    ValidationError::new(
                        "ir.bad_ref",
                        format!(
                            "$from each.* は map 領域内でのみ使えます（node {} は領域外）",
                            ctx.node.id
                        ),
                    )
                    .at_node(&ctx.node.id),
                );
            }
        }
        s if s.starts_with("nodes.") && s.ends_with(".output") => {
            // `nodes.<id>.output` の <id> は当該ノードの祖先必須。
            let referenced = source.split('.').nth(1).unwrap_or("");
            let is_ancestor = ctx.ancestors.is_some_and(|a| a.contains(referenced));
            if !is_ancestor && from.default.is_none() {
                errors.push(
                    ValidationError::new(
                        "ir.bad_ref",
                        format!("$from nodes.{referenced} は祖先でなく default もありません"),
                    )
                    .at_node(&ctx.node.id),
                );
            }
        }
        _ => errors.push(
            ValidationError::new(
                "ir.bad_ref",
                format!(
                    "未知の $from source: {source}（input / trigger / run / each / nodes.<id>.output のみ。中身の指定は path を使う）"
                ),
            )
            .at_node(&ctx.node.id),
        ),
    }
}

/// 条件式の値式（left/right）の `$from` source を検証する。
///
/// ノード context 有り（branch/wait）は祖先性まで検証する。context 無し（イベント filter）は
/// 解決文脈が trigger しかないため、`nodes.*`/`each.*` を参照したら弾く（保存時に不解決を防ぐ）。
fn check_condition_from(
    expr: &ValueExpr,
    ctx: Option<&NodeCtx<'_>>,
    errors: &mut Vec<ValidationError>,
) {
    let ValueExpr::From(from) = expr else { return };
    if let Some(c) = ctx {
        check_from(from, c, errors);
    } else {
        let head = from.from.split('.').next().unwrap_or("");
        if matches!(head, "nodes" | "each") {
            errors.push(ValidationError::new(
                "ir.bad_ref",
                format!(
                    "イベント filter は {} を参照できません（trigger のみ解決可能）",
                    from.from
                ),
            ));
        }
    }
}

/// 条件木を検証する（regex コンパイル・演算子と right の整合・$from source）。
fn check_condition(cond: &Condition, ctx: Option<&NodeCtx<'_>>, errors: &mut Vec<ValidationError>) {
    cond.for_each_cmp(&mut |cmp: &Comparison| {
        check_condition_from(&cmp.left, ctx, errors);
        if let Some(right) = &cmp.right {
            check_condition_from(right, ctx, errors);
        }
        // exists/is_null は right 不要、それ以外は right 必須。
        let needs_right = !matches!(cmp.op, CmpOp::Exists | CmpOp::IsNull);
        if needs_right && cmp.right.is_none() {
            push_cmp_err(ctx, errors, "比較演算子に right がありません");
        }
        if !needs_right && cmp.right.is_some() {
            push_cmp_err(ctx, errors, "exists/is_null は right を取りません");
        }
        // matches は right がリテラル文字列で、regex としてコンパイル可能・長さ上限内。
        if cmp.op == CmpOp::Matches {
            match &cmp.right {
                Some(ValueExpr::Literal(serde_json::Value::String(pat))) => {
                    if pat.len() > MAX_REGEX_LEN {
                        push_cmp_err(ctx, errors, "regex パターンが長すぎます（最大 256）");
                    } else if regex::Regex::new(pat).is_err() {
                        push_cmp_err(ctx, errors, "regex がコンパイルできません");
                    }
                }
                _ => push_cmp_err(
                    ctx,
                    errors,
                    "matches の right はリテラル文字列である必要があります",
                ),
            }
        }
    });
}

fn push_cmp_err(ctx: Option<&NodeCtx<'_>>, errors: &mut Vec<ValidationError>, msg: &str) {
    let mut e = ValidationError::new("ir.expr_type_error", msg);
    if let Some(c) = ctx {
        e = e.at_node(&c.node.id);
    }
    errors.push(e);
}

/// 各ノードの祖先ノード集合を求める（エッジの推移閉包・逆向き到達）。
fn compute_ancestors(ir: &WorkflowIr) -> HashMap<String, HashSet<String>> {
    // 前段（predecessors）隣接。
    let mut preds: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &ir.nodes {
        preds.entry(n.id.as_str()).or_default();
    }
    for e in &ir.edges {
        preds
            .entry(e.to.as_str())
            .or_default()
            .push(e.from.as_str());
    }
    let mut result: HashMap<String, HashSet<String>> = HashMap::new();
    for n in &ir.nodes {
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack: Vec<&str> = preds.get(n.id.as_str()).cloned().unwrap_or_default();
        while let Some(p) = stack.pop() {
            if seen.insert(p.to_string()) {
                if let Some(pp) = preds.get(p) {
                    stack.extend(pp.iter().copied());
                }
            }
        }
        result.insert(n.id.clone(), seen);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)]
    fn ir(v: serde_json::Value) -> WorkflowIr {
        WorkflowIr::from_json(&v).unwrap()
    }

    #[test]
    fn from_non_ancestor_without_default_fails() {
        let mut errs = Vec::new();
        // b が a を参照するが a→b エッジが無い（祖先でない）→ default なしで失敗。
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "b", "type": "storage.read",
                      "params": { "id": { "$from": "nodes.a.output", "path": "/x" } } }
                ],
                "edges": []
            })),
            &mut errs,
        );
        assert!(errs.iter().any(|e| e.code == "ir.bad_ref"), "{errs:?}");
    }

    #[test]
    fn from_ancestor_ok() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "b", "type": "storage.read",
                      "params": { "id": { "$from": "nodes.a.output", "path": "/x" } } }
                ],
                "edges": [{ "from": "a", "to": "b" }]
            })),
            &mut errs,
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn bad_regex_in_condition() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "triggers": [{
                    "kind": "event", "source": "storage.write", "scope": {},
                    "filter": { "cmp": { "left": { "$from": "trigger" }, "op": "matches", "right": "(" } }
                }],
                "nodes": [], "edges": []
            })),
            &mut errs,
        );
        assert!(errs.iter().any(|e| e.code == "ir.expr_type_error"));
    }

    #[test]
    fn exists_without_right_ok() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "triggers": [{
                    "kind": "event", "source": "storage.write", "scope": {},
                    "filter": { "cmp": { "left": { "$from": "trigger", "path": "/a" }, "op": "exists" } }
                }],
                "nodes": [], "edges": []
            })),
            &mut errs,
        );
        assert!(errs.is_empty(), "{errs:?}");
    }
}
