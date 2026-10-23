//! 集計結果の組み立て＋スモールセル抑制（Task 9.4・PIT-17・Task 15.3 で索引経路へ）。
//!
//! 集計にも行述語を無条件合成する（不可視行は件数・合計に混入しない）。SQL は索引経路
//! （[`super::planner::lower`]・候補 → 検査 → 索引値でグループ化）と本体経路（[`super::v1`]）のどちらも
//! 同じ列（`grp_count`, `g0..`, `m0..`）を返し、ここで共通に組み立てる。
//! 各グループの**実件数が K 未満なら値を返さず抑制**する（個人特定・反復差分の逆算を難しくする。
//! DP ではない・集計クエリの監査で補う）。

use serde_json::{Map, Value};
use sqlx::Row;

use super::bound::{BHaving, BoundQuery, FieldKind, GroupKey};
use super::ir::{AggregateGroup, CondOp};
use crate::{map_db, DataError};

/// 集計の行をグループへ組み立てる。返り値 = (グループ列, 抑制発生)。
pub(crate) fn assemble(
    q: &BoundQuery,
    rows: &[sqlx::postgres::PgRow],
    k: i64,
) -> Result<(Vec<AggregateGroup>, bool), DataError> {
    let mut groups = Vec::with_capacity(rows.len());
    let mut suppressed = false;
    for row in rows {
        let cnt: i64 = row.try_get("grp_count").map_err(map_db)?;
        let mut key = Map::new();
        for (i, g) in q.group_by.iter().enumerate() {
            let f = q.col_field(*g);
            if f.derived && f.kind == FieldKind::Text {
                // 材料化した派生列の切り詰め行（先頭 2,000B だけ）は値を区別できない（§3.3・本体で再確認
                // できない）。違う値を 1 つのグループに混ぜた集計を返さない。
                let truncated: Option<bool> =
                    row.try_get(format!("gt{i}").as_str()).map_err(map_db)?;
                if truncated == Some(true) {
                    return Err(DataError::Invalid(format!(
                        "派生フィールド '{}' に索引キーの上限（{} バイト）を超える値があるため group_by に使えません",
                        f.key,
                        crate::index::MAX_INDEX_KEY_BYTES
                    )));
                }
            }
            let v: Option<String> = row.try_get(format!("g{i}").as_str()).map_err(map_db)?;
            key.insert(q.col_key(*g), v.map_or(Value::Null, Value::String));
        }
        let mut values = Vec::with_capacity(q.metrics.len());
        for (i, (m, _)) in q.metrics.iter().enumerate() {
            let v: Option<f64> = row.try_get(format!("m{i}").as_str()).map_err(map_db)?;
            values.push(metric_value(*m, v));
        }
        let (value, values) = suppress(cnt, k, values, &mut suppressed);
        groups.push(AggregateGroup { key, value, values });
    }
    let groups = finish(q, groups);
    Ok((groups, suppressed))
}

/// HAVING → 並べ替え → 件数の上限 → 出力しない指標を外す（抑制した値は `NULL` のまま比較される）。
fn finish(q: &BoundQuery, mut groups: Vec<AggregateGroup>) -> Vec<AggregateGroup> {
    if let Some(h) = &q.having {
        groups.retain(|g| having(h, &g.values));
    }
    if !q.group_sort.is_empty() {
        groups.sort_by(|a, b| {
            for (k, desc) in &q.group_sort {
                let (x, y) = match k {
                    GroupKey::Group(i) => {
                        let name = q
                            .group_by
                            .get(*i)
                            .map(|c| q.col_key(*c))
                            .unwrap_or_default();
                        (a.key.get(&name).cloned(), b.key.get(&name).cloned())
                    }
                    GroupKey::Metric(i) => (a.values.get(*i).cloned(), b.values.get(*i).cloned()),
                };
                let o = cmp_nulls_last(x.as_ref(), y.as_ref(), *desc);
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        });
    }
    if let Some(n) = q.group_limit {
        groups.truncate(n as usize);
    }
    if q.hidden_metrics > 0 {
        let visible = q.metrics.len() - q.hidden_metrics;
        for g in &mut groups {
            g.values.truncate(visible);
        }
    }
    groups
}

/// 値の比較（数値は数値として・文字列は辞書順・値なしは昇順でも降順でも末尾）。
fn cmp_nulls_last(lhs: Option<&Value>, rhs: Option<&Value>, desc: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let lhs = lhs.filter(|v| !v.is_null());
    let rhs = rhs.filter(|v| !v.is_null());
    match (lhs, rhs) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => {
            let ord = match (as_num(left), as_num(right)) {
                (Some(ln), Some(rn)) => ln.partial_cmp(&rn).unwrap_or(Ordering::Equal),
                _ => left
                    .as_str()
                    .unwrap_or("")
                    .cmp(right.as_str().unwrap_or("")),
            };
            if desc {
                ord.reverse()
            } else {
                ord
            }
        }
    }
}

fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// HAVING の評価（抑制した値・値なしは `NULL` で、どの比較も成立しない）。
fn having(h: &BHaving, values: &[Value]) -> bool {
    match h {
        BHaving::All(v) => v.iter().all(|x| having(x, values)),
        BHaving::Any(v) => v.iter().any(|x| having(x, values)),
        BHaving::Not(x) => !having(x, values),
        BHaving::Cmp { metric, op, value } => {
            let Some(x) = values.get(*metric).and_then(Value::as_f64) else {
                return false;
            };
            match op {
                CondOp::Eq => (x - value).abs() < f64::EPSILON * x.abs().max(1.0),
                CondOp::Neq => (x - value).abs() >= f64::EPSILON * x.abs().max(1.0),
                CondOp::Lt => x < *value,
                CondOp::Lte => x <= *value,
                CondOp::Gt => x > *value,
                CondOp::Gte => x >= *value,
                _ => false,
            }
        }
    }
}

fn metric_value(m: super::ir::Metric, v: Option<f64>) -> Value {
    match (m, v) {
        // count は整数で返す（v1 互換）。
        (super::ir::Metric::Count, Some(c)) => Value::Number((c as i64).into()),
        (_, v) => v
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
    }
}

/// K 未満は値を伏せる（count 含む・全体集計も同様）。
fn suppress(cnt: i64, k: i64, values: Vec<Value>, suppressed: &mut bool) -> (Value, Vec<Value>) {
    if cnt < k {
        *suppressed = true;
        let n = values.len();
        return (Value::Null, vec![Value::Null; n]);
    }
    let first = values.first().cloned().unwrap_or(Value::Null);
    (first, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn small_cells_are_suppressed() {
        let mut s = false;
        let (v, vs) = suppress(4, 5, vec![json!(4), json!(10.5)], &mut s);
        assert!(s);
        assert_eq!(v, Value::Null);
        assert_eq!(vs, vec![Value::Null, Value::Null]);
        let mut s = false;
        let (v, vs) = suppress(5, 5, vec![json!(5)], &mut s);
        assert!(!s);
        assert_eq!(v, json!(5));
        assert_eq!(vs, vec![json!(5)]);
    }

    #[test]
    fn count_is_integer() {
        assert_eq!(
            metric_value(super::super::ir::Metric::Count, Some(3.0)),
            json!(3)
        );
        assert_eq!(
            metric_value(super::super::ir::Metric::Avg, Some(2.5)),
            json!(2.5)
        );
        assert_eq!(
            metric_value(super::super::ir::Metric::Max, None),
            Value::Null
        );
    }
}
