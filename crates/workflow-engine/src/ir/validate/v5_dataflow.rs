//! V5: データフロー（`$from` 祖先性・default 要否・path の形・`$template` の差し込み・条件木型整合・
//! regex コンパイル・ir.md §8）。
//!
//! 値の位置は実行時の解決（`control::eval::resolve_deep`）と同じ範囲を見る: リテラルの object / array の中・
//! `$template` の vars の中・条件の値（`in` の候補の配列など）の中の `$from` も検証する。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::template::{child, missing_placeholders};
use super::ValidationError;
use crate::ir::expr::{
    CmpOp, Comparison, Condition, FromRef, TemplateExpr, ValueExpr, MAX_REGEX_LEN,
};
use crate::ir::{Node, WorkflowIr};

/// V5 検証を実行する。
pub(super) fn check(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    let ancestors = compute_ancestors(ir);
    for node in &ir.nodes {
        let ctx = NodeCtx {
            node,
            ancestors: ancestors.get(node.id.as_str()),
        };
        check_value(&node.params, "/params", Some(&ctx), errors);
    }
    // トリガ filter の regex/型整合・参照（trigger しか解決できない）。
    for (i, t) in ir.triggers.iter().enumerate() {
        if let crate::ir::Trigger::Event(ev) = t {
            if let Some(cond) = &ev.filter {
                check_condition(cond, &format!("/triggers/{i}/filter"), None, errors);
            }
        }
    }
}

struct NodeCtx<'a> {
    node: &'a Node,
    /// このノードの祖先ノード id 集合（`$from: nodes.<id>` の祖先性検証用）。
    ancestors: Option<&'a HashSet<String>>,
}

/// 位置つきのエラー（ノードの params ならノード id も付ける・イベント filter は位置だけ）。
fn err(
    code: &str,
    msg: impl Into<String>,
    path: &str,
    ctx: Option<&NodeCtx<'_>>,
) -> ValidationError {
    let e = ValidationError::new(code, msg).at_path(path);
    match ctx {
        Some(c) => e.at_node(&c.node.id),
        None => e,
    }
}

/// 値の JSON を再帰的に走査し、`$from`/`$template`/condition を検証する。
///
/// `ctx` が `None` はイベント filter（解決できるのはイベントのペイロードだけ）。
fn check_value(
    value: &Value,
    path: &str,
    ctx: Option<&NodeCtx<'_>>,
    errors: &mut Vec<ValidationError>,
) {
    match value {
        Value::Object(map) => {
            // `$from` オブジェクト。
            if map.contains_key("$from") {
                match serde_json::from_value::<FromRef>(value.clone()) {
                    Ok(from) => check_from(&from, path, ctx, errors),
                    // params は生 JSON で V1 の deny-unknown が効かないため、型崩れ（path 型違い・
                    // 余分フィールド等）はここで弾く（実行時にリテラル誤解釈されるのを防ぐ）。
                    Err(e) => errors.push(err(
                        "ir.bad_ref",
                        format!("不正な $from 式です: {e}"),
                        path,
                        ctx,
                    )),
                }
                return;
            }
            // `$template` オブジェクトも同様に型崩れを弾く（untagged で Literal に吸収され保存されるのを防ぐ）。
            if map.contains_key("$template") {
                match serde_json::from_value::<TemplateExpr>(value.clone()) {
                    Ok(t) => check_template(&t, map, path, ctx, errors),
                    Err(e) => errors.push(err(
                        "ir.bad_ref",
                        format!("不正な $template 式です: {e}"),
                        path,
                        ctx,
                    )),
                }
                return;
            }
            // condition（branch/wait の params.condition）。検証したら再帰対象から外す（二重検証防止）。
            if ctx.is_some() {
                if let Some(cond) = map.get("condition") {
                    if let Ok(c) = serde_json::from_value::<Condition>(cond.clone()) {
                        check_condition(&c, &child(path, "condition"), ctx, errors);
                    }
                }
            }
            for (k, v) in map {
                if ctx.is_some() && k == "condition" {
                    continue; // 上で検証済み（$from の重複エラーを避ける）。
                }
                check_value(v, &child(path, k), ctx, errors);
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                check_value(v, &child(path, &i.to_string()), ctx, errors);
            }
        }
        _ => {}
    }
}

