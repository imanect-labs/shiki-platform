//! SQL（3 値論理）→ IR の条件木（2 値論理）の翻訳表（query-optimizer.md §3.1・**ここ 1 か所だけ**）。
//!
//! SQL の条件 p の真偽は TRUE / FALSE / UNKNOWN の 3 値で、`WHERE p` が残すのは TRUE の行だけ。IR の条件は
//! 2 値論理（値なしの `neq` は真・`not` は補集合）なので、各式について
//!
//! - `T(p)` = 「p が TRUE になる行」
//! - `F(p)` = 「p が FALSE になる行」（UNKNOWN の行はどちらにも入らない）
//!
//! を IR で作り、`WHERE p` は `T(p)`、`NOT p` は `F(p)`、`(p) IS NULL` は `¬T(p) ∧ ¬F(p)` に写す。
//!
//! | SQL | `T` | `F` |
//! |---|---|---|
//! | `f = c` | `eq c` | `exists ∧ ¬eq c` |
//! | `f <> c` | `exists ∧ ¬eq c`（= `exists ∧ neq c`） | `eq c` |
//! | `f < c`（`<=` `>` `>=` も同形） | `lt c` | `gte c` |
//! | `f IN (…)` | `in […]` | `exists ∧ ¬in […]`（リストに NULL があれば偽にならない） |
//! | `f BETWEEN a AND b` | `gte a ∧ lte b` | `lt a ∨ gt b` |
//! | `f LIKE 'p%'` / `'%p%'` / `'%p'` / `'p'` | `starts_with` / `contains` / `ends_with` / `eq` | `exists ∧ ¬…` |
//! | `f IS NULL` | `is_null` | `exists` |
//! | `a AND b` | `T a ∧ T b` | `F a ∨ F b` |
//! | `a OR b` | `T a ∨ T b` | `F a ∧ F b` |
//! | `NOT a` | `F a` | `T a` |
//! | `(a) IS NULL` | `¬T a ∧ ¬F a` | `T a ∨ F a` |
//! | `(a) IS TRUE` / `IS FALSE` | `T a` / `F a` | 補集合 |
//! | リテラルどうしの比較 | 畳み込み（恒真・恒偽） | 畳み込み |
//!
//! 例（query-optimizer.md §3.1）: `f <> c` は `exists f ∧ neq c`、`NOT (f < c)` は `gte c`（`is_null` を足さない）。
//! 恒真は `all: []`、恒偽は `any: []` で表す（束縛・正規化で TRUE / FALSE に畳まれる）。

use serde_json::Value;

use super::ast::{BExpr, Cmp, Col, Lit, Operand};
use crate::query::ir::{CondOp, Condition};
use crate::query::reject::{QueryRejection, RejectCode};

type R<T> = Result<T, QueryRejection>;

/// 束縛済みの列（この列が属する表の参照名と、値を数値として扱うか）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RCol {
    pub key: String,
    pub numeric: bool,
}

/// 恒真。
pub(crate) fn always() -> Condition {
    Condition::All(Vec::new())
}

/// 恒偽。
pub(crate) fn never() -> Condition {
    Condition::Any(Vec::new())
}

fn cmp(c: &RCol, op: CondOp, v: Option<Value>) -> Condition {
    Condition::cmp(c.key.clone(), op, v)
}

fn not(c: Condition) -> Condition {
    Condition::Not(Box::new(c))
}

fn exists(c: &RCol) -> Condition {
    cmp(c, CondOp::Exists, None)
}

fn invalid(msg: impl Into<String>, loc: super::ast::Loc) -> QueryRejection {
    QueryRejection::new(RejectCode::InvalidQuery, msg).at(loc)
}

/// リテラル → 列の型に合わせた IR の値（数値列の `'100'` は数値・文字列列の `100` は文字列）。
pub(crate) fn value_for(c: &RCol, l: &Lit, loc: super::ast::Loc) -> R<Value> {
    match (l, c.numeric) {
        (Lit::Null, _) => Err(invalid(
            format!("'{}' と NULL の比較は常に不明（UNKNOWN）です", c.key),
            loc,
        )
        .hint("値が無いことは IS NULL / IS NOT NULL で書いてください")),
        (Lit::Bool(_), _) => Err(invalid(
            format!("'{}' と TRUE / FALSE は比べられません", c.key),
            loc,
        )),
        (Lit::Num(n) | Lit::Str(n), true) => n
            .trim()
            .parse::<serde_json::Number>()
            .map(Value::Number)
            .map_err(|_| {
                invalid(
                    format!("'{}' は数値の列です（'{n}' は数値ではありません）", c.key),
                    loc,
                )
            }),
        (Lit::Num(n) | Lit::Str(n), false) => Ok(Value::String(n.clone())),
    }
}

