#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{json, Map, Value};

use super::*;

/// 問い合わせの仕分け（APP-02）の出力の形。
fn triage() -> Value {
    json!({
        "type": "object",
        "required": ["category", "summary"],
        "properties": {
            "category": { "type": "string", "enum": ["billing", "bug", "howto", "cancel", "other"] },
            "summary": { "type": "string", "maxLength": 20, "description": "要約" },
            "priority": { "type": ["integer", "null"], "minimum": 1, "maximum": 4 },
            "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 3 },
            "contact": {
                "type": "object",
                "properties": { "name": { "type": "string" } }
            }
        }
    })
}

fn issue(schema: &Value) -> SchemaIssue {
    check_output_schema(schema).expect_err("拒否されるはず")
}

#[test]
fn accepts_a_typical_schema() {
    assert_eq!(check_output_schema(&triage()), Ok(()));
    // 項目の無いオブジェクトも書ける（閉じた空のオブジェクト）。
    assert_eq!(check_output_schema(&json!({ "type": "object" })), Ok(()));
}

#[test]
fn the_root_is_an_object() {
    assert_eq!(issue(&json!("x")).path, "");
    for root in [
        json!({ "type": "array", "items": { "type": "string" } }),
        json!({ "properties": { "a": { "type": "string" } } }),
        json!({ "type": ["object", "null"] }),
    ] {
        let got = issue(&root);
        assert_eq!(got.path, "/type", "{root}");
        assert!(got.message.contains("type: \"object\""), "{got:?}");
    }
}

#[test]
fn unsupported_keywords_and_boolean_schemas_are_rejected() {
    for (sub, path, word) in [
        (json!({ "$ref": "#/$defs/x" }), "/properties/a/$ref", "$ref"),
        (json!({ "anyOf": [] }), "/properties/a/anyOf", "anyOf"),
        (json!({ "oneOf": [] }), "/properties/a/oneOf", "oneOf"),
    ] {
        let got = issue(&json!({ "type": "object", "properties": { "a": sub } }));
        assert_eq!(got.path, path);
        assert!(got.message.contains(word), "{got:?}");
    }
    let got = issue(&json!({ "type": "object", "properties": { "a": true } }));
    assert_eq!(got.path, "/properties/a");
}

#[test]
fn type_arrays_are_one_type_and_null() {
    let with = |t: Value| json!({ "type": "object", "properties": { "a": { "type": t } } });
    assert_eq!(
        check_output_schema(&with(json!(["string", "null"]))),
        Ok(())
    );
    assert_eq!(
        check_output_schema(&with(json!(["null", "integer"]))),
        Ok(())
    );
    for bad in [
        json!(["string", "number"]),
        json!(["string"]),
        json!(["string", "null", "number"]),
        json!(["null", "null"]),
        json!("date"),
        json!(3),
    ] {
        assert_eq!(
            issue(&with(bad.clone())).path,
            "/properties/a/type",
            "{bad}"
        );
    }
}

#[test]
fn objects_must_be_closed() {
    for ap in [json!(true), json!({ "type": "string" })] {
        let got = issue(&json!({ "type": "object", "additionalProperties": ap }));
        assert_eq!(got.path, "/additionalProperties");
    }
    assert_eq!(
        check_output_schema(&json!({ "type": "object", "additionalProperties": false })),
        Ok(())
    );
    // 入れ子のオブジェクトも同じ。
    let got = issue(&json!({ "type": "object", "properties": {
        "a": { "type": "object", "additionalProperties": true } } }));
    assert_eq!(got.path, "/properties/a/additionalProperties");
}

#[test]
fn property_names_and_required_are_checked() {
    for name in ["日本語", "a-b", "1st", "", &"a".repeat(65)] {
        let got = issue(&json!({ "type": "object", "properties": { name: { "type": "string" } } }));
        assert!(got.message.contains("使えません"), "{name}: {got:?}");
    }
    assert_eq!(
        check_output_schema(
            &json!({ "type": "object", "properties": { "_ok": {}, &"a".repeat(64): {} } })
        ),
        Ok(())
    );
    let props = json!({ "a": { "type": "string" } });
    let got = issue(&json!({ "type": "object", "properties": props, "required": ["b"] }));
    assert_eq!(got.path, "/required/0");
    let got = issue(&json!({ "type": "object", "properties": props, "required": ["a", "a"] }));
    assert_eq!(got.path, "/required/1");
    let got = issue(&json!({ "type": "object", "properties": props, "required": "a" }));
    assert_eq!(got.path, "/required");
    let got = issue(&json!({ "type": "object", "properties": props, "required": [1] }));
    assert_eq!(got.path, "/required/0");
}

#[test]
fn keywords_must_fit_the_type() {
    let got = issue(&json!({ "type": "object", "properties": {
        "a": { "type": "string", "required": [] } } }));
    assert_eq!(got.path, "/properties/a/required");
    let got = issue(&json!({ "type": "object", "properties": {
        "a": { "type": "string", "items": { "type": "string" } } } }));
    assert_eq!(got.path, "/properties/a/items");
}

