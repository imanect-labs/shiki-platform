//! 派生値の行ごとの計算（純関数・data-platform §7.2・§7.3）。
//!
//! 参照先の行（[`RefRows`]）は呼び出し側が用意する:
//! - 読取（[`super`]）: **閲覧者本人の権限で**引いた可視行だけ（テーブル viewer・行述語の SQL 前段＋
//!   Rust 事後検査・参照先のフィールドマスク適用済み）。見えない参照先は「無い」のと同じに扱う（PIT-20）。
//! - 材料化（[`super::materialize`]）: `invariant` と証明できた列だけを、全行から計算する。
//!
//! どちらも同じ関数で値を出すので、材料化した値は「誰が読んでも同じ」読取時の値と一致する。

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::graph::eval_order;
use super::types::{Materialization, RollupOp};
use crate::formula::decimal::Decimal;
use crate::formula::eval::{compare, eval, Env, FVal};
use crate::formula::FormulaExpr;
use crate::model::{ComputedOp, FieldDef, FieldType, TableSchema};

/// 1 本のリンク列で辿った参照先の行（外部形 `key → 値`）。
#[derive(Debug, Default, Clone)]
pub(crate) struct RefRows {
    /// 参照先の id → 行（読取では可視行だけ・マスク済み）。
    pub rows: HashMap<Uuid, Map<String, Value>>,
    /// 参照先のフィールドの型（値の解釈に使う）。
    pub types: HashMap<String, FieldType>,
    /// スモールセル抑制の K（参照先の `aggregate_min_rows` を継承・PIT-17）。
    pub min_rows: i64,
}

