//! 値式・条件の評価器（[`super`]）の単体テスト（eval.rs の行数を抑えるため `#[path]` で分離）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use serde_json::json;
use std::collections::HashMap;

/// テスト用: source 文字列 → 値のマップ resolver。
struct MapResolver(HashMap<String, Value>);
impl ValueResolver for MapResolver {
    fn resolve_from(&self, from: &FromRef) -> Option<Value> {
        let base = self.0.get(&from.from)?;
        match &from.path {
            Some(p) => base.pointer(p).cloned(),
            None => Some(base.clone()),
        }
    }
}

fn resolver(pairs: &[(&str, Value)]) -> MapResolver {
    MapResolver(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect(),
    )
}

fn from(src: &str, path: Option<&str>) -> ValueExpr {
    ValueExpr::From(FromRef {
        from: src.into(),
        path: path.map(String::from),
        default: None,
    })
}

fn cmp(left: ValueExpr, op: CmpOp, right: Option<ValueExpr>) -> Condition {
    Condition::Cmp(Comparison { left, op, right })
}

fn eval(c: &Condition, r: &dyn ValueResolver) -> bool {
    eval_condition(c, r).unwrap()
}

#[allow(clippy::needless_pass_by_value)] // json! をそのまま渡す。
fn expr(v: Value) -> ValueExpr {
    serde_json::from_value(v).unwrap()
}

#[test]
fn numeric_and_string_comparison() {
    let r = resolver(&[("input", json!({ "n": 5, "s": "hello" }))]);
    assert!(eval(
        &cmp(
            from("input", Some("/n")),
            CmpOp::Gt,
            Some(ValueExpr::Literal(json!(3)))
        ),
        &r
    ));
    assert!(!eval(
        &cmp(
            from("input", Some("/n")),
            CmpOp::Lt,
            Some(ValueExpr::Literal(json!(3)))
        ),
        &r
    ));
    assert!(eval(
        &cmp(
            from("input", Some("/s")),
            CmpOp::StartsWith,
            Some(ValueExpr::Literal(json!("hel")))
        ),
        &r
    ));
}

#[test]
fn exists_in_contains_matches() {
    let r = resolver(&[(
        "input",
        json!({ "tags": ["a", "b"], "name": "report-2026" }),
    )]);
    assert!(eval(
        &cmp(from("input", Some("/missing")), CmpOp::IsNull, None),
        &r
    ));
    assert!(eval(
        &cmp(
            from("input", Some("/tags")),
            CmpOp::Contains,
            Some(ValueExpr::Literal(json!("a")))
        ),
        &r
    ));
    assert!(eval(
        &cmp(
            ValueExpr::Literal(json!("b")),
            CmpOp::In,
            Some(from("input", Some("/tags")))
        ),
        &r
    ));
    assert!(eval(
        &cmp(
            from("input", Some("/name")),
            CmpOp::Matches,
            Some(ValueExpr::Literal(json!("^report-\\d+$")))
        ),
        &r
    ));
}

#[test]
fn all_any_not_composition() {
    let r = resolver(&[("input", json!({ "n": 5 }))]);
    let gt3 = cmp(
        from("input", Some("/n")),
        CmpOp::Gt,
        Some(ValueExpr::Literal(json!(3))),
    );
    let lt10 = cmp(
        from("input", Some("/n")),
        CmpOp::Lt,
        Some(ValueExpr::Literal(json!(10))),
    );
    assert!(eval(&Condition::All(vec![gt3.clone(), lt10.clone()]), &r));
    assert!(eval(
        &Condition::Not(Box::new(cmp(
            from("input", Some("/n")),
            CmpOp::Eq,
            Some(ValueExpr::Literal(json!(1)))
        ))),
        &r
    ));
}

#[test]
fn template_substitution() {
    let r = resolver(&[("input", json!({ "who": "world" }))]);
    let t = ValueExpr::Template(TemplateExpr {
        template: "hi {who}!".into(),
        vars: [("who".to_string(), from("input", Some("/who")))]
            .into_iter()
            .collect(),
    });
    assert_eq!(resolve_deep(&t, &r), Ok(json!("hi world!")));
}

#[test]
fn template_escaped_braces_survive() {
    let r = resolver(&[("input", json!({ "who": "world" }))]);
    let t = ValueExpr::Template(TemplateExpr {
        template: "{{literal}} {who} {{{who}}}".into(),
        vars: [("who".to_string(), from("input", Some("/who")))]
            .into_iter()
            .collect(),
    });
    // {{ }} はリテラル波括弧・{who} は置換（`{{x}}` は差し込みではない・ir.md §3.2）。
    assert_eq!(resolve_deep(&t, &r), Ok(json!("{literal} world {world}")));
}

