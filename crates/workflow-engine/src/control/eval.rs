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

/// 左辺を「値の集合」として見る（data の多値列と同じ模型・query-optimizer.md §3.1）。
///
/// 値なし（未解決・`null`）と空配列は空集合、配列は要素の集合（`null` の要素は値なし）、
/// それ以外は要素 1 個の集合。
fn value_set(v: Option<&Value>) -> Vec<&Value> {
    match v {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().filter(|x| !x.is_null()).collect(),
        Some(other) => vec![other],
    }
}

/// 等値（数値は数値として比較する: `1 = 1.0`。整数同士は精度を落とさない）。
fn same(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Number(l), Value::Number(r)) => {
            if let (Some(li), Some(ri)) = (l.as_i64(), r.as_i64()) {
                li == ri
            } else if let (Some(lu), Some(ru)) = (l.as_u64(), r.as_u64()) {
                lu == ru
            } else {
                // 浮動小数の等値は JSON の数値の値としての一致（誤差を許さない・data の 10 進比較と同じ結果）。
                l.as_f64().zip(r.as_f64()).is_some_and(|(lf, rf)| {
                    lf.partial_cmp(&rf).is_some_and(std::cmp::Ordering::is_eq)
                })
            }
        }
        _ => lhs == rhs,
    }
}

/// 比較 1 つを評価する（2 値論理・data の評価規則と同じ表・query-optimizer.md §3.1）。
///
/// - `eq c`: 値のいずれかが c に等しい（単値は値 = c・多値は「c を含む」・値なしは偽）
/// - `neq c`: `not(eq c)`（値なしは**真**・多値は「c を含まない」）
/// - `in [..]`: 値のいずれかが候補のいずれかに等しい（値なしは偽）
/// - `contains` / `starts_with` / `ends_with`: いずれかの要素（文字列）が満たす
/// - `exists`: 値が 1 個以上（空配列は偽）・`is_null`: `not(exists)`
/// - `lt` / `lte` / `gt` / `gte`: 単値だけ（多値は data では使えない・ここでは偽）
///
/// 2026-10 に data 側へ揃えた（それ以前は配列全体との一致・`in` は左辺が配列だと常に偽・
/// 空配列の `exists` は真・`1 ≠ 1.0`）。保存済み定義の影響は `ir::validate::semantics` が検出する。
fn eval_cmp(cmp: &Comparison, r: &dyn ValueResolver) -> bool {
    let left = resolve_value(&cmp.left, r);
    let right = cmp.right.as_ref().and_then(|e| resolve_value(e, r));
    let vals = value_set(left.as_ref());
    match cmp.op {
        CmpOp::Exists => !vals.is_empty(),
        CmpOp::IsNull => vals.is_empty(),
        CmpOp::Eq => right
            .as_ref()
            .is_some_and(|c| vals.iter().any(|v| same(v, c))),
        CmpOp::Neq => !right
            .as_ref()
            .is_some_and(|c| vals.iter().any(|v| same(v, c))),
        CmpOp::Lt | CmpOp::Lte | CmpOp::Gt | CmpOp::Gte => match left.as_ref() {
            Some(Value::Array(_)) => false,
            l => compare_ordered(l, right.as_ref(), cmp.op),
        },
        CmpOp::In => match right {
            Some(Value::Array(cands)) => vals.iter().any(|v| cands.iter().any(|c| same(v, c))),
            _ => false,
        },
        CmpOp::Contains => any_str(&vals, right.as_ref(), |s, p| s.contains(p)),
        CmpOp::StartsWith => any_str(&vals, right.as_ref(), |s, p| s.starts_with(p)),
        CmpOp::EndsWith => any_str(&vals, right.as_ref(), |s, p| s.ends_with(p)),
        CmpOp::Matches => match (left, right) {
            (Some(Value::String(s)), Some(Value::String(pat))) => {
                regex::Regex::new(&pat).is_ok_and(|re| re.is_match(&s))
            }
            _ => false,
        },
    }
}

/// いずれかの要素（文字列）が右辺の文字列について `f` を満たすか。
fn any_str(vals: &[&Value], right: Option<&Value>, f: impl Fn(&str, &str) -> bool) -> bool {
    let Some(Value::String(p)) = right else {
        return false;
    };
    vals.iter().any(|v| v.as_str().is_some_and(|s| f(s, p)))
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
