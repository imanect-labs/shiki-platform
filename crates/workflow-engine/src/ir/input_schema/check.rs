//! 入力項目（`input_schema`）の形を保存のときに確かめる（V1c・GAP-35）。
//!
//! 起動の検査（[`super::validate_input`]）は fail-closed で、確かめられない形（`$ref`・`oneOf` などのキーワード・
//! 型の名前の誤り・読めない pattern・数でない範囲）に当たると起動しない。ただし見るのは値が通る位置だけなので、
//! 省ける項目の下にある誤りは、その項目に値が入ったときに初めて起動を止める。ここではスキーマの全体を見て、
//! 起動の検査が「スキーマが不正です」「対応していません」と断る形を、保存のときに位置つきで返す。
//!
//! 見るキーワードと意味は起動の検査と同じ集合（[`super::ASSERTIONS`]・[`super::ANNOTATIONS`]）。`x-order` の形は
//! [`super::order_errors`] が見る。

use serde_json::{Map, Value};

use super::{
    compile_pattern, pointer_token, ANNOTATIONS, ASSERTIONS, MAX_DEPTH, MAX_ERRORS,
    NUMBER_KEYWORDS, TYPES,
};

/// 入力項目の形の誤り（入力項目の中の位置の JSON Pointer, 文言）。根は空文字。最大 [`MAX_ERRORS`] 件。
#[must_use]
pub fn schema_errors(schema: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk(schema, "", 0, &mut out);
    out.truncate(MAX_ERRORS);
    out
}

fn push(out: &mut Vec<(String, String)>, at: String, message: impl Into<String>) {
    if out.len() < MAX_ERRORS {
        out.push((at, message.into()));
    }
}

