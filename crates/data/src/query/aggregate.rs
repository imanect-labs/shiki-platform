//! 集計結果の組み立て＋スモールセル抑制（Task 9.4・PIT-17・Task 15.3 で索引経路へ）。
//!
//! 集計にも行述語を無条件合成する（不可視行は件数・合計に混入しない）。SQL は索引経路
//! （[`super::planner::lower`]・候補 → 検査 → 索引値でグループ化）と本体経路（[`super::v1`]）のどちらも
//! 同じ列（`grp_count`, `g0..`, `m0..`）を返し、ここで共通に組み立てる。
//! 各グループの**実件数が K 未満なら値を返さず抑制**する（個人特定・反復差分の逆算を難しくする。
//! DP ではない・集計クエリの監査で補う）。

use serde_json::{Map, Value};
use sqlx::Row;

use super::bound::BoundQuery;
use super::ir::AggregateGroup;
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
            let v: Option<String> = row.try_get(format!("g{i}").as_str()).map_err(map_db)?;
            key.insert(
                q.field(*g).key.clone(),
                v.map_or(Value::Null, Value::String),
            );
        }
        let mut values = Vec::with_capacity(q.metrics.len());
        for (i, (m, _)) in q.metrics.iter().enumerate() {
            let v: Option<f64> = row.try_get(format!("m{i}").as_str()).map_err(map_db)?;
            values.push(metric_value(*m, v));
        }
        let (value, values) = suppress(cnt, k, values, &mut suppressed);
        groups.push(AggregateGroup { key, value, values });
    }
    Ok((groups, suppressed))
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