/// `$template` を検証する（差し込みの名前が vars にあるか・vars の中の参照）。
///
/// 実行時は vars に無い名前も、解決できない var も step の失敗になる（空文字で埋めない）。
fn check_template(
    t: &TemplateExpr,
    raw: &serde_json::Map<String, Value>,
    path: &str,
    ctx: Option<&NodeCtx<'_>>,
    errors: &mut Vec<ValidationError>,
) {
    let missing = missing_placeholders(t);
    if !missing.is_empty() {
        let names = missing
            .iter()
            .map(|n| format!("{{{n}}}"))
            .collect::<Vec<_>>()
            .join("・");
        let known = t
            .vars
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        errors.push(err(
            "ir.bad_template",
            format!(
                "$template の {names} が vars にありません（vars: {}）。波括弧そのものを出すなら {{{{ と }}}} と書きます",
                if known.is_empty() { "なし" } else { known.as_str() }
            ),
            path,
            ctx,
        ));
    }
    // vars の値も参照として検査する（source 名・祖先性・path の形）。生 JSON を見るので、形の崩れた
    // `$from`（リテラルに読まれたもの）も弾ける。
    if let Some(vars) = raw.get("vars").and_then(Value::as_object) {
        let vars_path = child(path, "vars");
        for (k, v) in vars {
            check_value(v, &child(&vars_path, k), ctx, errors);
        }
    }
}

/// `$from` の source を検証する（祖先性・default 要否・path の形）。
fn check_from(
    from: &FromRef,
    path: &str,
    ctx: Option<&NodeCtx<'_>>,
    errors: &mut Vec<ValidationError>,
) {
    // path は JSON Pointer（空なら source 全体）。`title` のように / が無いと実行時に必ず解決できない。
    if let Some(p) = from.path.as_deref() {
        if !p.is_empty() && !p.starts_with('/') {
            errors.push(err(
                "ir.bad_ref",
                format!("$from の path は / で始めます（'{p}' なら '/{p}'）"),
                path,
                ctx,
            ));
        }
    }
    let Some(ctx) = ctx else {
        // イベント filter は解決文脈が trigger（イベントのペイロード）しか無い。
        let head = from.from.split('.').next().unwrap_or("");
        if matches!(head, "nodes" | "each") {
            errors.push(err(
                "ir.bad_ref",
                format!(
                    "イベント filter は {} を参照できません（trigger のみ解決可能）",
                    from.from
                ),
                path,
                None,
            ));
        }
        return;
    };
    // source は閉集合へ **exact match**（`each.item` や `input.foo` のような「先頭一致だが
    // 実行時リゾルバが解決できない」source を保存時に弾く。中身の指定は path で行う）。
    let source = from.from.as_str();
    match source {
        "input" | "trigger" | "run" => { /* 静的に許可 */ }
        "each" => {
            // each.* は map 領域内（node.parent が map ノード）でのみ有効。領域外は実行時に
            // 現在要素が無く失敗するため保存時に弾く。
            if ctx.node.parent.is_none() {
                errors.push(err(
                    "ir.bad_ref",
                    format!(
                        "$from each.* は map 領域内でのみ使えます（node {} は領域外）",
                        ctx.node.id
                    ),
                    path,
                    Some(ctx),
                ));
            }
        }
        s if s.starts_with("nodes.") && s.ends_with(".output") => {
            // `nodes.<id>.output` の <id> は当該ノードの祖先必須。
            let referenced = source.split('.').nth(1).unwrap_or("");
            let is_ancestor = ctx.ancestors.is_some_and(|a| a.contains(referenced));
            if !is_ancestor && from.default.is_none() {
                errors.push(err(
                    "ir.bad_ref",
                    format!("$from nodes.{referenced} は祖先でなく default もありません"),
                    path,
                    Some(ctx),
                ));
            }
        }
        _ => errors.push(err(
            "ir.bad_ref",
            format!(
                "未知の $from source: {source}（input / trigger / run / each / nodes.<id>.output のみ。中身の指定は path を使う）"
            ),
            path,
            Some(ctx),
        )),
    }
}