/// リテラルどうしの比較の畳み込み（数値は数値として・文字列は辞書順）。`None` は UNKNOWN（NULL を含む）。
fn fold(lhs: &Lit, op: Cmp, rhs: &Lit) -> Option<bool> {
    use std::cmp::Ordering;
    let ord: Ordering = match (lhs, rhs) {
        (Lit::Str(left), Lit::Str(right)) => left.cmp(right),
        (Lit::Bool(left), Lit::Bool(right)) => left.cmp(right),
        // 数値と、数値として読める文字列（`'10' = 10`）。
        (Lit::Num(left) | Lit::Str(left), Lit::Num(right) | Lit::Str(right)) => {
            let (ln, rn): (f64, f64) = (left.trim().parse().ok()?, right.trim().parse().ok()?);
            ln.partial_cmp(&rn)?
        }
        // NULL を含む比較は UNKNOWN。
        _ => return None,
    };
    Some(match op {
        Cmp::Eq => ord == Ordering::Equal,
        Cmp::Ne => ord != Ordering::Equal,
        Cmp::Lt => ord == Ordering::Less,
        Cmp::Le => ord != Ordering::Greater,
        Cmp::Gt => ord == Ordering::Greater,
        Cmp::Ge => ord != Ordering::Less,
    })
}

fn flip(op: Cmp) -> Cmp {
    match op {
        Cmp::Lt => Cmp::Gt,
        Cmp::Le => Cmp::Ge,
        Cmp::Gt => Cmp::Lt,
        Cmp::Ge => Cmp::Le,
        other => other,
    }
}

/// LIKE のパターン → IR の演算子と値（受け付けるのは `p%` / `%p%` / `%p` / `p`）。
fn like(pattern: &str, loc: super::ast::Loc) -> R<(CondOp, String)> {
    let bad = || {
        QueryRejection::new(
            RejectCode::Unsupported,
            format!("LIKE のパターン '{pattern}' は使えません"),
        )
        .at(loc)
        .hint("使える形は 'abc%'（前方一致）・'%abc%'（部分一致）・'%abc'（後方一致）・'abc'（一致）です（_ と途中の % は使えません）")
    };
    if pattern.contains('_') || pattern.contains('\\') {
        return Err(bad());
    }
    let starts = pattern.starts_with('%');
    let ends = pattern.len() > usize::from(starts) && pattern.ends_with('%');
    let inner = pattern
        .strip_prefix('%')
        .unwrap_or(pattern)
        .strip_suffix('%')
        .filter(|_| ends)
        .unwrap_or_else(|| pattern.strip_prefix('%').unwrap_or(pattern));
    if inner.contains('%') || inner.is_empty() {
        return Err(bad());
    }
    Ok(match (starts, ends) {
        (false, false) => (CondOp::Eq, inner.to_string()),
        (false, true) => (CondOp::StartsWith, inner.to_string()),
        (true, true) => (CondOp::Contains, inner.to_string()),
        (true, false) => (CondOp::EndsWith, inner.to_string()),
    })
}

/// 翻訳器（列の束縛は呼び出し側が与える）。
pub(crate) struct Translator<'r> {
    pub resolve: &'r mut dyn FnMut(&Col) -> R<RCol>,
}