#[test]
fn value_constraints_are_well_formed() {
    let with = |sub: Value| json!({ "type": "object", "properties": { "a": sub } });
    for (sub, path) in [
        (json!({ "enum": [] }), "/properties/a/enum"),
        (json!({ "enum": "x" }), "/properties/a/enum"),
        (json!({ "enum": [{ "a": 1 }] }), "/properties/a/enum"),
        (
            json!({ "type": "string", "pattern": "(" }),
            "/properties/a/pattern",
        ),
        (
            json!({ "type": "number", "minimum": "1" }),
            "/properties/a/minimum",
        ),
        (
            json!({ "type": "string", "maxLength": -1 }),
            "/properties/a/maxLength",
        ),
        (
            json!({ "type": "array", "minItems": 1.5 }),
            "/properties/a/minItems",
        ),
        (
            json!({ "type": "string", "description": 3 }),
            "/properties/a/description",
        ),
    ] {
        assert_eq!(issue(&with(sub.clone())).path, path, "{sub}");
    }
    assert_eq!(
        check_output_schema(&with(json!({ "enum": ["a", 1, true, null] }))),
        Ok(())
    );
}

/// 深さは根から properties / items をたどる段数で数え、5 段まで書ける。
#[test]
fn nesting_is_bounded() {
    let nested = |levels: usize| {
        let mut s = json!({ "type": "string" });
        for _ in 0..levels {
            s = json!({ "type": "object", "properties": { "a": s } });
        }
        s
    };
    assert_eq!(check_output_schema(&nested(5)), Ok(()));
    let got = issue(&nested(6));
    assert!(got.message.contains("5 段"), "{got:?}");
    // 配列の items も 1 段に数える。
    let arr = json!({ "type": "object", "properties": { "a": { "type": "array", "items":
        { "type": "array", "items": { "type": "array", "items": { "type": "array", "items":
            { "type": "array", "items": { "type": "string" } } } } } } } });
    assert!(check_output_schema(&arr).is_err());
}

fn props(n: usize, required: bool, sub: &Value) -> Value {
    let mut p = Map::new();
    for i in 0..n {
        p.insert(format!("k{i}"), sub.clone());
    }
    let names: Vec<String> = (0..n).map(|i| format!("k{i}")).collect();
    if required {
        json!({ "type": "object", "properties": p, "required": names })
    } else {
        json!({ "type": "object", "properties": p })
    }
}

/// 任意項目と null を許す項目は Anthropic の上限（24・16）まで、項目は 100 まで。
#[test]
fn counts_are_bounded() {
    let s = json!({ "type": "string" });
    assert_eq!(check_output_schema(&props(24, false, &s)), Ok(()));
    let got = issue(&props(25, false, &s));
    assert!(
        got.message.contains("任意の項目") && got.message.contains("24"),
        "{got:?}"
    );
    // 全部を必須にすれば任意項目は数えない。
    assert_eq!(check_output_schema(&props(100, true, &s)), Ok(()));
    let got = issue(&props(101, true, &s));
    assert!(got.message.contains("100"), "{got:?}");
    let nullable = json!({ "type": ["string", "null"] });
    assert_eq!(check_output_schema(&props(16, true, &nullable)), Ok(()));
    let got = issue(&props(17, true, &nullable));
    assert!(got.message.contains("null を許す"), "{got:?}");
    // 入れ子の任意項目も数える（3 × 9 = 27）。
    let inner = props(8, false, &s);
    let got = issue(&props(3, false, &inner));
    assert!(got.message.contains("任意の項目"), "{got:?}");
}

#[test]
fn enum_values_and_size_are_bounded() {
    let values: Vec<String> = (0..129).map(|i| format!("v{i}")).collect();
    let e = json!({ "type": "string", "enum": values });
    let got = issue(&props(2, true, &e));
    assert!(got.message.contains("256"), "{got:?}");
    let big = json!({ "type": "string", "description": "あ".repeat(6000) });
    let got = issue(&props(1, true, &big));
    assert!(got.message.contains("KiB"), "{got:?}");
}

#[test]
fn malformed_order_is_rejected() {
    let got = issue(&json!({ "type": "object", "properties": { "a": {} }, "x-order": ["b"] }));
    assert_eq!(got.path, "/x-order/0");
}

#[test]
fn close_objects_closes_every_object() {
    let closed = close_objects(&triage());
    assert_eq!(closed["additionalProperties"], false);
    assert_eq!(
        closed["properties"]["contact"]["additionalProperties"],
        false
    );
    // オブジェクトでない項目には書かない。元の値は変えない。
    assert!(closed["properties"]["summary"]
        .get("additionalProperties")
        .is_none());
    assert!(triage().get("additionalProperties").is_none());
    let arr = json!({ "type": "object", "properties": { "rows": { "type": "array",
        "items": { "type": "object", "properties": { "x": {} } } } } });
    assert_eq!(
        close_objects(&arr)["properties"]["rows"]["items"]["additionalProperties"],
        false
    );
}

