//! 対話トリガの入力の検証（`input_schema`・ir.md §2 の JSON Schema のサブセット）。
//!
//! 対話起動（API・会話の図のボタン・ミニアプリ）は、全経路が run を作る**前に**ここを通る。
//! 画面の検証は信用しない（送られる値は利用者がいくらでも書き換えられる）。
//!
//! 扱うキーワード: `type`・`properties`・`required`・`additionalProperties`・`items`・`enum`・
//! `const`・`minimum`・`maximum`・`exclusiveMinimum`・`exclusiveMaximum`・`minLength`・
//! `maxLength`・`pattern`・`minItems`・`maxItems`。注記（`title`・`description`・`default`・
//! `examples`・`format`・`$schema`・`$comment`）は読み飛ばす。それ以外（`$ref`・`oneOf` など）は
//! 検証できないので**起動しない**（黙って緩く通さない・fail-closed）。

use serde_json::{Map, Value};

/// 返す誤りの上限（巨大な入力で応答を膨らませない）。
const MAX_ERRORS: usize = 20;
/// 入れ子の上限（スキーマ・入力ともに）。
const MAX_DEPTH: usize = 32;

const ASSERTIONS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
];
const ANNOTATIONS: &[&str] = &[
    "title",
    "description",
    "default",
    "examples",
    "format",
    "$schema",
    "$comment",
];

