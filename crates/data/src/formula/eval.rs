//! 数式のインタプリタ（閉じた AST を Rust で評価する・data-platform §7.3）。
//!
//! - 算術は [`Decimal`]（`numeric` 精度・二進誤差なし）。
//! - null・型の不一致・0 除算・桁あふれは null（例外にしない＝1 行の不正値で一覧全体を落とさない）。
//! - 比較の被演算子に null があれば偽。`if` は条件が true のときだけ真の枝。

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use serde_json::Value;

use super::ast::{FormulaCmpOp, FormulaExpr};
use super::decimal::Decimal;
use crate::model::FieldType;

/// 評価中の値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FVal {
    Null,
    Num(Decimal),
    Text(String),
    Bool(bool),
    Date(NaiveDate),
    DateTime(DateTime<Utc>),
}

/// 評価環境（フィールドの値と式内ロールアップの値）。
pub(crate) trait Env {
    /// 自テーブルのフィールドの値（閲覧者に読めない列は null）。
    fn field(&self, key: &str) -> FVal;
    /// 式内ロールアップ `node` の値（事前に計算したもの）。
    fn rollup(&self, node: &FormulaExpr) -> FVal;
}

impl FVal {
    /// 保存値（JSON）をフィールドの型に沿って読む。配列（多値・lookup）は `, ` 連結の文字列。
    pub(crate) fn from_json(v: &Value, ty: Option<FieldType>) -> FVal {
        match v {
            Value::Null | Value::Object(_) => FVal::Null,
            Value::Bool(b) => FVal::Bool(*b),
            Value::Number(n) => Decimal::from_number(n).map_or(FVal::Null, FVal::Num),
            Value::String(s) => match ty {
                Some(FieldType::Date) => {
                    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_or(FVal::Null, FVal::Date)
                }
                Some(FieldType::DateTime) => DateTime::parse_from_rfc3339(s)
                    .map_or(FVal::Null, |d| FVal::DateTime(d.with_timezone(&Utc))),
                _ => FVal::Text(s.clone()),
            },
            Value::Array(a) => FVal::Text(
                a.iter()
                    .filter(|x| !x.is_null())
                    .map(|x| FVal::from_json(x, ty).text())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }
    }

    /// JSON へ戻す（number は正確に戻せる場合だけ数値・date は `YYYY-MM-DD`）。
    pub(crate) fn to_json(&self) -> Value {
        match self {
            FVal::Null => Value::Null,
            FVal::Num(d) => d.to_json(),
            FVal::Text(s) => Value::String(s.clone()),
            FVal::Bool(b) => Value::Bool(*b),
            FVal::Date(d) => Value::String(d.format("%Y-%m-%d").to_string()),
            FVal::DateTime(t) => Value::String(t.to_rfc3339_opts(SecondsFormat::Micros, true)),
        }
    }

    /// 文字列表現（`concat` 用。null は空文字）。
    pub(crate) fn text(&self) -> String {
        match self {
            FVal::Null => String::new(),
            FVal::Num(d) => d.canonical(),
            FVal::Text(s) => s.clone(),
            FVal::Bool(b) => b.to_string(),
            FVal::Date(_) | FVal::DateTime(_) => match self.to_json() {
                Value::String(s) => s,
                _ => String::new(),
            },
        }
    }

    fn truthy(&self) -> bool {
        matches!(self, FVal::Bool(true))
    }

    fn date(&self) -> Option<NaiveDate> {
        match self {
            FVal::Date(d) => Some(*d),
            FVal::DateTime(t) => Some(t.date_naive()),
            FVal::Text(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().or_else(|| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc).date_naive())
            }),
            _ => None,
        }
    }
}