/// 行のリンク列の値（参照先 id の並び）。保存形は配列（移行前の v1 の単一 UUID 文字列も要素 1 件として読む）、
/// 対称列は読取時に配列で入れたもの。
pub(crate) fn linked_ids(row: &Map<String, Value>, link_key: &str) -> Vec<Uuid> {
    match row.get(link_key) {
        Some(Value::String(s)) => Uuid::parse_str(s).into_iter().collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// ロールアップの値。可視なリンク先が K 件未満なら抑制（null）。
pub(crate) fn rollup_value(
    ids: &[Uuid],
    refs: Option<&RefRows>,
    target: Option<&str>,
    op: RollupOp,
) -> FVal {
    let Some(refs) = refs else {
        return FVal::Null;
    };
    let visible: Vec<&Map<String, Value>> = ids.iter().filter_map(|i| refs.rows.get(i)).collect();
    if (visible.len() as i64) < refs.min_rows {
        return FVal::Null;
    }
    let ty = target.and_then(|t| refs.types.get(t).copied());
    let vals: Vec<FVal> = match target {
        Some(t) => visible
            .iter()
            .map(|r| r.get(t).map_or(FVal::Null, |v| FVal::from_json(v, ty)))
            .filter(|v| *v != FVal::Null)
            .collect(),
        None => Vec::new(),
    };
    let nums = || {
        vals.iter().filter_map(|v| match v {
            FVal::Num(d) => Some(*d),
            _ => None,
        })
    };
    match op {
        RollupOp::Count => FVal::Num(Decimal::from_i64(visible.len() as i64)),
        RollupOp::Sum => nums()
            .try_fold(Decimal::ZERO, Decimal::checked_add)
            .map_or(FVal::Null, FVal::Num),
        RollupOp::Avg => {
            let n: Vec<Decimal> = nums().collect();
            if n.is_empty() {
                return FVal::Null;
            }
            n.iter()
                .try_fold(Decimal::ZERO, |a, b| a.checked_add(*b))
                .and_then(|s| s.checked_div(Decimal::from_i64(n.len() as i64)))
                .map_or(FVal::Null, FVal::Num)
        }
        RollupOp::Min | RollupOp::Max => {
            let mut best: Option<&FVal> = None;
            for v in &vals {
                best = match best {
                    None => Some(v),
                    Some(b) => match compare(v, b) {
                        Some(o) if (op == RollupOp::Min) == o.is_lt() && o.is_ne() => Some(v),
                        _ => Some(b),
                    },
                };
            }
            best.cloned().unwrap_or(FVal::Null)
        }
        RollupOp::Concat => FVal::Text(
            vals.iter()
                .map(FVal::text)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

/// 1 テーブル分の評価器。
pub(crate) struct RowEval<'a> {
    pub schema: &'a TableSchema,
    /// リンク列の参照名 → 参照先の行。
    pub refs: &'a HashMap<String, RefRows>,
    pub now: DateTime<Utc>,
    /// 材料化（`invariant` の列だけを計算し、他は null として扱う）。
    pub only_invariant: bool,
    /// 閲覧者に読めない自テーブルの列（派生列を含む）。数式の材料にしない（PIT-19）。
    pub masked: &'a HashSet<String>,
}

struct RowEnv<'a> {
    schema: &'a TableSchema,
    row: &'a Map<String, Value>,
    results: &'a HashMap<String, FVal>,
    inline: &'a HashMap<usize, FVal>,
    masked: &'a HashSet<String>,
}

impl Env for RowEnv<'_> {
    fn field(&self, key: &str) -> FVal {
        if self.masked.contains(key) {
            return FVal::Null;
        }
        if let Some(v) = self.results.get(key) {
            return v.clone();
        }
        let ty = self.schema.field(key).map(|f| f.field_type);
        if ty.is_some_and(FieldType::is_derived) {
            return FVal::Null;
        }
        self.row
            .get(key)
            .map_or(FVal::Null, |v| FVal::from_json(v, ty))
    }

    fn rollup(&self, node: &FormulaExpr) -> FVal {
        self.inline
            .get(&std::ptr::from_ref(node).addr())
            .cloned()
            .unwrap_or(FVal::Null)
    }
}

impl RowEval<'_> {
    /// 行の派生フィールドを依存順に計算する（`row` は閲覧者に読める列だけを持つこと）。
    pub(crate) fn evaluate(&self, row: &Map<String, Value>) -> Map<String, Value> {
        let mut results: HashMap<String, FVal> = HashMap::new();
        let mut out = Map::new();
        for f in eval_order(self.schema) {
            if self.only_invariant && f.materialization != Some(Materialization::Invariant) {
                continue;
            }
            let (v, json) = self.one(f, row, &results);
            results.insert(f.key.clone(), v);
            out.insert(f.key.clone(), json);
        }
        out
    }

    fn one(
        &self,
        f: &FieldDef,
        row: &Map<String, Value>,
        results: &HashMap<String, FVal>,
    ) -> (FVal, Value) {
        let single = |v: FVal| {
            let j = v.to_json();
            (v, j)
        };
        match f.field_type {
            FieldType::Lookup => {
                let Some(d) = &f.lookup else {
                    return single(FVal::Null);
                };
                let refs = self.refs.get(&d.via_field);
                let ty = refs.and_then(|r| r.types.get(&d.target_field).copied());
                let vals: Vec<Value> = linked_ids(row, &d.via_field)
                    .iter()
                    .filter_map(|id| refs.and_then(|r| r.rows.get(id)))
                    .filter_map(|r| r.get(&d.target_field).cloned())
                    .filter(|v| !v.is_null())
                    .collect();
                let via_single = self
                    .schema
                    .field(&d.via_field)
                    .is_some_and(|v| v.link.as_ref().is_none_or(|l| l.cardinality.src_single()));
                if via_single {
                    let v = vals.into_iter().next().unwrap_or(Value::Null);
                    (FVal::from_json(&v, ty), v)
                } else {
                    let arr = Value::Array(vals);
                    (FVal::from_json(&arr, ty), arr)
                }
            }
            FieldType::Rollup => {
                let Some(d) = &f.rollup else {
                    return single(FVal::Null);
                };
                single(rollup_value(
                    &linked_ids(row, &d.link),
                    self.refs.get(&d.link),
                    d.target.as_deref(),
                    d.op,
                ))
            }
            FieldType::Computed => {
                let Some(d) = &f.computed else {
                    return single(FVal::Null);
                };
                let env = RowEnv {
                    schema: self.schema,
                    row,
                    results,
                    inline: &HashMap::new(),
                    masked: self.masked,
                };
                let vals: Vec<FVal> = d.fields.iter().map(|k| env.field(k)).collect();
                single(match d.op {
                    ComputedOp::Sum => vals
                        .iter()
                        .filter_map(|v| match v {
                            FVal::Num(n) => Some(*n),
                            _ => None,
                        })
                        .try_fold(Decimal::ZERO, Decimal::checked_add)
                        .map_or(FVal::Null, FVal::Num),
                    ComputedOp::Concat => FVal::Text(
                        vals.iter()
                            .filter(|v| matches!(v, FVal::Text(_)))
                            .map(FVal::text)
                            .collect(),
                    ),
                })
            }
            FieldType::Formula => {
                let Some(e) = &f.formula else {
                    return single(FVal::Null);
                };
                let inline: HashMap<usize, FVal> = e
                    .rollups()
                    .into_iter()
                    .map(|n| {
                        let v = match n {
                            FormulaExpr::Rollup { link, target, op } => rollup_value(
                                &linked_ids(row, link),
                                self.refs.get(link),
                                target.as_deref(),
                                *op,
                            ),
                            _ => FVal::Null,
                        };
                        (std::ptr::from_ref(n).addr(), v)
                    })
                    .collect();
                let env = RowEnv {
                    schema: self.schema,
                    row,
                    results,
                    inline: &inline,
                    masked: self.masked,
                };
                single(eval(e, &env, self.now))
            }
            _ => single(FVal::Null),
        }
    }
}

#[cfg(test)]
#[path = "compute_tests.rs"]
mod tests;