/// 入力をスキーマで検証する。誤りは「場所: 理由」の文（日本語）の一覧。
pub fn validate_input(schema: &Value, input: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    check(schema, input, "", 0, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn at(path: &str) -> &str {
    if path.is_empty() {
        "入力"
    } else {
        path
    }
}

fn check(schema: &Value, v: &Value, path: &str, depth: usize, errs: &mut Vec<String>) {
    if errs.len() >= MAX_ERRORS {
        return;
    }
    let s = match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            errs.push(format!("{}: この値は受け付けません", at(path)));
            return;
        }
        Value::Object(s) => s,
        _ => {
            errs.push(format!("{}: 入力スキーマが不正です", at(path)));
            return;
        }
    };
    if depth > MAX_DEPTH {
        errs.push(format!("{}: 入れ子が深すぎます", at(path)));
        return;
    }
    if let Some(k) = s
        .keys()
        .find(|k| !ASSERTIONS.contains(&k.as_str()) && !ANNOTATIONS.contains(&k.as_str()))
    {
        errs.push(format!(
            "{}: 入力スキーマの「{k}」には対応していません",
            at(path)
        ));
        return;
    }
    if let Some(t) = s.get("type") {
        let types: Vec<&str> = match t {
            Value::String(x) => vec![x.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if types.is_empty() {
            errs.push(format!("{}: 入力スキーマの type が不正です", at(path)));
            return;
        }
        if !types.iter().any(|t| type_matches(t, v)) {
            let names: Vec<&str> = types.iter().map(|t| type_name(t)).collect();
            errs.push(format!(
                "{}: {}を入れてください",
                at(path),
                names.join("か")
            ));
            return;
        }
    }
    if let Some(e) = s.get("enum") {
        match e {
            Value::Array(opts) if opts.iter().any(|o| json_eq(o, v)) => {}
            Value::Array(_) => errs.push(format!("{}: 選択肢にない値です", at(path))),
            _ => errs.push(format!("{}: 入力スキーマの enum が不正です", at(path))),
        }
    }
    if let Some(c) = s.get("const") {
        if !json_eq(c, v) {
            errs.push(format!("{}: 決められた値ではありません", at(path)));
        }
    }
    match v {
        Value::Number(n) => check_number(s, n.as_f64().unwrap_or(f64::NAN), path, errs),
        Value::String(text) => check_string(s, text, path, errs),
        Value::Array(items) => check_array(s, items, path, depth, errs),
        Value::Object(map) => check_object(s, map, path, depth, errs),
        Value::Bool(_) | Value::Null => {}
    }
}

fn num(s: &Map<String, Value>, key: &str, path: &str, errs: &mut Vec<String>) -> Option<f64> {
    let v = s.get(key)?;
    let n = v.as_f64();
    if n.is_none() {
        errs.push(format!("{}: 入力スキーマの {key} が不正です", at(path)));
    }
    n
}

fn check_number(s: &Map<String, Value>, x: f64, path: &str, errs: &mut Vec<String>) {
    if let Some(m) = num(s, "minimum", path, errs) {
        if x < m {
            errs.push(format!("{}: {m} 以上にしてください", at(path)));
        }
    }
    if let Some(m) = num(s, "maximum", path, errs) {
        if x > m {
            errs.push(format!("{}: {m} 以下にしてください", at(path)));
        }
    }
    if let Some(m) = num(s, "exclusiveMinimum", path, errs) {
        if x <= m {
            errs.push(format!("{}: {m} より大きくしてください", at(path)));
        }
    }
    if let Some(m) = num(s, "exclusiveMaximum", path, errs) {
        if x >= m {
            errs.push(format!("{}: {m} より小さくしてください", at(path)));
        }
    }
}

fn check_string(s: &Map<String, Value>, text: &str, path: &str, errs: &mut Vec<String>) {
    // JSON Schema の文字数はコードポイントで数える。
    let len = count_f64(text.chars().count());
    if let Some(m) = num(s, "minLength", path, errs) {
        if len < m {
            errs.push(format!("{}: {m} 文字以上にしてください", at(path)));
        }
    }
    if let Some(m) = num(s, "maxLength", path, errs) {
        if len > m {
            errs.push(format!("{}: {m} 文字までです", at(path)));
        }
    }
    if let Some(p) = s.get("pattern") {
        // ワークフローの作者が書いたパターン。regex crate は線形時間なので入力で暴走しない。
        let re = p
            .as_str()
            .and_then(|p| regex::RegexBuilder::new(p).size_limit(1 << 20).build().ok());
        match re {
            Some(re) if re.is_match(text) => {}
            Some(_) => errs.push(format!("{}: 形式が違います", at(path))),
            None => errs.push(format!("{}: 入力スキーマの pattern が不正です", at(path))),
        }
    }
}

fn check_array(
    s: &Map<String, Value>,
    items: &[Value],
    path: &str,
    depth: usize,
    errs: &mut Vec<String>,
) {
    let n = count_f64(items.len());
    if let Some(m) = num(s, "minItems", path, errs) {
        if n < m {
            errs.push(format!("{}: {m} 個以上選んでください", at(path)));
        }
    }
    if let Some(m) = num(s, "maxItems", path, errs) {
        if n > m {
            errs.push(format!("{}: {m} 個までです", at(path)));
        }
    }
    if let Some(sub) = s.get("items") {
        for (i, item) in items.iter().enumerate() {
            check(sub, item, &format!("{}[{i}]", at(path)), depth + 1, errs);
        }
    }
}

fn check_object(
    s: &Map<String, Value>,
    map: &Map<String, Value>,
    path: &str,
    depth: usize,
    errs: &mut Vec<String>,
) {
    let child = |k: &str| {
        if path.is_empty() {
            k.to_string()
        } else {
            format!("{path}.{k}")
        }
    };
    match s.get("required") {
        None => {}
        Some(Value::Array(req)) => {
            for k in req.iter().filter_map(Value::as_str) {
                if !map.contains_key(k) {
                    errs.push(format!("{}: 必須です", child(k)));
                }
            }
        }
        Some(_) => errs.push(format!("{}: 入力スキーマの required が不正です", at(path))),
    }
    let props = match s.get("properties") {
        None => None,
        Some(Value::Object(p)) => Some(p),
        Some(_) => {
            errs.push(format!(
                "{}: 入力スキーマの properties が不正です",
                at(path)
            ));
            return;
        }
    };
    for (k, v) in map {
        match props.and_then(|p| p.get(k)) {
            Some(sub) => check(sub, v, &child(k), depth + 1, errs),
            None => match s.get("additionalProperties") {
                None | Some(Value::Bool(true)) => {}
                Some(Value::Bool(false)) => {
                    errs.push(format!("{}: 受け付けない項目です", child(k)));
                }
                Some(sub) => check(sub, v, &child(k), depth + 1, errs),
            },
        }
    }
}

/// 個数を数値の比較に使う（入力の大きさの上限があるので精度は落ちない）。
#[allow(clippy::cast_precision_loss)]
fn count_f64(n: usize) -> f64 {
    n as f64
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v
            .as_f64()
            .is_some_and(|x| x.is_finite() && x.fract() == 0.0),
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        "null" => v.is_null(),
        _ => false,
    }
}

fn type_name(t: &str) -> &str {
    match t {
        "string" => "文字",
        "number" => "数値",
        "integer" => "整数",
        "boolean" => "オンかオフ",
        "object" => "項目の組",
        "array" => "一覧",
        "null" => "空",
        other => other,
    }
}

/// JSON の値の一致（数は 1 と 1.0 を同じとみなす）。
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, p)| y.get(k).is_some_and(|q| json_eq(p, q)))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "title": "経費申請",
            "required": ["amount", "category"],
            "additionalProperties": false,
            "properties": {
                "amount": { "type": "integer", "minimum": 1, "maximum": 1_000_000 },
                "category": { "type": "string", "enum": ["交通費", "交際費"] },
                "memo": { "type": "string", "maxLength": 5, "description": "任意" },
                "code": { "type": "string", "pattern": "^[A-Z]{3}$" },
                "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 2 },
                "urgent": { "type": "boolean" }
            }
        })
    }

    #[test]
    fn accepts_valid_input() {
        let input = json!({ "amount": 1200, "category": "交通費", "memo": "あいう", "tags": ["a"], "urgent": true, "code": "ABC" });
        assert_eq!(validate_input(&schema(), &input), Ok(()));
        // 1200.0 も整数として受ける
        assert!(validate_input(
            &schema(),
            &json!({ "amount": 1200.0, "category": "交際費" })
        )
        .is_ok());
    }

    #[test]
    fn reports_each_violation() {
        let input = json!({ "amount": 0, "memo": "あいうえおか", "tags": ["a", "b", 3], "extra": 1, "code": "abc" });
        let errs = validate_input(&schema(), &input).unwrap_err();
        let joined = errs.join("\n");
        assert!(joined.contains("category: 必須です"), "{joined}");
        assert!(joined.contains("amount: 1 以上"), "{joined}");
        assert!(joined.contains("memo: 5 文字まで"), "{joined}");
        assert!(joined.contains("tags: 2 個まで"), "{joined}");
        assert!(joined.contains("tags[2]: 文字を入れてください"), "{joined}");
        assert!(joined.contains("extra: 受け付けない項目です"), "{joined}");
        assert!(joined.contains("code: 形式が違います"), "{joined}");
    }

    #[test]
    fn type_and_enum_mismatch() {
        let errs = validate_input(&schema(), &json!({ "amount": "1200", "category": "食費" }))
            .unwrap_err();
        assert!(errs.iter().any(|e| e == "amount: 整数を入れてください"));
        assert!(errs.iter().any(|e| e == "category: 選択肢にない値です"));
        assert!(validate_input(&schema(), &json!([1])).is_err());
        assert!(validate_input(&json!({ "type": ["string", "null"] }), &Value::Null).is_ok());
    }

    /// 検証できないキーワードは緩く通さず、起動しない。
    #[test]
    fn unsupported_keywords_fail_closed() {
        let s = json!({ "type": "object", "properties": { "a": { "$ref": "#/defs/x" } } });
        let errs = validate_input(&s, &json!({ "a": 1 })).unwrap_err();
        assert_eq!(
            errs,
            vec!["a: 入力スキーマの「$ref」には対応していません".to_string()]
        );
        assert!(validate_input(&json!({ "oneOf": [] }), &json!(1)).is_err());
        assert!(validate_input(&json!({ "pattern": "(" }), &json!("x")).is_err());
        assert!(validate_input(&json!(false), &json!(1)).is_err());
        assert!(validate_input(&json!(true), &json!(1)).is_ok());
    }

    #[test]
    fn exclusive_bounds_and_const() {
        let s = json!({ "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 10 });
        assert!(validate_input(&s, &json!(0)).is_err());
        assert!(validate_input(&s, &json!(10)).is_err());
        assert!(validate_input(&s, &json!(9.5)).is_ok());
        assert!(validate_input(&json!({ "const": { "a": [1] } }), &json!({ "a": [1.0] })).is_ok());
        assert!(validate_input(&json!({ "const": "x" }), &json!("y")).is_err());
    }

    #[test]
    fn error_count_is_bounded() {
        let mut props = Map::new();
        let mut input = Map::new();
        for i in 0..100 {
            props.insert(format!("k{i}"), json!({ "type": "string" }));
            input.insert(format!("k{i}"), json!(i));
        }
        let s = json!({ "type": "object", "properties": props });
        let errs = validate_input(&s, &Value::Object(input)).unwrap_err();
        assert_eq!(errs.len(), MAX_ERRORS);
    }
}