/// ir.md §3.1: default の無い var が解決できなければ空文字にせず失敗させる。
#[test]
fn template_unresolved_var_fails_closed() {
    let r = resolver(&[("input", json!({ "title": "出張旅費" }))]);
    let t = expr(json!({ "$template": "件名: {title} / {amount} 円",
        "vars": { "title": { "$from": "input", "path": "/title" },
                  "amount": { "$from": "input", "path": "/amount" } } }));
    let err = resolve_deep(&t, &r).unwrap_err();
    assert_eq!(err.code, EXPR_RESOLVE_ERROR);
    assert!(err.message.contains("{amount}"), "{}", err.message);
    // path の書き誤り（先頭の / なし）も同じく失敗する（空文字で埋めない）。
    let bad_path = expr(json!({ "$template": "{t}",
        "vars": { "t": { "$from": "input", "path": "title" } } }));
    assert_eq!(
        resolve_deep(&bad_path, &r).unwrap_err().code,
        EXPR_RESOLVE_ERROR
    );
}

#[test]
fn template_var_default_is_substituted() {
    let r = resolver(&[("input", json!({ "who": null }))]);
    let t = expr(json!({ "$template": "{who} さん / {n}",
        "vars": { "who": { "$from": "input", "path": "/who", "default": "ゲスト" },
                  "n": { "$from": "input", "path": "/n", "default": 3 } } }));
    // パス不在・null のどちらも default（ir.md §3.1）。数値は JSON 表現で埋める。
    assert_eq!(resolve_deep(&t, &r), Ok(json!("ゲスト さん / 3")));
}

/// vars に無い名前（UI の既定名 v1 のまま・JSON 本文の素の `{`）は黙って消さず失敗させる。
#[test]
fn template_placeholder_missing_from_vars_fails() {
    let r = resolver(&[("input", json!({ "title": "x", "msg": "m" }))]);
    let t = expr(json!({ "$template": "件名: {title}",
        "vars": { "v1": { "$from": "input", "path": "/title" } } }));
    let err = resolve_deep(&t, &r).unwrap_err();
    assert_eq!(err.code, EXPR_RESOLVE_ERROR);
    assert!(err.message.contains("{title}"), "{}", err.message);
    let json_body = expr(json!({ "$template": "{\"text\": \"{msg}\"}",
        "vars": { "msg": { "$from": "input", "path": "/msg" } } }));
    assert_eq!(
        resolve_deep(&json_body, &r).unwrap_err().code,
        EXPR_RESOLVE_ERROR
    );
    // 波括弧をエスケープすれば JSON の本文も組み立てられる。
    let escaped = expr(json!({ "$template": "{{\"text\": \"{msg}\"}}",
        "vars": { "msg": { "$from": "input", "path": "/msg" } } }));
    assert_eq!(resolve_deep(&escaped, &r), Ok(json!("{\"text\": \"m\"}")));
}

/// ir.md §3.1「任意の位置」: リテラルの object / array の中の `$from` / `$template` も展開する。
#[test]
fn literal_nested_refs_are_resolved_deeply() {
    let r = resolver(&[("input", json!({ "title": "出張旅費", "amount": 12000 }))]);
    let e = expr(json!({
        "title": { "$from": "input", "path": "/title" },
        "lines": [ { "$template": "{a} 円", "vars": { "a": { "$from": "input", "path": "/amount" } } }, 1 ],
        "meta": { "fixed": "x", "amount": { "$from": "input", "path": "/amount" } }
    }));
    assert_eq!(
        resolve_deep(&e, &r),
        Ok(json!({ "title": "出張旅費", "lines": ["12000 円", 1],
                   "meta": { "fixed": "x", "amount": 12000 } }))
    );
    // 入れ子の参照も解決できなければ失敗する（`{"$from":…}` のまま下流へ流さない）。
    let missing = expr(json!({ "a": [ { "$from": "input", "path": "/nope" } ] }));
    assert_eq!(
        resolve_deep(&missing, &r).unwrap_err().code,
        EXPR_RESOLVE_ERROR
    );
    // 形の崩れた値式も（保存時は V5 が弾く）。
    let malformed = expr(json!({ "a": { "$from": "input", "typo": 1 } }));
    assert_eq!(
        resolve_deep(&malformed, &r).unwrap_err().code,
        EXPR_RESOLVE_ERROR
    );
    // テンプレートの var がリテラルの object でも中の参照を解決する。
    let var_obj = expr(json!({ "$template": "{o}",
        "vars": { "o": { "t": { "$from": "input", "path": "/title" } } } }));
    assert_eq!(
        resolve_deep(&var_obj, &r),
        Ok(json!("{\"t\":\"出張旅費\"}"))
    );
}

