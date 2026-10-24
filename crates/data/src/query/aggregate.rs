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

/// 抑制の判定 1 つ（件数の列名・K）。FROM の表は `grp_count`（レコードの個数）と表の K、結合を残した
/// 集計で列を集計する結合先は、その結合先のレコードの個数と結合先の表の K（ロールアップと同じく参照先の
/// K を継承する・data-platform §7.2）。
pub(crate) struct MinRows {
    pub col: String,
    pub k: i64,
}

/// 抑制の判定の並び（FROM の表 → 列を集計する結合先）。結合先の材料が無い（開けない）なら既定の K
/// （その結合は組を作らないので結果は空）。
pub(crate) fn min_rows(
    table: &crate::model::DataTable,
    q: &BoundQuery,
    scopes: &super::scopes::ScopeTables,
) -> Vec<MinRows> {
    let k_of = |s: &crate::model::TableSchema| {
        s.aggregate_min_rows
            .unwrap_or(crate::DEFAULT_AGGREGATE_MIN_ROWS)
    };
    let mut out = vec![MinRows {
        col: "grp_count".to_string(),
        k: k_of(&table.schema),
    }];
    if q.joins.is_empty() {
        return out;
    }
    for k in q.metric_joins() {
        let kk = q
            .joins
            .get(k - 1)
            .and_then(|j| q.scopes.get(j.scope))
            .and_then(|s| scopes.get(s.table))
            .map_or(crate::DEFAULT_AGGREGATE_MIN_ROWS, |st| {
                k_of(&st.table.schema)
            });
        out.push(MinRows {
            col: super::planner::join::join_count_col(k),
            k: kk,
        });
    }
    out
}

/// 集計の行をグループへ組み立てる。返り値 = (グループ列, 抑制発生)。どれか 1 つでも K 未満なら抑制する。
pub(crate) fn assemble(
    q: &BoundQuery,
    rows: &[sqlx::postgres::PgRow],
    mins: &[MinRows],
) -> Result<(Vec<AggregateGroup>, bool), DataError> {
    let mut groups = Vec::with_capacity(rows.len());
    let mut suppressed = false;
    for row in rows {
        let mut small = false;
        for m in mins {
            let cnt: i64 = row.try_get(m.col.as_str()).map_err(map_db)?;
            small |= cnt < m.k;
        }
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
        let (value, values) = suppress(small, values, &mut suppressed);
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
                // グループ列は列型で比べ方を決める（キーは常に文字列。テキスト列の '10' と '2' を数値に
                // 解釈すると辞書順にならず、'2'・'10'・'1a' で比較が循環して全順序が崩れる）。指標は数値。
                let (x, y, numeric) = match k {
                    GroupKey::Group(i) => {
                        let col = q.group_by.get(*i).copied();
                        let name = col.map(|c| q.col_key(c)).unwrap_or_default();
                        let numeric = col.is_some_and(|c| q.col_field(c).kind == FieldKind::Num);
                        (a.key.get(&name), b.key.get(&name), numeric)
                    }
                    GroupKey::Metric(i) => (a.values.get(*i), b.values.get(*i), true),
                };
                let o = cmp_nulls_last(x, y, *desc, numeric);
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

/// 値の比較（値なしは昇順でも降順でも末尾）。どちらの比べ方でも全順序になる。
///
/// - `numeric`（数値列のグループ・指標）: 数値として比べる。数値に読めない値は（来ない想定だが）
///   数値の後ろに辞書順で並べる。
/// - それ以外（テキスト・日付・選択肢などのグループ）: 文字列の辞書順。数値に読めても数値にしない。
fn cmp_nulls_last(
    lhs: Option<&Value>,
    rhs: Option<&Value>,
    desc: bool,
    numeric: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let lhs = lhs.filter(|v| !v.is_null());
    let rhs = rhs.filter(|v| !v.is_null());
    match (lhs, rhs) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => {
            let text = || {
                left.as_str()
                    .unwrap_or("")
                    .cmp(right.as_str().unwrap_or(""))
            };
            let ord = if numeric {
                match (as_num(left), as_num(right)) {
                    (Some(ln), Some(rn)) => ln.total_cmp(&rn),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => text(),
                }
            } else {
                text()
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

/// K 未満（`small`）は値を伏せる（count 含む・全体集計も同様）。
fn suppress(small: bool, values: Vec<Value>, suppressed: &mut bool) -> (Value, Vec<Value>) {
    if small {
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
        let (v, vs) = suppress(4 < 5, vec![json!(4), json!(10.5)], &mut s);
        assert!(s);
        assert_eq!(v, Value::Null);
        assert_eq!(vs, vec![Value::Null, Value::Null]);
        let mut s = false;
        let (v, vs) = suppress(5 < 5, vec![json!(5)], &mut s);
        assert!(!s);
        assert_eq!(v, json!(5));
        assert_eq!(vs, vec![json!(5)]);
    }

    /// テキスト列のグループは辞書順（数値に読めるキーも数値にしない）・数値列のグループと指標は数値順。
    /// どちらも全順序（'2'・'10'・'1a' で循環しない）。
    #[test]
    fn group_keys_compare_by_column_type() {
        use std::cmp::Ordering;
        let s = |v: &str| json!(v);
        let (two, ten, mixed) = (s("2"), s("10"), s("1a"));
        let text = |a: &Value, b: &Value| cmp_nulls_last(Some(a), Some(b), false, false);
        assert_eq!(text(&ten, &two), Ordering::Less, "辞書順: '10' < '2'");
        assert_eq!(text(&mixed, &two), Ordering::Less);
        assert_eq!(text(&ten, &mixed), Ordering::Less);
        let mut keys = vec![two.clone(), ten.clone(), mixed.clone()];
        keys.sort_by(|a, b| text(a, b));
        assert_eq!(keys, vec![ten.clone(), mixed.clone(), two.clone()]);
        let num = |a: &Value, b: &Value| cmp_nulls_last(Some(a), Some(b), false, true);
        assert_eq!(num(&two, &ten), Ordering::Less, "数値順: 2 < 10");
        assert_eq!(num(&s("1.5"), &json!(2)), Ordering::Less);
        // 数値に読めない値は数値の後ろ（全順序を保つ）。
        assert_eq!(num(&ten, &mixed), Ordering::Less);
        assert_eq!(num(&mixed, &two), Ordering::Greater);
        // 値なしは昇順でも降順でも末尾。
        assert_eq!(
            cmp_nulls_last(None, Some(&two), true, false),
            Ordering::Greater
        );
        assert_eq!(cmp_nulls_last(Some(&two), None, true, true), Ordering::Less);
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