/// 条件木を検証する（regex コンパイル・演算子と right の整合・値の中の参照）。
///
/// 値は実行時と同じく入れ子まで見る（`in` の候補の配列に置いた `$from` も検証する）。
fn check_condition(
    cond: &Condition,
    path: &str,
    ctx: Option<&NodeCtx<'_>>,
    errors: &mut Vec<ValidationError>,
) {
    match cond {
        Condition::All(cs) => {
            for (i, c) in cs.iter().enumerate() {
                check_condition(c, &format!("{path}/all/{i}"), ctx, errors);
            }
        }
        Condition::Any(cs) => {
            for (i, c) in cs.iter().enumerate() {
                check_condition(c, &format!("{path}/any/{i}"), ctx, errors);
            }
        }
        Condition::Not(c) => check_condition(c, &format!("{path}/not"), ctx, errors),
        Condition::Cmp(cmp) => check_cmp(cmp, &format!("{path}/cmp"), ctx, errors),
    }
}

fn check_cmp(
    cmp: &Comparison,
    path: &str,
    ctx: Option<&NodeCtx<'_>>,
    errors: &mut Vec<ValidationError>,
) {
    // 値式は直列化して生 JSON と同じ走査に掛ける（形の崩れた値式はリテラルのまま元の JSON に戻る）。
    let operand = |e: &ValueExpr| serde_json::to_value(e).unwrap_or(Value::Null);
    check_value(&operand(&cmp.left), &format!("{path}/left"), ctx, errors);
    if let Some(right) = &cmp.right {
        check_value(&operand(right), &format!("{path}/right"), ctx, errors);
    }
    // exists/is_null は right 不要、それ以外は right 必須。
    let needs_right = !matches!(cmp.op, CmpOp::Exists | CmpOp::IsNull);
    if needs_right && cmp.right.is_none() {
        errors.push(err(
            "ir.expr_type_error",
            "比較演算子に right がありません",
            path,
            ctx,
        ));
    }
    if !needs_right && cmp.right.is_some() {
        errors.push(err(
            "ir.expr_type_error",
            "exists/is_null は right を取りません",
            path,
            ctx,
        ));
    }
    // matches は right がリテラル文字列で、regex としてコンパイル可能・長さ上限内。
    if cmp.op == CmpOp::Matches {
        let msg = match &cmp.right {
            Some(ValueExpr::Literal(Value::String(pat))) => {
                if pat.len() > MAX_REGEX_LEN {
                    Some("regex パターンが長すぎます（最大 256）")
                } else if regex::Regex::new(pat).is_err() {
                    Some("regex がコンパイルできません")
                } else {
                    None
                }
            }
            _ => Some("matches の right はリテラル文字列である必要があります"),
        };
        if let Some(msg) = msg {
            errors.push(err("ir.expr_type_error", msg, path, ctx));
        }
    }
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

    /// 1 ノード（`a` → `b`）の `b` の params を V5 に通す。
    #[allow(clippy::needless_pass_by_value)]
    fn check_b(params: serde_json::Value) -> Vec<ValidationError> {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "b", "type": "script.run", "params": params },
                    { "id": "c", "type": "storage.read", "params": {} }
                ],
                "edges": [{ "from": "a", "to": "b" }]
            })),
            &mut errs,
        );
        errs
    }

    /// vars の中の `$from` も source 名・祖先性・path の形を検証する（以前は $template で走査を止めていた）。
    #[test]
    fn template_vars_are_checked_as_refs() {
        let ok = check_b(json!({ "input": { "$template": "{t} {x}", "vars": {
            "t": { "$from": "nodes.a.output", "path": "/text" }, "x": 1 } } }));
        assert!(ok.is_empty(), "{ok:?}");
        for (var, want) in [
            (json!({ "$from": "nodes.c.output" }), "祖先でなく"),
            (json!({ "$from": "ask" }), "未知の $from source"),
            (json!({ "$from": "input", "path": "title" }), "/ で始めます"),
            (json!({ "$from": "input", "typo": 1 }), "不正な $from"),
        ] {
            let errs = check_b(json!({ "input": { "$template": "{t}", "vars": { "t": var } } }));
            assert_eq!(errs.len(), 1, "{errs:?}");
            assert_eq!(errs[0].code, "ir.bad_ref");
            assert!(errs[0].message.contains(want), "{}", errs[0].message);
            assert_eq!(errs[0].node_id.as_deref(), Some("b"));
            assert_eq!(errs[0].path.as_deref(), Some("/params/input/vars/t"));
        }
    }

    /// 実行時は vars に無い名前で失敗するので、保存時に弾く（UI の既定名 v1 のまま・JSON 本文の素の `{`）。
    #[test]
    fn placeholder_missing_from_vars_is_bad_template() {
        let errs = check_b(json!({ "input": { "$template": "件名: {title}",
            "vars": { "v1": { "$from": "input", "path": "/title" } } } }));
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.bad_template");
        assert!(errs[0].message.contains("{title}") && errs[0].message.contains("v1"));
        assert_eq!(errs[0].path.as_deref(), Some("/params/input"));
        let json_body = check_b(json!({ "input": { "$template": "{\"text\": \"{m}\"}",
            "vars": { "m": "x" } } }));
        assert_eq!(json_body[0].code, "ir.bad_template");
        // `{{key}}` はエスケープ（差し込みではない）ので V5 のエラーではない（警告は semantics_warnings）。
        assert!(
            check_b(json!({ "input": { "$template": "{{t}}", "vars": { "t": 1 } } })).is_empty()
        );
    }

    #[test]
    fn from_path_must_be_a_json_pointer() {
        let errs = check_b(json!({ "input": { "$from": "input", "path": "title" } }));
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].path.as_deref(), Some("/params/input"));
        // 入れ子の位置も JSON Pointer で返す（キーの / と ~ はエスケープ）。
        let nested = check_b(json!({ "input": { "a/b": [ { "$from": "input", "path": "x" } ] } }));
        assert_eq!(nested[0].path.as_deref(), Some("/params/input/a~1b/0"));
        assert!(check_b(json!({ "input": { "$from": "input", "path": "" } })).is_empty());
    }

    /// 条件の値も実行時と同じく入れ子まで見る（`in` の候補の配列に置いた `$from`）。
    #[test]
    fn condition_operands_are_checked_deeply() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "br", "type": "control.branch", "params": { "condition": { "any": [
                        { "cmp": { "left": { "$from": "input", "path": "/who" }, "op": "in",
                                   "right": [ { "$from": "nodes.a.output", "path": "/x" }, "bob" ] } }
                    ] } } }
                ],
                "edges": []
            })),
            &mut errs,
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.bad_ref");
        assert_eq!(
            errs[0].path.as_deref(),
            Some("/params/condition/any/0/cmp/right/0")
        );

        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "triggers": [{
                    "kind": "event", "source": "storage.write", "scope": {},
                    "filter": { "cmp": { "left": { "$from": "trigger", "path": "/a" }, "op": "in",
                        "right": [ { "$template": "{n}", "vars": { "n": { "$from": "nodes.x.output" } } } ] } }
                }],
                "nodes": [], "edges": []
            })),
            &mut errs,
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0].message.contains("イベント filter"),
            "{}",
            errs[0].message
        );
        assert_eq!(
            errs[0].path.as_deref(),
            Some("/triggers/0/filter/cmp/right/0/vars/n")
        );
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
