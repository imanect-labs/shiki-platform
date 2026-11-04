//! 保存を止めない警告: 入力（`$from: input`）の path が、入力項目（`input_schema`）に無い項目を指す（GAP-35）。
//!
//! 手動で実行するときのダイアログ・ミニアプリの部品の候補は入力項目から欄を作るので、入力項目に無い項目には値を
//! 入れる欄が出ない（API や画面のボタンが名前で送れば値は入る。入力項目が閉じていなければ起動の検査も通す）。
//! 書き損じ（AI が作ったワークフローの `/amount` と `金額` など）を保存のときに知らせる。止めはしない（保存済みの
//! 定義に、名前で送る使い方のものがあり得る）。
//!
//! 決まりはここだけに置く。ワークフローのエディターは、この注意を参照の欄の下と入力項目の表（残った参照の一覧・
//! 「項目に足す」）に写す（web は同じ判定を持たない）。
//!
//! 見るのは、入力項目が項目（`properties`）を決めているときだけ。形を追えない位置（`properties` も `items` も無い・
//! 根が true）から先は言わない。`additionalProperties: true` と書いた入れ物は、無い項目も受けると決めてあるので言わない。

use serde_json::Value;

use super::template::child;
use super::ValidationError;
use crate::ir::expr::FromRef;
use crate::ir::WorkflowIr;

/// 警告のコード: 入力の参照が、入力項目に無い項目を指す。
pub const UNKNOWN_INPUT: &str = "ir.unknown_input";

/// 入力項目に無い項目を指す入力の参照（ブロックの設定の中の位置つき）。
pub(super) fn warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let Some(schema) = ir.input_schema.as_ref() else {
        return Vec::new();
    };
    if !schema.get("properties").is_some_and(Value::is_object) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for node in &ir.nodes {
        let mut found = Vec::new();
        refs(&node.params, "/params", &mut found);
        for (at, from) in found {
            let Some(pointer) = from.path.as_deref() else {
                continue;
            };
            if let Some(key) = missing_key(schema, pointer) {
                let then = if from.default.is_some() {
                    "値が入らないときは既定値を使います"
                } else {
                    "値が入らないと、このブロックは失敗します"
                };
                out.push(
                    ValidationError::new(
                        UNKNOWN_INPUT,
                        format!(
                            "「{key}」は入力項目にありません。手動で実行するときに欄が出ないため、{then}。"
                        ),
                    )
                    .at_node(&node.id)
                    .at_path(at),
                );
            }
        }
    }
    out
}

/// 値の中の入力の参照（`$from: input`）と、その位置。`$template` の vars・条件の値・入れ子のリテラルの中も見る
/// （実行時の解決と同じ範囲）。
fn refs(value: &Value, path: &str, out: &mut Vec<(String, FromRef)>) {
    match value {
        Value::Object(map) => {
            if map.contains_key("$from") {
                // 形の崩れた参照は V5 が誤りにする。
                if let Ok(from) = serde_json::from_value::<FromRef>(value.clone()) {
                    if from.from == "input" {
                        out.push((path.to_string(), from));
                    }
                }
                return;
            }
            for (k, v) in map {
                refs(v, &child(path, k), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                refs(v, &child(path, &i.to_string()), out);
            }
        }
        _ => {}
    }
}

/// path の中で、入力項目に無い最初の項目のキー（無い項目が無い・形を追えないなら `None`）。
fn missing_key(schema: &Value, pointer: &str) -> Option<String> {
    if !pointer.starts_with('/') {
        return None; // 形の誤りは V5 が言う。
    }
    let mut s = schema;
    for raw in pointer.split('/').skip(1) {
        let seg = raw.replace("~1", "/").replace("~0", "~");
        let obj = s.as_object()?;
        if let Some(props) = obj.get("properties").and_then(Value::as_object) {
            if let Some(sub) = props.get(&seg) {
                s = sub;
                continue;
            }
            match obj.get("additionalProperties") {
                Some(extra @ Value::Object(_)) => {
                    s = extra;
                    continue;
                }
                Some(Value::Bool(true)) => return None,
                _ => return Some(seg),
            }
        }
        match obj.get("items") {
            Some(items) if seg.parse::<usize>().is_ok() => s = items,
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::needless_pass_by_value)]
    use serde_json::json;

    use super::*;

    fn warned(schema: Value, params: Value) -> Vec<(Option<String>, String)> {
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "input_schema": schema,
            "nodes": [{ "id": "s", "type": "script.run", "params": {
                "source": { "inline": "function main(i){ return i; }" }, "input": params } }],
            "edges": []
        }))
        .unwrap();
        warnings(&ir)
            .into_iter()
            .map(|w| {
                assert_eq!(w.code, UNKNOWN_INPUT);
                assert_eq!(w.node_id.as_deref(), Some("s"));
                (w.path, w.message)
            })
            .collect()
    }

    fn schema() -> Value {
        json!({ "type": "object", "properties": {
            "title": { "type": "string" },
            "addr": { "type": "object", "properties": { "zip": { "type": "string" } } },
            "rows": { "type": "array", "items": { "type": "object", "properties": { "n": { "type": "integer" } } } },
            "free": { "type": "object" }
        } })
    }

    /// 入力項目に無い項目を指す参照を、ブロックの設定の中の位置つきで知らせる（入れ子・差し込み・条件の中も）。
    #[test]
    fn references_to_inputs_that_are_not_defined_are_warned() {
        let got = warned(
            schema(),
            json!({
                "a": { "$from": "input", "path": "/amount" },
                "b": { "$template": "{z}", "vars": { "z": { "$from": "input", "path": "/addr/zipcode" } } },
                "c": [{ "$from": "input", "path": "/rows/0/m", "default": 0 }],
                "ok": [
                    { "$from": "input", "path": "/title" },
                    { "$from": "input", "path": "/addr/zip" },
                    { "$from": "input", "path": "/rows/3/n" },
                    { "$from": "input", "path": "/free/anything" },
                    { "$from": "input" },
                    { "$from": "trigger", "path": "/amount" }
                ]
            }),
        );
        assert_eq!(
            got,
            vec![
                (
                    Some("/params/input/a".into()),
                    "「amount」は入力項目にありません。手動で実行するときに欄が出ないため、値が入らないと、このブロックは失敗します。".into()
                ),
                (
                    Some("/params/input/b/vars/z".into()),
                    "「zipcode」は入力項目にありません。手動で実行するときに欄が出ないため、値が入らないと、このブロックは失敗します。".into()
                ),
                (
                    Some("/params/input/c/0".into()),
                    "「m」は入力項目にありません。手動で実行するときに欄が出ないため、値が入らないときは既定値を使います。".into()
                ),
            ]
        );
    }

    /// 入力項目が項目を決めていない・無い項目も受けると決めてあるときは言わない。
    #[test]
    fn open_or_undefined_inputs_are_not_warned() {
        let refer = json!({ "$from": "input", "path": "/amount" });
        assert!(warned(json!({ "type": "object" }), refer.clone()).is_empty());
        assert!(warned(
            json!({ "type": "object", "properties": {}, "additionalProperties": true }),
            refer.clone()
        )
        .is_empty());
        assert!(warned(
            json!({ "type": "object", "properties": {}, "additionalProperties": { "type": "number" } }),
            refer.clone()
        )
        .is_empty());
        // 閉じた入力項目（additionalProperties: false）と、省略（画面は項目の欄だけを出す）は言う。
        assert_eq!(
            warned(
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                refer.clone()
            )
            .len(),
            1
        );
        assert_eq!(
            warned(json!({ "type": "object", "properties": {} }), refer).len(),
            1
        );
    }
}
