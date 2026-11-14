//! `$template` の保存時検証（ir.md §3.2・§8）。字句は実行時の組み立てと同じ [`template_pieces`] で読む。
//!
//! - V5: `{name}` が vars のキーにあること（[`missing_placeholders`]。vars の中の参照は V5 の走査が見る）
//! - V7: vars の数が [`MAX_TEMPLATE_VARS`] 以下であること（[`check_limits`]）
//! - 保存を止めない警告: vars のキーを `{{key}}` と書いた（波括弧そのものが出て差し込まれない）・閉じの無い `{`
//!   （差し込みにならず文字のまま出る・D-37）（[`warnings`]）

use serde_json::{Map, Value};

use super::ValidationError;
use crate::ir::expr::{template_pieces, FromRef, TemplateExpr, TemplatePiece, MAX_TEMPLATE_VARS};
use crate::ir::{Trigger, WorkflowIr};

/// 警告のコード: `{{key}}`（エスケープした波括弧）で vars のキーを囲んでいる。
pub const TEMPLATE_ESCAPED_VAR: &str = "ir.template_escaped_var";
/// 警告のコード: 閉じの無い `{` がある（差し込みにならず文字のまま出る・D-37）。
pub const TEMPLATE_UNCLOSED_BRACE: &str = "ir.template_unclosed_brace";