impl Translator<'_> {
    /// `T(p)`（`want = true`）または `F(p)`（`want = false`）。
    #[allow(clippy::too_many_lines)] // 翻訳表をそのまま 1 か所に並べる。
    pub(crate) fn tr(&mut self, e: &BExpr, want: bool) -> R<Condition> {
        Ok(match e {
            BExpr::Const(b) => {
                if *b == want {
                    always()
                } else {
                    never()
                }
            }
            BExpr::And(v) | BExpr::Or(v) => {
                let kids = v.iter().map(|x| self.tr(x, want)).collect::<R<Vec<_>>>()?;
                // T(AND) = ∧T・F(AND) = ∨F・T(OR) = ∨T・F(OR) = ∧F
                if matches!(e, BExpr::And(_)) == want {
                    Condition::All(kids)
                } else {
                    Condition::Any(kids)
                }
            }
            BExpr::Not(x) => self.tr(x, !want)?,
            BExpr::Unknown { inner, negated } => {
                let t = self.tr(inner, true)?;
                let f = self.tr(inner, false)?;
                // (p IS NULL) が TRUE ⇔ p が UNKNOWN。IS NULL 自体は 2 値（UNKNOWN にならない）。
                let unknown = Condition::All(vec![not(t.clone()), not(f.clone())]);
                let known = Condition::Any(vec![t, f]);
                match (want, negated) {
                    (true, false) | (false, true) => unknown,
                    _ => known,
                }
            }
            BExpr::Truth {
                inner,
                value,
                negated,
            } => {
                let base = self.tr(inner, *value)?;
                match (want, negated) {
                    (true, false) | (false, true) => base,
                    _ => not(base),
                }
            }
            BExpr::ColNull { col, negated } => {
                let c = (self.resolve)(col)?;
                let is_null = cmp(&c, CondOp::IsNull, None);
                match (want, negated) {
                    (true, false) | (false, true) => is_null,
                    _ => exists(&c),
                }
            }
            BExpr::Cmp { lhs, op, rhs, loc } => {
                let (col, op, lit) = match (lhs, rhs) {
                    (Operand::Lit(a), Operand::Lit(b)) => {
                        // 恒真・恒偽の畳み込み（UNKNOWN はどちらにも入らない）。
                        return Ok(match fold(a, *op, b) {
                            Some(v) if v == want => always(),
                            _ => never(),
                        });
                    }
                    (Operand::Col(c), Operand::Lit(l)) => (c, *op, l),
                    (Operand::Lit(l), Operand::Col(c)) => (c, flip(*op), l),
                    (Operand::Col(_), Operand::Col(_)) => {
                        return Err(QueryRejection::new(
                            RejectCode::Unsupported,
                            "列どうしの比較は使えません（列とリテラルを比べてください）",
                        )
                        .at(*loc))
                    }
                    _ => {
                        return Err(QueryRejection::new(
                            RejectCode::Unsupported,
                            "集計関数は HAVING にだけ書けます",
                        )
                        .at(*loc))
                    }
                };
                let c = (self.resolve)(col)?;
                let v = value_for(&c, lit, *loc)?;
                let (t_op, f_op, negate_f) = match op {
                    Cmp::Eq => (CondOp::Eq, CondOp::Eq, true),
                    Cmp::Lt => (CondOp::Lt, CondOp::Gte, false),
                    Cmp::Le => (CondOp::Lte, CondOp::Gt, false),
                    Cmp::Gt => (CondOp::Gt, CondOp::Lte, false),
                    Cmp::Ge => (CondOp::Gte, CondOp::Lt, false),
                    Cmp::Ne => {
                        let eq = cmp(&c, CondOp::Eq, Some(v));
                        return Ok(if want {
                            Condition::All(vec![exists(&c), not(eq)])
                        } else {
                            eq
                        });
                    }
                };
                if want {
                    cmp(&c, t_op, Some(v))
                } else if negate_f {
                    Condition::All(vec![exists(&c), not(cmp(&c, f_op, Some(v)))])
                } else {
                    cmp(&c, f_op, Some(v))
                }
            }
            BExpr::In {
                col,
                list,
                negated,
                loc,
            } => {
                let c = (self.resolve)(col)?;
                let has_null = list.iter().any(|l| matches!(l, Lit::Null));
                let vals = list
                    .iter()
                    .filter(|l| !matches!(l, Lit::Null))
                    .map(|l| value_for(&c, l, *loc))
                    .collect::<R<Vec<_>>>()?;
                let t = cmp(&c, CondOp::In, Some(Value::Array(vals)));
                // F(IN) = 値があり、どれにも等しくない（NULL を含むリストでは FALSE にならない）。
                let f = if has_null {
                    never()
                } else {
                    Condition::All(vec![exists(&c), not(t.clone())])
                };
                if want == *negated {
                    f
                } else {
                    t
                }
            }
            BExpr::Between {
                col,
                lo,
                hi,
                negated,
                loc,
            } => {
                let c = (self.resolve)(col)?;
                let lo = value_for(&c, lo, *loc)?;
                let hi = value_for(&c, hi, *loc)?;
                let t = Condition::All(vec![
                    cmp(&c, CondOp::Gte, Some(lo.clone())),
                    cmp(&c, CondOp::Lte, Some(hi.clone())),
                ]);
                let f = Condition::Any(vec![
                    cmp(&c, CondOp::Lt, Some(lo)),
                    cmp(&c, CondOp::Gt, Some(hi)),
                ]);
                if want == *negated {
                    f
                } else {
                    t
                }
            }
            BExpr::Like {
                col,
                pattern,
                negated,
                loc,
            } => {
                let c = (self.resolve)(col)?;
                if c.numeric {
                    return Err(invalid(
                        format!("'{}' は数値の列なので LIKE は使えません", c.key),
                        *loc,
                    ));
                }
                let (op, v) = like(pattern, *loc)?;
                let t = cmp(&c, op, Some(Value::String(v)));
                if want == *negated {
                    Condition::All(vec![exists(&c), not(t)])
                } else {
                    t
                }
            }
        })
    }
}

