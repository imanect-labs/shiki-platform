//! V5: データフロー（`$from` 祖先性・default 要否・path の形・`$template` の差し込み・条件木型整合・
//! regex コンパイル・ir.md §8）。
//!
//! 値の位置は実行時の解決（`control::eval::resolve_deep`）と同じ範囲を見る: リテラルの object / array の中・
//! `$template` の vars の中・条件の値（`in` の候補の配列など）の中の `$from` も検証する。`nodes.<id>.output` と
//! `each` の path は、出力の形から決まる範囲で照らす（[`output_refs`]・GAP-137）。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

mod output_refs;
use output_refs::{OutputShapes, Place, UNKNOWN_OUTPUT_PATH};

use super::template::{child, missing_placeholders, template_var_ref};
use super::ValidationError;
use crate::ir::expr::{
    CmpOp, Comparison, Condition, FromRef, TemplateExpr, ValueExpr, MAX_REGEX_LEN,
};
use crate::ir::{Node, WorkflowIr};

/// V5 検証を実行する。
pub(super) fn check(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    let ancestors = compute_ancestors(ir);
    let shapes = OutputShapes::new(ir);
    for node in &ir.nodes {
        let ctx = NodeCtx {
            node,
            ancestors: ancestors.get(node.id.as_str()),
            event_refs: false,
            shapes: &shapes,
        };
        // wait(event) の filter は、到来したイベント（event・互換の別名 trigger / payload）と、停止時に
        // リテラルへ束縛する参照（input・nodes.*・run・each）を書ける（GAP-07 の T3）。
        if node.node_type == crate::vocab::NodeType::ControlWait.as_str() {
            if let Some(filter) = node.params.get("filter") {
                let in_filter = NodeCtx {
                    event_refs: true,
                    ..ctx
                };
                check_value(filter, "/params/filter", Some(&in_filter), errors);
            }
            if let Some(params) = node.params.as_object() {
                for (k, v) in params.iter().filter(|(k, _)| k.as_str() != "filter") {
                    check_value(v, &child("/params", k), Some(&ctx), errors);
                }
                continue;
            }
        }
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

#[derive(Clone, Copy)]
struct NodeCtx<'a> {
    node: &'a Node,
    /// このノードの祖先ノード id 集合（`$from: nodes.<id>` の祖先性検証用）。
    ancestors: Option<&'a HashSet<String>>,
    /// wait(event) の filter の中か（到来したイベント `event` を参照できる）。
    event_refs: bool,
    /// `$from` の path を照らす出力の形（GAP-137）。
    shapes: &'a OutputShapes<'a>,
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
                    Ok(from) => check_from(&from, path, ctx, Place::Value, errors),
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
    // `$from`（リテラルに読まれたもの）も弾ける。差し込みの `"default": null` は既定値にならない（解決できずに
    // 失敗する）ので、既定値なしとして照らし、誤りの文で言い添える。
    if let Some(vars) = raw.get("vars").and_then(Value::as_object) {
        let vars_path = child(path, "vars");
        for (k, v) in vars {
            let at = child(&vars_path, k);
            let Some((from, null_default)) = template_var_ref(v) else {
                check_value(v, &at, ctx, errors);
                continue;
            };
            let before = errors.len();
            check_from(&from, &at, ctx, Place::Value, errors);
            if null_default {
                for e in &mut errors[before..] {
                    e.message.push_str(TEMPLATE_NULL_DEFAULT);
                }
            }
        }
    }
}

/// 差し込みの `"default": null` を書いた参照の誤りに添える文。
const TEMPLATE_NULL_DEFAULT: &str = "（$template の差し込みでは、default の null は値が無いのと同じで、解決できずに失敗します。空の文字にするなら \"default\": \"\" と書きます）";

/// `$from` の source を検証する（祖先性・default 要否・path の形・出力の形から決まる path の有無）。
///
/// `place` は参照の置き場所（分岐の条件の比較の値そのものか・ほかの値の位置か）で、出力に無い所を指す誤りの文が
/// 実行の振る舞いを言い分けるのに使う。
fn check_from(
    from: &FromRef,
    path: &str,
    ctx: Option<&NodeCtx<'_>>,
    place: Place,
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
        // wait(event) の filter では、到来したイベントを指す（trigger も filter の中ではイベントを指す）。
        "event" | "payload" if ctx.event_refs => {}
        "each" => {
            // each.* は map 領域内（node.parent が map ノード）でのみ有効。領域外は実行時に
            // 現在要素が無く失敗するため保存時に弾く。
            if ctx.node.parent.is_none() {
                errors.push(err(
                    "ir.bad_ref",
                    format!(
                        "「繰り返し中の 1 件」は、「{}」の中のブロックでだけ使えます。",
                        crate::vocab::label_ja(crate::vocab::NodeType::ControlMap)
                    ),
                    path,
                    Some(ctx),
                ));
            } else {
                check_output_path(from, path, ctx, place, errors);
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
            } else {
                check_output_path(from, path, ctx, place, errors);
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

/// 参照の path が、出力の形から決まる範囲で出力に無い所を指していれば `ir.unknown_output_path`（GAP-137）。
///
/// 参照そのものの誤り（祖先でない・map の外の each）を言ったときは呼ばない（1 つの参照に誤りを重ねない）。
fn check_output_path(
    from: &FromRef,
    path: &str,
    ctx: &NodeCtx<'_>,
    place: Place,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(msg) = ctx.shapes.check(ctx.node, from, place) {
        errors.push(err(UNKNOWN_OUTPUT_PATH, msg, path, Some(ctx)));
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
    // 比較の値そのものの参照は、解決しなければ値が無いものとして比べる（`control::eval` の `resolve_operand`）。
    // ほかの値式は直列化して生 JSON と同じ走査に掛ける（形の崩れた値式はリテラルのまま元の JSON に戻る）。
    let operand = |e: &ValueExpr, at: &str, errors: &mut Vec<ValidationError>| match e {
        ValueExpr::From(f) => check_from(f, at, ctx, Place::Condition, errors),
        other => check_value(
            &serde_json::to_value(other).unwrap_or(Value::Null),
            at,
            ctx,
            errors,
        ),
    };
    operand(&cmp.left, &format!("{path}/left"), errors);
    if let Some(right) = &cmp.right {
        operand(right, &format!("{path}/right"), errors);
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
    // matches は right がリテラル文字列で、regex としてコンパイル可能・長さ上限内。長さは入力項目の pattern と同じく
    // 文字数で数える（BUG-228）。
    if cmp.op == CmpOp::Matches {
        let msg = match &cmp.right {
            Some(ValueExpr::Literal(Value::String(pat))) => {
                if pat.chars().count() > MAX_REGEX_LEN {
                    Some(format!("正規表現は {MAX_REGEX_LEN} 字までにしてください。"))
                } else if regex::Regex::new(pat).is_err() {
                    Some("正規表現の書き方が正しくありません。".to_string())
                } else {
                    None
                }
            }
            _ => Some("matches の right はリテラル文字列である必要があります".to_string()),
        };
        if let Some(msg) = msg {
            errors.push(err("ir.expr_type_error", msg, path, ctx));
        }
    }
}

/// 各ノードの祖先ノード集合を求める（エッジの推移閉包・逆向き到達）。
///
/// 繰り返し（map）の中のブロックは、繰り返しのブロックの祖先も祖先に数える（入れ子なら外側の繰り返しの祖先も・
/// D-123）。中のブロックが動くのは繰り返しのブロックが動いた後なので、その祖先の結果はもう出ている。実行時も
/// 本体と外側の要素の結果を中に見せる（`RunStore::step_outputs`）。繰り返しのブロックそのものは数えない（中が
/// 終わるまで結果が無い）。領域の閉包（V2）で線は同じ領域の中にしか引けないので、線でたどれるのは同じ領域の
/// ブロックだけになる。
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
    let parent_of: HashMap<&str, &str> = ir
        .nodes
        .iter()
        .filter_map(|n| n.parent.as_deref().map(|p| (n.id.as_str(), p)))
        .collect();
    let mut result: HashMap<String, HashSet<String>> = HashMap::new();
    for n in &ir.nodes {
        let mut seen: HashSet<String> = HashSet::new();
        // 自分と、自分を囲む繰り返し（内から外へ）の前段から逆向きにたどる。親の連鎖が輪になっていても
        // （V2 が誤りにする）止まるよう、たどった繰り返しを覚える。
        let mut enclosing: HashSet<&str> = HashSet::new();
        let mut cur = Some(n.id.as_str());
        let mut stack: Vec<&str> = Vec::new();
        while let Some(c) = cur {
            if !enclosing.insert(c) {
                break;
            }
            stack.extend(preds.get(c).into_iter().flatten().copied());
            cur = parent_of.get(c).copied();
        }
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

    /// 繰り返しの中のブロックが `nodes.<id>.output` で指したブロックごとの V5 の誤り（ノード id の一覧）。
    #[allow(clippy::needless_pass_by_value)]
    fn bad_refs(v: serde_json::Value) -> Vec<String> {
        let mut errs = Vec::new();
        check(&ir(v), &mut errs);
        errs.iter()
            .map(|e| {
                assert_eq!(e.code, "ir.bad_ref", "{e:?}");
                e.node_id.clone().unwrap_or_default()
            })
            .collect()
    }

    fn out(id: &str) -> serde_json::Value {
        json!({ "$from": format!("nodes.{id}.output"), "path": "/items" })
    }

    /// 繰り返しの中のブロックは、繰り返しの手前のブロック（線でたどれる祖先）の結果を参照できる（D-123・BUG-152）。
    /// 線の無い外のブロック・繰り返しの後ろのブロック・繰り返しのブロックそのものは祖先ではない。
    #[test]
    fn inside_a_map_the_map_ancestors_are_ancestors() {
        let wf = |inside_ref: serde_json::Value| {
            json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "approvers", "type": "data.query", "params": {} },
                    { "id": "before", "type": "storage.read", "params": {} },
                    { "id": "loose", "type": "storage.read", "params": {} },
                    { "id": "m", "type": "control.map", "params": { "items": out("before") } },
                    { "id": "first", "type": "script.run", "parent": "m",
                      "params": { "input": { "$from": "each", "path": "/item" } } },
                    { "id": "write", "type": "data.record.create", "parent": "m",
                      "params": { "table": "t", "data": inside_ref } },
                    { "id": "after", "type": "storage.read", "params": {} }
                ],
                "edges": [
                    { "from": "approvers", "to": "before" },
                    { "from": "before", "to": "m" },
                    { "from": "m", "to": "after" },
                    { "from": "first", "to": "write" }
                ]
            })
        };
        // 繰り返しの手前（線で 2 段前まで）・中の前のブロックは参照できる。
        for ok in ["approvers", "before", "first"] {
            assert!(bad_refs(wf(out(ok))).is_empty(), "{ok}");
        }
        // 線の無い外のブロック・繰り返しの後ろ・繰り返しそのもの（中が終わるまで結果が無い）は default が要る。
        for ng in ["loose", "after", "m"] {
            assert_eq!(bad_refs(wf(out(ng))), vec!["write".to_string()], "{ng}");
            let with_default = json!({ "$from": format!("nodes.{ng}.output"), "default": [] });
            assert!(bad_refs(wf(with_default)).is_empty(), "{ng}");
            // null の既定値も既定値（書いた `"default": null` を書いていないものとして扱わない）。
            let null_default = json!({ "$from": format!("nodes.{ng}.output"), "default": null });
            assert!(bad_refs(wf(null_default)).is_empty(), "{ng} null");
        }
    }

    /// `$template` の差し込みの `"default": null` は既定値にならない（差し込みの null は解決できずに失敗する・
    /// 実行の `resolve_template`）。祖先でない参照は止め、文で言い添える。空の文字の既定値は通り、祖先の参照の
    /// null の既定値も（前と同じく）止めない。
    #[test]
    fn a_null_default_in_a_template_var_is_no_default() {
        let wf = |from: &str, default: serde_json::Value| {
            json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "a", "type": "storage.read", "params": {} },
                    { "id": "loose", "type": "storage.read", "params": {} },
                    { "id": "b", "type": "storage.read", "params": { "id": {
                        "$template": "[{d}]",
                        "vars": { "d": { "$from": format!("nodes.{from}.output"), "path": "/x", "default": default } }
                    } } }
                ],
                "edges": [{ "from": "a", "to": "b" }]
            })
        };
        let errs = |v: serde_json::Value| {
            let mut errs = Vec::new();
            check(&ir(v), &mut errs);
            errs
        };
        let null_loose = errs(wf("loose", json!(null)));
        assert_eq!(null_loose.len(), 1, "{null_loose:?}");
        assert_eq!(null_loose[0].code, "ir.bad_ref");
        assert_eq!(null_loose[0].path.as_deref(), Some("/params/id/vars/d"));
        assert!(
            null_loose[0].message.ends_with(TEMPLATE_NULL_DEFAULT),
            "{null_loose:?}"
        );
        assert!(errs(wf("loose", json!(""))).is_empty());
        assert!(errs(wf("a", json!(null))).is_empty());
        // 差し込みでない値の位置の null の既定値は既定値のまま（項目を空にする書込など）。
        let plain = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "loose", "type": "storage.read", "params": {} },
                { "id": "b", "type": "storage.read", "params": {
                    "id": { "$from": "nodes.loose.output", "path": "/x", "default": null } } }
            ],
            "edges": []
        });
        assert!(errs(plain).is_empty());
    }

    /// 入れ子の繰り返しの中は、内側の繰り返しの祖先（外側の中のブロック）と、外側の繰り返しの祖先（本体）を
    /// たどる。内側の中のブロックは外側の中から参照できない（内側が終わるまで結果が無い）。
    #[test]
    fn nested_maps_reach_the_ancestors_of_every_enclosing_map() {
        let wf = |leaf_ref: serde_json::Value, outer_ref: serde_json::Value| {
            json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "list", "type": "storage.list", "params": {} },
                    { "id": "loose", "type": "storage.read", "params": {} },
                    { "id": "outer", "type": "control.map", "params": { "items": out("list") } },
                    { "id": "read", "type": "storage.read", "parent": "outer", "params": {} },
                    { "id": "sibling", "type": "storage.read", "parent": "outer", "params": {} },
                    { "id": "inner", "type": "control.map", "parent": "outer",
                      "params": { "items": out("read") } },
                    { "id": "done", "type": "script.run", "parent": "outer",
                      "params": { "input": outer_ref } },
                    { "id": "leaf", "type": "script.run", "parent": "inner",
                      "params": { "input": leaf_ref } }
                ],
                "edges": [
                    { "from": "list", "to": "outer" },
                    { "from": "read", "to": "inner" },
                    { "from": "inner", "to": "done" }
                ]
            })
        };
        let none = json!({ "$from": "each", "path": "/item" });
        // 内側の中: 内側の祖先（外側の中の read）と外側の祖先（本体の list）。
        for ok in ["read", "list"] {
            assert!(bad_refs(wf(out(ok), none.clone())).is_empty(), "{ok}");
        }
        // 外側の中でも線の無いブロック・本体の線の無いブロック・囲む繰り返しは祖先ではない。
        for ng in ["sibling", "loose", "inner", "outer"] {
            assert_eq!(
                bad_refs(wf(out(ng), none.clone())),
                vec!["leaf".to_string()],
                "{ng}"
            );
        }
        // 外側の中の done は、内側の繰り返し（線の前段）を参照でき、内側の中の leaf は参照できない。
        assert!(bad_refs(wf(none.clone(), out("inner"))).is_empty());
        assert_eq!(
            bad_refs(wf(none.clone(), out("leaf"))),
            vec!["done".to_string()]
        );
    }

    /// 親の連鎖が輪になった IR（V2 が誤りにする）でも、祖先をたどる処理は止まる。
    #[test]
    fn a_cyclic_parent_chain_does_not_hang() {
        let errs = bad_refs(json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "a", "type": "control.map", "parent": "b", "params": { "items": [] } },
                { "id": "b", "type": "control.map", "parent": "a", "params": { "items": [] } },
                { "id": "c", "type": "script.run", "parent": "a",
                  "params": { "input": out("x") } }
            ],
            "edges": []
        }));
        assert_eq!(errs, vec!["c".to_string()]);
    }

    /// 繰り返し中の 1 件（`each`）は繰り返しの中のブロックだけが使える。外で使うと、内部の名前（each・map・
    /// node の id）を出さずに言う（付録 D の N3.1.4・D-98）。
    #[test]
    fn each_is_only_for_blocks_inside_a_map() {
        let mut errs = Vec::new();
        check(
            &ir(json!({
                "ir_version": 1, "name": "wf",
                "nodes": [
                    { "id": "m", "type": "control.map", "params": { "items": [] } },
                    { "id": "inside", "type": "storage.read", "parent": "m",
                      "params": { "id": { "$from": "each", "path": "/item" } } },
                    { "id": "outside", "type": "storage.read",
                      "params": { "id": { "$from": "each", "path": "/item" } } }
                ],
                "edges": []
            })),
            &mut errs,
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].node_id.as_deref(), Some("outside"));
        assert_eq!(errs[0].path.as_deref(), Some("/params/id"));
        assert_eq!(
            errs[0].message,
            "「繰り返し中の 1 件」は、「繰り返し」の中のブロックでだけ使えます。"
        );
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

    /// BUG-228: 「正規表現に一致」の長さは、入力項目の pattern と同じく文字数で数え、文言の数と合わせる。日本語の
    /// 256 文字は通り（前はバイトで数えて 86 文字ほどで断っていた）、257 文字は断る。
    #[test]
    fn matches_length_is_counted_in_characters() {
        let errs_for = |pat: String| {
            let mut errs = Vec::new();
            check(
                &ir(json!({
                    "ir_version": 1, "name": "wf",
                    "nodes": [{ "id": "b", "type": "control.branch", "params": { "condition":
                        { "cmp": { "left": "x", "op": "matches", "right": pat } } } }],
                    "edges": []
                })),
                &mut errs,
            );
            errs.into_iter()
                .map(|e| (e.code, e.message))
                .collect::<Vec<_>>()
        };
        assert_eq!(errs_for("あ".repeat(MAX_REGEX_LEN)), vec![]);
        assert_eq!(
            errs_for("あ".repeat(MAX_REGEX_LEN + 1)),
            vec![(
                "ir.expr_type_error".to_string(),
                "正規表現は 256 字までにしてください。".to_string()
            )]
        );
        assert_eq!(
            errs_for("(".to_string()),
            vec![(
                "ir.expr_type_error".to_string(),
                "正規表現の書き方が正しくありません。".to_string()
            )]
        );
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
