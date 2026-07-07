//! 条件・値式の評価（**純関数**・ir.md §3・engine.md §4.3）。
//!
//! 値解決は [`ValueResolver`] 経由（`$from` の source→値写像は呼び出し側＝ワーカーが供給）。
//! 条件木は閉じた op のみ。regex は `matches` でのみ使う（パターン長は保存時 V5 で上限済み）。

use serde_json::Value;

use crate::ir::expr::{CmpOp, Comparison, Condition, FromRef, TemplateExpr, ValueExpr};

/// `$from` 参照を実行時の値へ解決する（source→値の写像を持つ）。
pub trait ValueResolver {
    /// 参照を解決する。解決できず default も無ければ `None`。
    fn resolve_from(&self, from: &FromRef) -> Option<Value>;
}

/// 値式を評価する（リテラル / `$from` / `$template`）。
pub fn resolve_value(expr: &ValueExpr, r: &dyn ValueResolver) -> Option<Value> {
    match expr {
        ValueExpr::Literal(v) => Some(v.clone()),
        ValueExpr::From(f) => r.resolve_from(f).or_else(|| f.default.clone()),
        ValueExpr::Template(t) => Some(Value::String(resolve_template(t, r))),
    }
}

/// `$template` を組み立てる（`{name}` を vars のキーで置換・欠損は空文字）。
///
/// エスケープ `{{`→`{`・`}}`→`}` はプレースホルダ解釈より先に処理する（JSON 断片やリテラル波括弧を保つ）。
fn resolve_template(t: &TemplateExpr, r: &dyn ValueResolver) -> String {
    let mut out = String::with_capacity(t.template.len());
    let mut chars = t.template.char_indices().peekable();
    let bytes = t.template.as_bytes();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' if bytes.get(i + 1) == Some(&b'{') => {
                out.push('{');
                chars.next(); // 2 つ目の { を消費。
            }
            '}' if bytes.get(i + 1) == Some(&b'}') => {
                out.push('}');
                chars.next(); // 2 つ目の } を消費。
            }
            '{' => {
                // プレースホルダ `{key}` を探す。閉じが無ければリテラル `{`。
                let start = i + 1;
                let rest = &t.template[start..];
                if let Some(close) = rest.find('}') {
                    let key = &rest[..close];
                    if let Some(ve) = t.vars.get(key) {
                        if let Some(v) = resolve_value(ve, r) {
                            out.push_str(&value_to_string(&v));
                        }
                    }
                    // key ＋ '}' 分を読み飛ばす。
                    for _ in 0..=key.chars().count() {
                        chars.next();
                    }
                } else {
                    out.push('{');
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// 値を文字列へ（テンプレート埋め込み・文字列比較用）。
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 条件木を評価する（`all`/`any`/`not`＋比較）。
pub fn eval_condition(cond: &Condition, r: &dyn ValueResolver) -> bool {
    match cond {
        Condition::All(cs) => cs.iter().all(|c| eval_condition(c, r)),
        Condition::Any(cs) => cs.iter().any(|c| eval_condition(c, r)),
        Condition::Not(c) => !eval_condition(c, r),
        Condition::Cmp(cmp) => eval_cmp(cmp, r),
    }
}

fn eval_cmp(cmp: &Comparison, r: &dyn ValueResolver) -> bool {
    let left = resolve_value(&cmp.left, r);
    let right = cmp.right.as_ref().and_then(|e| resolve_value(e, r));
    match cmp.op {
        CmpOp::Exists => left.as_ref().is_some_and(|v| !v.is_null()),
        CmpOp::IsNull => left.as_ref().is_none_or(Value::is_null),
        CmpOp::Eq => left == right,
        CmpOp::Neq => left != right,
        CmpOp::Lt | CmpOp::Lte | CmpOp::Gt | CmpOp::Gte => {
            compare_ordered(left.as_ref(), right.as_ref(), cmp.op)
        }
        CmpOp::In => match (left, right) {
            (Some(l), Some(Value::Array(items))) => items.contains(&l),
            _ => false,
        },
        CmpOp::Contains => match (left, right) {
            (Some(Value::String(s)), Some(Value::String(sub))) => s.contains(&sub),
            (Some(Value::Array(items)), Some(v)) => items.contains(&v),
            _ => false,
        },
        CmpOp::StartsWith => str_pair(left, right, |s, p| s.starts_with(p)),
        CmpOp::EndsWith => str_pair(left, right, |s, p| s.ends_with(p)),
        CmpOp::Matches => match (left, right) {
            (Some(Value::String(s)), Some(Value::String(pat))) => {
                regex::Regex::new(&pat).is_ok_and(|re| re.is_match(&s))
            }
            _ => false,
        },
    }
}

/// 数値なら数値比較、そうでなければ文字列比較（型不一致は false）。
fn compare_ordered(left: Option<&Value>, right: Option<&Value>, op: CmpOp) -> bool {
    let (Some(l), Some(r)) = (left, right) else {
        return false;
    };
    let ord = if let (Some(a), Some(b)) = (l.as_i64(), r.as_i64()) {
        // 整数同士は f64 変換で精度を落とさず i64 で比較する（2^53 超の ID/カウンタを正しく扱う）。
        Some(a.cmp(&b))
    } else if let (Some(a), Some(b)) = (l.as_u64(), r.as_u64()) {
        Some(a.cmp(&b))
    } else if let (Some(a), Some(b)) = (l.as_f64(), r.as_f64()) {
        a.partial_cmp(&b)
    } else {
        match (l.as_str(), r.as_str()) {
            (Some(a), Some(b)) => Some(a.cmp(b)),
            _ => None,
        }
    };
    match (ord, op) {
        (Some(o), CmpOp::Lt) => o.is_lt(),
        (Some(o), CmpOp::Lte) => o.is_le(),
        (Some(o), CmpOp::Gt) => o.is_gt(),
        (Some(o), CmpOp::Gte) => o.is_ge(),
        _ => false,
    }
}

fn str_pair(left: Option<Value>, right: Option<Value>, f: impl Fn(&str, &str) -> bool) -> bool {
    match (left, right) {
        (Some(Value::String(s)), Some(Value::String(p))) => f(&s, &p),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn numeric_and_string_comparison() {
        let r = resolver(&[("input", json!({ "n": 5, "s": "hello" }))]);
        assert!(eval_condition(
            &cmp(
                from("input", Some("/n")),
                CmpOp::Gt,
                Some(ValueExpr::Literal(json!(3)))
            ),
            &r
        ));
        assert!(!eval_condition(
            &cmp(
                from("input", Some("/n")),
                CmpOp::Lt,
                Some(ValueExpr::Literal(json!(3)))
            ),
            &r
        ));
        assert!(eval_condition(
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
        assert!(eval_condition(
            &cmp(from("input", Some("/missing")), CmpOp::IsNull, None),
            &r
        ));
        assert!(eval_condition(
            &cmp(
                from("input", Some("/tags")),
                CmpOp::Contains,
                Some(ValueExpr::Literal(json!("a")))
            ),
            &r
        ));
        assert!(eval_condition(
            &cmp(
                ValueExpr::Literal(json!("b")),
                CmpOp::In,
                Some(from("input", Some("/tags")))
            ),
            &r
        ));
        assert!(eval_condition(
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
        assert!(eval_condition(
            &Condition::All(vec![gt3.clone(), lt10.clone()]),
            &r
        ));
        assert!(eval_condition(
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
        assert_eq!(resolve_value(&t, &r), Some(json!("hi world!")));
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
        // {{ }} はリテラル波括弧・{who} は置換。
        assert_eq!(
            resolve_value(&t, &r),
            Some(json!("{literal} world {world}"))
        );
    }

    #[test]
    fn integer_comparison_beyond_f64_precision() {
        // 2^53 超の整数を f64 に落とさず正しく比較する。
        let r = resolver(&[(
            "input",
            json!({ "a": 9_007_199_254_740_993_i64, "b": 9_007_199_254_740_992_i64 }),
        )]);
        assert!(eval_condition(
            &cmp(
                from("input", Some("/a")),
                CmpOp::Gt,
                Some(from("input", Some("/b")))
            ),
            &r
        ));
        assert!(!eval_condition(
            &cmp(
                from("input", Some("/a")),
                CmpOp::Eq,
                Some(from("input", Some("/b")))
            ),
            &r
        ));
    }

    #[test]
    fn default_when_unresolved() {
        let r = resolver(&[]);
        let expr = ValueExpr::From(FromRef {
            from: "input".into(),
            path: Some("/x".into()),
            default: Some(json!("fallback")),
        });
        assert_eq!(resolve_value(&expr, &r), Some(json!("fallback")));
    }
}