/// 条件が参照する列（表の振り分け用）。
pub(crate) fn cols(e: &BExpr, out: &mut Vec<Col>) {
    match e {
        BExpr::And(v) | BExpr::Or(v) => v.iter().for_each(|x| cols(x, out)),
        BExpr::Not(x) | BExpr::Unknown { inner: x, .. } | BExpr::Truth { inner: x, .. } => {
            cols(x, out);
        }
        BExpr::Const(_) => {}
        BExpr::Cmp { lhs, rhs, .. } => {
            for o in [lhs, rhs] {
                if let Operand::Col(c) = o {
                    out.push(c.clone());
                }
            }
        }
        BExpr::In { col, .. }
        | BExpr::ColNull { col, .. }
        | BExpr::Like { col, .. }
        | BExpr::Between { col, .. } => out.push(col.clone()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn col(n: &str) -> Col {
        Col {
            qual: None,
            name: n.into(),
            quoted: false,
            loc: None,
        }
    }

    fn tr(e: &BExpr, want: bool) -> Condition {
        let mut r = |c: &Col| -> R<RCol> {
            Ok(RCol {
                key: c.name.clone(),
                numeric: c.name == "n",
            })
        };
        Translator { resolve: &mut r }.tr(e, want).unwrap()
    }

    #[test]
    fn ne_requires_a_value_and_not_lt_is_gte_without_is_null() {
        let ne = BExpr::Cmp {
            lhs: Operand::Col(col("s")),
            op: Cmp::Ne,
            rhs: Operand::Lit(Lit::Str("x".into())),
            loc: None,
        };
        assert_eq!(
            serde_json::to_value(tr(&ne, true)).unwrap(),
            json!({"all": [{"cmp": {"field": "s", "op": "exists"}},
                           {"not": {"cmp": {"field": "s", "op": "eq", "value": "x"}}}]})
        );
        let lt = BExpr::Cmp {
            lhs: Operand::Col(col("n")),
            op: Cmp::Lt,
            rhs: Operand::Lit(Lit::Num("5".into())),
            loc: None,
        };
        assert_eq!(
            serde_json::to_value(tr(&BExpr::Not(Box::new(lt)), true)).unwrap(),
            json!({"cmp": {"field": "n", "op": "gte", "value": 5}})
        );
    }

    #[test]
    fn literal_comparisons_fold_and_null_is_unknown() {
        let t = BExpr::Cmp {
            lhs: Operand::Lit(Lit::Num("1".into())),
            op: Cmp::Eq,
            rhs: Operand::Lit(Lit::Num("1.0".into())),
            loc: None,
        };
        assert_eq!(tr(&t, true), always());
        assert_eq!(tr(&t, false), never());
        let u = BExpr::Cmp {
            lhs: Operand::Lit(Lit::Null),
            op: Cmp::Eq,
            rhs: Operand::Lit(Lit::Num("1".into())),
            loc: None,
        };
        assert_eq!(tr(&u, true), never());
        assert_eq!(tr(&u, false), never());
    }

    #[test]
    fn like_patterns_map_to_closed_operators() {
        assert_eq!(
            like("ab%", None).unwrap(),
            (CondOp::StartsWith, "ab".into())
        );
        assert_eq!(like("%ab%", None).unwrap(), (CondOp::Contains, "ab".into()));
        assert_eq!(like("%ab", None).unwrap(), (CondOp::EndsWith, "ab".into()));
        assert_eq!(like("ab", None).unwrap(), (CondOp::Eq, "ab".into()));
        for bad in ["a_b", "a%b", "%", "%%", "a%b%"] {
            assert!(like(bad, None).is_err(), "{bad}");
        }
    }
}