/// JSON Pointer の子の位置（`~` と `/` をエスケープする）。
pub(super) fn child(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// `$template` の差し込み（vars の値）の `$from`（形の崩れたものと `$from` でない値は `None`）と、書いた
/// `"default": null` を外したか。
///
/// 差し込みの null は空の文字にならず、値が無いものとして step を失敗させる（実行の `resolve_template`・ir.md §3.1）
/// ので、差し込みの `"default": null` は既定値にならない。保存の照合（祖先でない参照の default の要否・解決しない
/// ときの振る舞いの文）と入力の参照の注意は、既定値なしとして読む（BUG-288 で null を既定値として残した後も、
/// 差し込みでは前と同じに止める）。
pub(super) fn template_var_ref(v: &Value) -> Option<(FromRef, bool)> {
    if !v.as_object()?.contains_key("$from") {
        return None;
    }
    let mut from: FromRef = serde_json::from_value(v.clone()).ok()?;
    let null_default = from.default.as_ref().is_some_and(Value::is_null);
    if null_default {
        from.default = None;
    }
    Some((from, null_default))
}

/// vars に無いプレースホルダの一覧（重複は 1 つにまとめる・空なら問題なし）。
pub(super) fn missing_placeholders(t: &TemplateExpr) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for piece in template_pieces(&t.template) {
        if let TemplatePiece::Var(name) = piece {
            if !t.vars.contains_key(name) && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// `$template` のオブジェクト（`{"$template": …, "vars": …}`）。
type TemplateObject = Map<String, Value>;
/// 訪問先: `f(ノード id, 位置, $template のオブジェクト)`（イベント filter はノード id なし）。
type VisitTemplate<'f> = &'f mut dyn FnMut(Option<&str>, &str, &TemplateObject);
/// 1 つの値の中の訪問先: `f(位置, $template のオブジェクト)`。
type VisitInValue<'f> = &'f mut dyn FnMut(&str, &TemplateObject);

/// IR の中の `$template` をすべて訪ねる（ノードの params とイベント filter）。
///
/// vars の中に入れ子にした `$template` も訪ねる。
fn for_each_template(ir: &WorkflowIr, f: VisitTemplate<'_>) {
    for node in &ir.nodes {
        walk(&node.params, "/params", &mut |path, t| {
            f(Some(&node.id), path, t);
        });
    }
    for (i, t) in ir.triggers.iter().enumerate() {
        if let Trigger::Event(ev) = t {
            if let Some(filter) = ev
                .filter
                .as_ref()
                .and_then(|c| serde_json::to_value(c).ok())
            {
                walk(&filter, &format!("/triggers/{i}/filter"), &mut |path, t| {
                    f(None, path, t);
                });
            }
        }
    }
}

fn walk(v: &Value, path: &str, f: VisitInValue<'_>) {
    match v {
        Value::Object(map) if map.contains_key("$template") => {
            f(path, map);
            if let Some(vars) = map.get("vars").and_then(Value::as_object) {
                let vars_path = child(path, "vars");
                for (k, x) in vars {
                    walk(x, &child(&vars_path, k), f);
                }
            }
        }
        // `$from` の default はリテラルとして返る（中を値式として解決しない）。
        Value::Object(map) if map.contains_key("$from") => {}
        Value::Object(map) => {
            for (k, x) in map {
                walk(x, &child(path, k), f);
            }
        }
        Value::Array(items) => {
            for (i, x) in items.iter().enumerate() {
                walk(x, &child(path, &i.to_string()), f);
            }
        }
        _ => {}
    }
}

/// V7: テンプレートの vars の数の上限（ir.md §8・既定 50）。
pub(super) fn check_limits(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    for_each_template(ir, &mut |node_id, path, t| {
        let n = t.get("vars").and_then(Value::as_object).map_or(0, Map::len);
        if n > MAX_TEMPLATE_VARS {
            let mut e = ValidationError::new(
                "ir.limit_exceeded",
                format!("$template の vars が多すぎます（{n} 個・最大 {MAX_TEMPLATE_VARS}）"),
            )
            .at_path(path);
            if let Some(id) = node_id {
                e = e.at_node(id);
            }
            errors.push(e);
        }
    });
}

/// 保存を止めない警告（テンプレートの書き方の誤りらしいもの）。
///
/// - vars のキーを `{{key}}` と書いた（差し込まれず `{key}` の文字が出る）。n8n や Dify の書き方に慣れた
///   利用者・AI が書きがち。`{{ key }}` のように空白を挟んだものも拾う。
/// - 閉じの無い `{`（`a { b`）。実行時は文字のまま出す（エラーにはしない・D-37）が、`{name` の閉じ忘れは
///   差し込んだつもりで差し込まれないので知らせる。字句は実行時と同じ [`template_pieces`] で読み、
///   文字の字句に残った `{` を閉じの無いものとみなす（`{{` と閉じた `{…}` は別の字句になる）。
#[must_use]
pub(super) fn warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = Vec::new();
    for_each_template(ir, &mut |node_id, path, raw| {
        let Ok(t) = serde_json::from_value::<TemplateExpr>(Value::Object(raw.clone())) else {
            return; // 形の崩れは V5 のエラー。
        };
        let pieces = template_pieces(&t.template);
        if pieces
            .iter()
            .any(|p| matches!(p, TemplatePiece::Text(text) if text.contains('{')))
        {
            let mut e = ValidationError::new(
                TEMPLATE_UNCLOSED_BRACE,
                "閉じの無い { があります（差し込みにならず、文字のまま出ます）。差し込むなら {名前} と閉じ、波括弧そのものを出すなら {{ と書きます",
            )
            .at_path(path);
            if let Some(id) = node_id {
                e = e.at_node(id);
            }
            out.push(e);
        }
        for w in pieces.windows(3) {
            let [TemplatePiece::Escaped('{'), TemplatePiece::Text(key), TemplatePiece::Escaped('}')] =
                w
            else {
                continue;
            };
            let key = key.trim();
            if !t.vars.contains_key(key) {
                continue;
            }
            let mut e = ValidationError::new(
                TEMPLATE_ESCAPED_VAR,
                format!(
                    "{{{{{key}}}}} は波括弧そのものとして出ます（「{{{key}}}」という文字になります）。差し込むなら {{{key}}} と書きます"
                ),
            )
            .at_path(path);
            if let Some(id) = node_id {
                e = e.at_node(id);
            }
            out.push(e);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)]
    fn ir(v: Value) -> WorkflowIr {
        WorkflowIr::from_json(&v).unwrap()
    }

    #[allow(clippy::needless_pass_by_value)]
    fn script_with_input(input: Value) -> WorkflowIr {
        ir(json!({
            "ir_version": 1, "name": "wf",
            "nodes": [ { "id": "echo", "type": "script.run", "params": {
                "source": { "inline": "function main(i){ return { got: i }; }" },
                "input": input } } ],
            "edges": []
        }))
    }

    #[test]
    fn missing_placeholders_lists_names_not_in_vars() {
        let t: TemplateExpr = serde_json::from_value(json!({
            "$template": "{title} {v1} {title} {{literal}}", "vars": { "v1": "x" } }))
        .unwrap();
        assert_eq!(missing_placeholders(&t), vec!["title"]);
    }

    /// BUG-1 の AI 生成の形: `{{title}}` は差し込みではない。保存は止めず警告で知らせる。
    #[test]
    fn warns_on_mustache_style_vars() {
        let w = warnings(&script_with_input(json!({
            "$template": "件名: {{title}} / 金額: {{ amount }} 円 / {{{title}}} / {{other}}",
            "vars": { "title": { "$from": "input", "path": "/title" },
                      "amount": { "$from": "input", "path": "/amount" } } })));
        // {{{title}}} は「{」＋差し込み＋「}」なので対象外。vars に無い {{other}} も対象外。
        assert_eq!(w.len(), 2, "{w:#?}");
        assert!(w.iter().all(|e| e.code == TEMPLATE_ESCAPED_VAR));
        assert_eq!(w[0].node_id.as_deref(), Some("echo"));
        assert_eq!(w[0].path.as_deref(), Some("/params/input"));
        assert!(w[0].message.contains("{{title}}") && w[0].message.contains("{title} と書きます"));
        assert!(w[1].message.contains("{amount}"), "{}", w[1].message);
    }

    /// D-37: 閉じの無い `{` は保存を止めずに知らせる（1 つのテンプレートに 1 件）。`{{`・閉じた差し込み・
    /// 対にならない `}` は対象外。
    #[test]
    fn warns_on_unclosed_brace() {
        let w = warnings(&script_with_input(json!({
            "$template": "件名: {title / 金額 { 円", "vars": { "title": "x" } })));
        assert_eq!(w.len(), 1, "{w:#?}");
        assert_eq!(w[0].code, TEMPLATE_UNCLOSED_BRACE);
        assert_eq!(w[0].node_id.as_deref(), Some("echo"));
        assert_eq!(w[0].path.as_deref(), Some("/params/input"));
        for ok in ["{title} {{literal}} x}y", "{{ だけ", "閉じた {title}"] {
            let w = warnings(&script_with_input(
                json!({ "$template": ok, "vars": { "title": "x" } }),
            ));
            assert!(
                w.iter().all(|e| e.code != TEMPLATE_UNCLOSED_BRACE),
                "{ok}: {w:#?}"
            );
        }
        // 閉じの無い `{` の後ろに `{{` があっても拾う（エスケープは別の字句）。
        let w = warnings(&script_with_input(
            json!({ "$template": "a { b {{c", "vars": {} }),
        ));
        assert_eq!(w.len(), 1, "{w:#?}");
    }

    #[test]
    fn nested_templates_are_visited_with_their_position() {
        let w = warnings(&script_with_input(json!({
            "lines": [ { "$template": "{{n}}", "vars": { "n": 1 } } ] })));
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].path.as_deref(), Some("/params/input/lines/0"));
    }

    #[test]
    fn v7_limits_template_vars() {
        let vars: Map<String, Value> = (0..=MAX_TEMPLATE_VARS)
            .map(|i| (format!("v{i}"), json!(i)))
            .collect();
        let mut errs = Vec::new();
        check_limits(
            &script_with_input(json!({ "$template": "x", "vars": vars })),
            &mut errs,
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.limit_exceeded");
        assert_eq!(errs[0].node_id.as_deref(), Some("echo"));
        let mut ok = Vec::new();
        check_limits(
            &script_with_input(json!({ "$template": "{a}", "vars": { "a": 1 } })),
            &mut ok,
        );
        assert!(ok.is_empty());
    }
}