#[test]
fn from_unresolved_fails_and_default_covers_missing_and_null() {
    let r = resolver(&[("input", json!({ "x": null }))]);
    assert_eq!(
        resolve_deep(&from("input", Some("/missing")), &r)
            .unwrap_err()
            .code,
        EXPR_RESOLVE_ERROR
    );
    // null は default が無ければ null のまま（項目を空にする書込などで使う）。
    assert_eq!(
        resolve_deep(&from("input", Some("/x")), &r),
        Ok(Value::Null)
    );
    for path in ["/missing", "/x"] {
        let e = ValueExpr::From(FromRef {
            from: "input".into(),
            path: Some(path.into()),
            default: Some(json!("fallback")),
        });
        assert_eq!(resolve_deep(&e, &r), Ok(json!("fallback")), "{path}");
    }
}

#[test]
fn integer_comparison_beyond_f64_precision() {
    // 2^53 超の整数を f64 に落とさず正しく比較する。
    let r = resolver(&[(
        "input",
        json!({ "a": 9_007_199_254_740_993_i64, "b": 9_007_199_254_740_992_i64 }),
    )]);
    assert!(eval(
        &cmp(
            from("input", Some("/a")),
            CmpOp::Gt,
            Some(from("input", Some("/b")))
        ),
        &r
    ));
    assert!(!eval(
        &cmp(
            from("input", Some("/a")),
            CmpOp::Eq,
            Some(from("input", Some("/b")))
        ),
        &r
    ));
}

/// ir.md §3.3: 大小比較の型不一致は黙って偽にせず expr_type_error（固定値の「100」と数値の比較など）。
#[test]
fn ordered_comparison_type_mismatch_is_an_error() {
    let r = resolver(&[(
        "input",
        json!({ "amount": 12000, "text": "100", "flag": true, "nil": null, "tags": [1, 2] }),
    )]);
    for (path, right) in [
        ("/amount", json!("10000")),
        ("/text", json!(50)),
        ("/flag", json!(true)),
        ("/amount", json!({ "v": 1 })),
        ("/amount", json!([1])),
    ] {
        let c = cmp(
            from("input", Some(path)),
            CmpOp::Gt,
            Some(ValueExpr::Literal(right.clone())),
        );
        let err = eval_condition(&c, &r).unwrap_err();
        assert_eq!(err.code, EXPR_TYPE_ERROR, "{path} gt {right}");
        assert!(
            !err.message.contains("12000"),
            "値は文言に出さない: {}",
            err.message
        );
    }
    // 値なし（パス不在・null）は表どおり偽。多値（配列）は従来どおり偽。
    for path in ["/missing", "/nil", "/tags"] {
        let c = cmp(
            from("input", Some(path)),
            CmpOp::Lt,
            Some(ValueExpr::Literal(json!(5))),
        );
        assert_eq!(eval_condition(&c, &r), Ok(false), "{path}");
    }
    // not / any の中の型不一致も失敗として伝わる（黙って補集合にしない）。
    let bad = cmp(
        from("input", Some("/text")),
        CmpOp::Gte,
        Some(ValueExpr::Literal(json!(1))),
    );
    assert!(eval_condition(&Condition::Not(Box::new(bad.clone())), &r).is_err());
    assert!(eval_condition(&Condition::Any(vec![bad]), &r).is_err());
}

/// 条件の値もリテラルの中の参照を展開する（`in` の候補の配列・data.query と同じ扱い）。
#[test]
fn condition_operands_resolve_nested_refs() {
    let r = resolver(&[("input", json!({ "who": "alice", "me": "alice" }))]);
    let c = cmp(
        from("input", Some("/who")),
        CmpOp::In,
        Some(expr(json!([{ "$from": "input", "path": "/me" }, "bob"]))),
    );
    assert!(eval(&c, &r));
    let missing = cmp(
        from("input", Some("/who")),
        CmpOp::In,
        Some(expr(json!([{ "$from": "input", "path": "/nope" }]))),
    );
    assert_eq!(
        eval_condition(&missing, &r).unwrap_err().code,
        EXPR_RESOLVE_ERROR
    );
    // 右辺のテンプレートも組み立ててから比べる。
    let t = cmp(
        from("input", Some("/who")),
        CmpOp::Eq,
        Some(expr(
            json!({ "$template": "{m}", "vars": { "m": { "$from": "input", "path": "/me" } } }),
        )),
    );
    assert!(eval(&t, &r));
}