#[test]
fn output_is_parsed_and_validated() {
    let schema = close_objects(&triage());
    let ok = r#"{"category":"bug","summary":"落ちる","priority":null,"contact":{"name":"a"}}"#;
    assert_eq!(validate_output(&schema, ok).unwrap()["category"], "bug");
    // 前後のコードフェンスは剥がして読む。
    let fenced = format!("```json\n{ok}\n```");
    assert_eq!(
        validate_output(&schema, &fenced).unwrap()["summary"],
        "落ちる"
    );
    assert!(validate_output(&schema, &format!("```\n{ok}\n```  ")).is_ok());
    let errs = validate_output(&schema, "はい、こちらです").unwrap_err();
    assert_eq!(
        errs,
        vec!["出力: JSON として読めません（JSON のオブジェクトを 1 つだけ出してください）"]
    );
    assert!(validate_output(&schema, "```json\n{\"category\"").is_err());
}

/// 閉じたオブジェクトは余分な項目を拒む。プロバイダへ送らない制約（長さ・範囲）も事後に守る。
#[test]
fn closed_schema_rejects_extras_and_enforces_dropped_constraints() {
    let schema = close_objects(&triage());
    let errs = validate_output(
        &schema,
        r#"{"category":"refund","summary":"あいうえおかきくけこさしすせそたちつてとな","priority":9,"extra":1,"contact":{"name":"a","x":1}}"#,
    )
    .unwrap_err();
    let joined = errs.join("\n");
    for want in [
        "category: 選択肢にない値です",
        "summary: 20 文字までです",
        "priority: 4 以下にしてください",
        "extra: 受け付けない項目です",
        "contact.x: 受け付けない項目です",
    ] {
        assert!(joined.contains(want), "{want}: {joined}");
    }
    let errs = validate_output(&schema, r#"{"summary":"a"}"#).unwrap_err();
    assert_eq!(errs, vec!["category: 必須です"]);
    let errs = validate_output(&schema, "[1]").unwrap_err();
    assert_eq!(errs, vec!["出力: 項目のまとまりを入れてください"]);
}

/// 保存のときの pattern は、入力項目と同じく 1 つ 256 文字まで・数はワークフローの全体（使い回す数え）で 50 個まで。
/// 超えた分はコンパイルせず、数の超過は超えた最初の位置で 1 回だけ言う（BUG-226）。実行のときの確かめは上限を掛けない
/// （保存済みの版の実行を変えない）。
#[test]
fn saved_patterns_are_capped_across_the_workflow() {
    use crate::ir::patterns::{compiled_count, PatternBudget, MAX_PATTERNS, MAX_PATTERN_CHARS};

    let with = |p: &str| json!({ "type": "object", "properties": { "a": { "type": "string", "pattern": p } } });
    let long = "a".repeat(MAX_PATTERN_CHARS + 1);
    let got = check_saved_output_schema(&with(&long), &mut PatternBudget::for_save()).unwrap_err();
    assert_eq!(
        got,
        SchemaIssue::new(
            "/properties/a/pattern",
            "pattern は 256 字までにしてください。"
        )
    );
    let at_limit = "あ".repeat(MAX_PATTERN_CHARS);
    assert_eq!(
        check_saved_output_schema(&with(&at_limit), &mut PatternBudget::for_save()),
        Ok(())
    );

    // 30 個ずつの出力の形を 3 つ（ブロック 3 つ）。数えを使い回すと、2 つ目の 21 個目で 1 回だけ言い、3 つ目は
    // 読めない pattern でも言わずにコンパイルもしない。
    let thirty = |p: &str| props(30, true, &json!({ "type": "string", "pattern": p }));
    let mut patterns = PatternBudget::for_save();
    let before = compiled_count();
    assert_eq!(
        check_saved_output_schema(&thirty("^a$"), &mut patterns),
        Ok(())
    );
    let got = check_saved_output_schema(&thirty("^a$"), &mut patterns).unwrap_err();
    assert_eq!(
        got.message,
        format!("出力の形の pattern は、すべての「AI に聞く」を合わせて {MAX_PATTERNS} 個までです。どれかのブロックの pattern を減らしてください。")
    );
    assert!(got.path.ends_with("/pattern"), "{got:?}");
    assert_eq!(
        check_saved_output_schema(&thirty("("), &mut patterns),
        Ok(())
    );
    assert_eq!(compiled_count() - before, MAX_PATTERNS);

    // 実行のときは長さも数も見ずに、すべて読む。
    assert_eq!(check_output_schema(&with(&long)), Ok(()));
    let before = compiled_count();
    assert_eq!(
        check_output_schema(&props(
            60,
            true,
            &json!({ "type": "string", "pattern": "^a$" })
        )),
        Ok(())
    );
    assert_eq!(compiled_count() - before, 60);
}