/// 2 値の順序比較（型が揃わなければ `None`）。
pub(crate) fn compare(a: &FVal, b: &FVal) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (FVal::Num(x), FVal::Num(y)) => Some(x.cmp(y)),
        (FVal::Text(x), FVal::Text(y)) => Some(x.cmp(y)),
        (FVal::Bool(x), FVal::Bool(y)) => Some(x.cmp(y)),
        (FVal::Date(_) | FVal::DateTime(_), FVal::Date(_) | FVal::DateTime(_)) => match (a, b) {
            (FVal::DateTime(x), FVal::DateTime(y)) => Some(x.cmp(y)),
            _ => Some(a.date()?.cmp(&b.date()?)),
        },
        _ => None,
    }
}

fn arith(lhs: FVal, rhs: FVal, op: impl Fn(Decimal, Decimal) -> Option<Decimal>) -> FVal {
    match (lhs, rhs) {
        (FVal::Num(x), FVal::Num(y)) => op(x, y).map_or(FVal::Null, FVal::Num),
        _ => FVal::Null,
    }
}

/// 式を評価する。
pub(crate) fn eval(e: &FormulaExpr, env: &dyn Env, now: DateTime<Utc>) -> FVal {
    use FormulaExpr as E;
    let ev = |x: &FormulaExpr| eval(x, env, now);
    match e {
        E::Field(k) => env.field(k),
        E::Lit(v) => FVal::from_json(v, None),
        E::Add(a, b) => arith(ev(a), ev(b), Decimal::checked_add),
        E::Sub(a, b) => arith(ev(a), ev(b), Decimal::checked_sub),
        E::Mul(a, b) => arith(ev(a), ev(b), Decimal::checked_mul),
        E::Div(a, b) => arith(ev(a), ev(b), Decimal::checked_div),
        E::Round(a, s) => match ev(a) {
            FVal::Num(d) => FVal::Num(d.round_half_up(u32::from(*s))),
            _ => FVal::Null,
        },
        E::Concat(v) => FVal::Text(v.iter().map(|x| ev(x).text()).collect()),
        E::Upper(a) => match ev(a) {
            FVal::Text(s) => FVal::Text(s.to_uppercase()),
            _ => FVal::Null,
        },
        E::Lower(a) => match ev(a) {
            FVal::Text(s) => FVal::Text(s.to_lowercase()),
            _ => FVal::Null,
        },
        E::DateAdd(d, n) => {
            let days = match ev(n) {
                FVal::Num(x) => x.to_i64().filter(|v| v.abs() <= 3_650_000),
                _ => None,
            };
            match (ev(d), days) {
                (FVal::Date(x), Some(n)) => x
                    .checked_add_signed(Duration::days(n))
                    .map_or(FVal::Null, FVal::Date),
                (FVal::DateTime(x), Some(n)) => x
                    .checked_add_signed(Duration::days(n))
                    .map_or(FVal::Null, FVal::DateTime),
                _ => FVal::Null,
            }
        }
        E::DateDiff(from, to) => match (ev(from).date(), ev(to).date()) {
            (Some(a), Some(b)) => FVal::Num(Decimal::from_i64((b - a).num_days())),
            _ => FVal::Null,
        },
        E::Now => FVal::DateTime(now),
        E::If(c, a, b) => {
            if ev(c).truthy() {
                ev(a)
            } else {
                ev(b)
            }
        }
        E::Switch {
            value,
            cases,
            default,
        } => {
            let v = ev(value);
            for c in cases {
                if compare(&v, &ev(&c.when)) == Some(std::cmp::Ordering::Equal) {
                    return ev(&c.then);
                }
            }
            default.as_deref().map_or(FVal::Null, ev)
        }
        E::And(v) => FVal::Bool(v.iter().all(|x| ev(x).truthy())),
        E::Or(v) => FVal::Bool(v.iter().any(|x| ev(x).truthy())),
        E::Not(a) => match ev(a) {
            FVal::Bool(b) => FVal::Bool(!b),
            _ => FVal::Null,
        },
        E::Cmp(a, op, b) => {
            let ord = compare(&ev(a), &ev(b));
            FVal::Bool(ord.is_some_and(|o| match op {
                FormulaCmpOp::Eq => o.is_eq(),
                FormulaCmpOp::Ne => o.is_ne(),
                FormulaCmpOp::Lt => o.is_lt(),
                FormulaCmpOp::Lte => o.is_le(),
                FormulaCmpOp::Gt => o.is_gt(),
                FormulaCmpOp::Gte => o.is_ge(),
            }))
        }
        E::Rollup { .. } => env.rollup(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    struct M(HashMap<&'static str, Value>);
    impl Env for M {
        fn field(&self, key: &str) -> FVal {
            match key {
                "due" => self
                    .0
                    .get(key)
                    .map_or(FVal::Null, |v| FVal::from_json(v, Some(FieldType::Date))),
                _ => self
                    .0
                    .get(key)
                    .map_or(FVal::Null, |v| FVal::from_json(v, None)),
            }
        }
        fn rollup(&self, _: &FormulaExpr) -> FVal {
            FVal::Num(Decimal::parse("0.2").unwrap())
        }
    }

    fn parse(v: Value) -> FormulaExpr {
        serde_json::from_value(v).unwrap()
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn money_formula_has_no_float_error() {
        let env = M([("price", json!(0.1)), ("qty", json!(3))].into());
        // price * qty + rollup(=0.2) = 0.5（f64 なら 0.30000000000000004 + 0.2）。
        let e = parse(json!({"add": [
            {"mul": [{"field": "price"}, {"field": "qty"}]},
            {"rollup": {"link": "items", "target": "amount", "op": "sum"}}
        ]}));
        assert_eq!(eval(&e, &env, now()).to_json(), json!(0.5));
        let tax = parse(json!({"round": [{"mul": [{"lit": 1980}, {"lit": 0.1}]}, 0]}));
        assert_eq!(eval(&tax, &env, now()).to_json(), json!(198));
    }

    #[test]
    fn nulls_and_type_mismatch_yield_null_not_error() {
        let env = M([("name", json!("x"))].into());
        let e = parse(json!({"add": [{"field": "missing"}, {"lit": 1}]}));
        assert_eq!(eval(&e, &env, now()), FVal::Null);
        let e = parse(json!({"div": [{"lit": 1}, {"lit": 0}]}));
        assert_eq!(eval(&e, &env, now()), FVal::Null);
        let e = parse(json!({"mul": [{"field": "name"}, {"lit": 2}]}));
        assert_eq!(eval(&e, &env, now()), FVal::Null);
        let e = parse(json!({"cmp": [{"field": "missing"}, "eq", {"lit": null}]}));
        assert_eq!(eval(&e, &env, now()), FVal::Bool(false));
    }

    #[test]
    fn strings_dates_and_conditions() {
        let env = M([("name", json!("Ab")), ("due", json!("2026-10-30"))].into());
        let e = parse(json!({"concat": [{"upper": {"field": "name"}}, {"lit": "-"}, {"lit": 1}]}));
        assert_eq!(eval(&e, &env, now()).to_json(), json!("AB-1"));
        let e = parse(json!({"date_diff": ["now", {"field": "due"}]}));
        assert_eq!(eval(&e, &env, now()).to_json(), json!(8));
        let e = parse(json!({"date_add": [{"field": "due"}, {"lit": 3}]}));
        assert_eq!(eval(&e, &env, now()).to_json(), json!("2026-11-02"));
        let e = parse(json!({"if": [
            {"cmp": [{"field": "due"}, "gt", "now"]}, {"lit": "未来"}, {"lit": "過去"}
        ]}));
        assert_eq!(eval(&e, &env, now()).to_json(), json!("未来"));
        let e = parse(json!({"switch": {
            "value": {"field": "name"},
            "cases": [{"when": {"lit": "Ab"}, "then": {"lit": 1}}],
            "default": {"lit": 0}
        }}));
        assert_eq!(eval(&e, &env, now()).to_json(), json!(1));
        let e = parse(json!({"and": [{"lit": true}, {"not": {"lit": false}}]}));
        assert_eq!(eval(&e, &env, now()).to_json(), json!(true));
    }
}