fn walk(schema: &Value, ptr: &str, depth: usize, out: &mut Vec<(String, String)>) {
    if out.len() >= MAX_ERRORS {
        return;
    }
    let s = match schema {
        // true は何でも受け、false は何も受けない（起動の検査と同じ）。
        Value::Bool(_) => return,
        Value::Object(s) => s,
        _ => {
            push(
                out,
                ptr.to_string(),
                "入力項目の形は JSON Schema のオブジェクトで書いてください",
            );
            return;
        }
    };
    if depth > MAX_DEPTH {
        push(
            out,
            ptr.to_string(),
            format!("入力項目の入れ子は {MAX_DEPTH} 段までです"),
        );
        return;
    }
    for k in s
        .keys()
        .filter(|k| !ASSERTIONS.contains(&k.as_str()) && !ANNOTATIONS.contains(&k.as_str()))
    {
        push(
            out,
            format!("{ptr}/{}", pointer_token(k)),
            format!(
                "入力項目の形に「{k}」は書けません。使えるのは type・properties・required・items・additionalProperties・enum・const と、範囲・文字数・個数・pattern です"
            ),
        );
    }
    check_keywords(s, ptr, out);
    if let Some(props) = s.get("properties") {
        match props {
            Value::Object(props) => {
                for (k, sub) in props {
                    walk(
                        sub,
                        &format!("{ptr}/properties/{}", pointer_token(k)),
                        depth + 1,
                        out,
                    );
                }
            }
            _ => push(
                out,
                format!("{ptr}/properties"),
                "properties は、項目のキーから形へのオブジェクトで書いてください",
            ),
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(sub) = s.get(key) {
            walk(sub, &format!("{ptr}/{key}"), depth + 1, out);
        }
    }
}

/// 値を縛るキーワードの形（type・enum・数の範囲・pattern・required）。
fn check_keywords(s: &Map<String, Value>, ptr: &str, out: &mut Vec<(String, String)>) {
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(name) => TYPES.contains(&name.as_str()),
            Value::Array(names) => {
                !names.is_empty()
                    && names
                        .iter()
                        .all(|n| n.as_str().is_some_and(|n| TYPES.contains(&n)))
            }
            _ => false,
        };
        if !ok {
            push(
                out,
                format!("{ptr}/type"),
                "type は string・number・integer・boolean・object・array・null のどれか（または、その配列）で書いてください",
            );
        }
    }
    if let Some(e) = s.get("enum") {
        if e.as_array().is_none_or(Vec::is_empty) {
            push(
                out,
                format!("{ptr}/enum"),
                "enum は、1 つ以上の値の配列で書いてください",
            );
        }
    }
    for key in NUMBER_KEYWORDS {
        if s.get(*key).is_some_and(|v| !v.is_number()) {
            push(
                out,
                format!("{ptr}/{key}"),
                format!("{key} は数で書いてください"),
            );
        }
    }
    if let Some(p) = s.get("pattern") {
        if p.as_str().and_then(compile_pattern).is_none() {
            push(
                out,
                format!("{ptr}/pattern"),
                "pattern を正規表現として読めません",
            );
        }
    }
    if let Some(req) = s.get("required") {
        if !req
            .as_array()
            .is_some_and(|r| r.iter().all(Value::is_string))
        {
            push(
                out,
                format!("{ptr}/required"),
                "required は、項目のキー（文字列）の配列で書いてください",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::validate_input;
    use super::*;

    fn at(schema: &Value) -> Vec<String> {
        schema_errors(schema).into_iter().map(|(p, _)| p).collect()
    }

    /// 画面の入力項目の表が書く形は通る。
    #[test]
    fn the_editor_schema_passes() {
        let schema = json!({
            "type": "object", "required": ["title"], "x-order": ["title", "amount", "day", "kind", "tags"],
            "properties": {
                "title": { "type": "string", "title": "件名", "maxLength": 200 },
                "amount": { "type": "integer", "minimum": 1 },
                "day": { "type": "string", "format": "date" },
                "kind": { "type": "string", "enum": ["a", "b"] },
                "tags": { "type": "array", "items": { "type": "string", "enum": ["x", "y"] } },
                "note": { "type": ["string", "null"], "pattern": "^[a-z]+$" }
            },
            "additionalProperties": false
        });
        assert!(
            schema_errors(&schema).is_empty(),
            "{:?}",
            schema_errors(&schema)
        );
        assert!(schema_errors(&json!(true)).is_empty());
    }

    /// 起動の検査が値の通る位置で断る形を、値が無くても保存のときに位置つきで返す。
    #[test]
    fn shapes_the_launch_check_refuses_are_found_anywhere() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": { "$ref": "#/defs/x" },
                "b": { "oneOf": [{ "type": "string" }] },
                "c": { "type": "text" },
                "d": { "type": "string", "pattern": "(" },
                "e": { "type": "integer", "minimum": "1" },
                "f": { "type": "array", "items": [{ "type": "string" }] },
                "g": { "type": "object", "properties": { "h": { "enum": [] } }, "required": "h" },
                "i": 5
            }
        });
        assert_eq!(
            at(&schema),
            vec![
                "/properties/a/$ref",
                "/properties/b/oneOf",
                "/properties/c/type",
                "/properties/d/pattern",
                "/properties/e/minimum",
                "/properties/f/items",
                "/properties/g/required",
                "/properties/g/properties/h/enum",
                "/properties/i",
            ]
        );
        // 値が入ると起動の検査も断る（保存の検査は起動の検査より緩くない）。
        assert!(validate_input(&schema, &json!({ "b": "x" })).is_err());
        assert!(validate_input(&schema, &json!({ "c": "x" })).is_err());
        assert!(validate_input(&schema, &json!({})).is_ok());
        // 根がオブジェクトでない・properties がオブジェクトでない。
        assert_eq!(at(&json!("object")), vec![""]);
        assert_eq!(at(&json!({ "properties": [] })), vec!["/properties"]);
    }

    /// 誤りは上限で止める。
    #[test]
    fn errors_are_capped() {
        let props: Map<String, Value> = (0..40)
            .map(|i| (format!("p{i}"), json!({ "oneOf": [] })))
            .collect();
        let schema = json!({ "type": "object", "properties": props });
        assert_eq!(schema_errors(&schema).len(), MAX_ERRORS);
    }
}
