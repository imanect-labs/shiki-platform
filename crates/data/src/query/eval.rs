//! クエリ条件の 2 値論理の評価器（テスト専用・query-optimizer.md §3.1 の表をそのまま実装）。
//!
//! 正規化規則の前後一致（§9.2）を、SQL を介さず意味論の表だけで確かめるために使う。
//! 行述語の評価器（[`super::planner::verify`]）とは別物（行述語は v1 の意味論）。

use std::collections::HashMap;

use super::bound::{Expr, Leaf, Val};
use super::planner::normalize::{cmp_val, in_range};
use crate::model::FieldId;

/// 1 レコードの値（フィールド → 値の集合。値なしは空・単値は要素 1 個）。
pub(crate) type Rec = HashMap<FieldId, Vec<Val>>;

pub(crate) fn eval(e: &Expr, r: &Rec) -> bool {
    match e {
        Expr::True => true,
        // 参照先の行を持たない評価器（正規化の規則は半結合を葉として扱う）。
        Expr::False | Expr::Link(_) => false,
        Expr::Not(x) => !eval(x, r),
        Expr::And(v) => v.iter().all(|c| eval(c, r)),
        Expr::Or(v) => v.iter().any(|c| eval(c, r)),
        Expr::Atom(a) => {
            let vals = r.get(&a.field).map_or(&[][..], Vec::as_slice);
            match &a.leaf {
                Leaf::Exists => !vals.is_empty(),
                Leaf::Eq(c) => vals.iter().any(|v| same(v, c)),
                Leaf::In(cs) => vals.iter().any(|v| cs.iter().any(|c| same(v, c))),
                Leaf::Range { lo, hi } => {
                    vals.iter().any(|v| in_range(v, lo.as_ref(), hi.as_ref()))
                }
                Leaf::StartsWith(p) => vals
                    .iter()
                    .any(|v| v.as_text().is_some_and(|s| s.starts_with(p.as_str()))),
                Leaf::Contains(p) => vals
                    .iter()
                    .any(|v| v.as_text().is_some_and(|s| s.contains(p.as_str()))),
                Leaf::EndsWith(p) => vals
                    .iter()
                    .any(|v| v.as_text().is_some_and(|s| s.ends_with(p.as_str()))),
            }
        }
    }
}

fn same(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Ref(x), Val::Ref(y)) => x == y,
        _ => cmp_val(a, b) == Some(std::cmp::Ordering::Equal),
    }
}
