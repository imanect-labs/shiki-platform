//! 本体経路（v1 方式・`data_record.data` の JSONB を直接評価する単発 SQL）。
//!
//! 役割は 3 つ:
//! 1. **機能フラグの切戻し先**（data-platform §13 M3）。索引経路と同じ IR・同じ全順序・同じカーソルで動く。
//! 2. **索引が未完成のときの正しい経路**: 必要な (フィールド, 索引) が `active` でない（バックフィル中・
//!    Task 15.2 より前のテーブル）リクエストはこちらで処理する（遅いが正しい・半端な索引を使わない）。
//! 3. **parity 検証の基準**（同一クエリの新旧結果の一致）。
//!
//! 行述語は v1 のコンパイラ（[`crate::policy::compile::compile_read_predicate`]）。クエリ条件は §3.1 の
//! 2 値論理（`coalesce(…, false)` で NULL を偽に固定し、`NOT` を補集合にする）。
//! 旧方式の式インデックスは撤去済み（Task 15.3）なので、索引を使わずテーブルのパーティションを走査する。

use std::fmt::Write as _;

use crate::field_key::{is_json_key, json_key};
use crate::model::{SYSTEM_FIELD_CREATED_AT, SYSTEM_FIELD_OWNER, SYSTEM_FIELD_UPDATED_AT};
use crate::query::bound::{Atom, BField, BoundQuery, Expr, FieldKind, Leaf, Val};
use crate::query::planner::keys::Pos;
use crate::query::planner::lower::Lowerer;
use crate::query::sql::{like_escape, Bind, SqlBuf};
use crate::DataError;

/// ISO-8601（擬似列と同じ固定幅表記・`format_timestamp` と一致）。
fn iso(col: &str) -> String {
    format!("to_char({col} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
}

/// フィールドの本体上の値（単値・text は `COLLATE "C"`・number は numeric）。
fn value_expr(f: &BField) -> Result<String, DataError> {
    Ok(match f.id {
        SYSTEM_FIELD_OWNER if f.is_pseudo() => "(r.owner COLLATE \"C\")".into(),
        SYSTEM_FIELD_CREATED_AT if f.is_pseudo() => {
            format!("({} COLLATE \"C\")", iso("r.created_at"))
        }
        SYSTEM_FIELD_UPDATED_AT if f.is_pseudo() => {
            format!("({} COLLATE \"C\")", iso("r.updated_at"))
        }
        _ => {
            let jk = json_key(f.id);
            if !is_json_key(&jk) {
                return Err(DataError::Internal(format!("JSONB キー '{jk}' が不正です")));
            }
            match f.kind {
                FieldKind::Num => format!("((r.data ->> '{jk}'))::numeric"),
                // リンクは多値として atom_sql が要素ごとに評価する（ソート・グループ化には使えない）。
                FieldKind::Link => format!("lower(r.data ->> '{jk}')"),
                _ => format!("((r.data ->> '{jk}') COLLATE \"C\")"),
            }
        }
    })
}

/// 原子式の 2 値評価（値なしは偽）。
fn atom_sql(q: &BoundQuery, a: &Atom, sql: &mut SqlBuf) -> Result<String, DataError> {
    let f = q.field(a.field);
    if matches!(f.kind, FieldKind::Multi | FieldKind::Link) {
        let jk = json_key(f.id);
        if !is_json_key(&jk) {
            return Err(DataError::Internal(format!("JSONB キー '{jk}' が不正です")));
        }
        // 多値は要素のどれかが条件を満たせば真（索引経路と同じ）。リンクの保存形は配列だが、値の移行
        // （v1 の単一 UUID 文字列 → 配列・crate::link_values）が終わるまでは文字列も要素 1 件として読む。
        let arr = format!(
            "(CASE jsonb_typeof(r.data -> '{jk}') WHEN 'array' THEN r.data -> '{jk}' \
             WHEN 'string' THEN jsonb_build_array(r.data -> '{jk}') ELSE '[]'::jsonb END)"
        );
        if a.leaf == Leaf::Exists {
            return Ok(format!("(jsonb_array_length({arr}) > 0)"));
        }
        let (kind, elem) = if f.kind == FieldKind::Link {
            (FieldKind::Link, "lower(e.v)")
        } else {
            (FieldKind::Text, "(e.v COLLATE \"C\")")
        };
        let cond = leaf_sql(&a.leaf, kind, elem, sql);
        return Ok(format!(
            "EXISTS (SELECT 1 FROM jsonb_array_elements_text({arr}) e(v) WHERE {cond})"
        ));
    }
    let v = value_expr(f)?;
    if a.leaf == Leaf::Exists {
        return Ok(format!("({v} IS NOT NULL)"));
    }
    Ok(format!(
        "coalesce({}, false)",
        leaf_sql(&a.leaf, f.kind, &v, sql)
    ))
}

fn val_ph(kind: FieldKind, v: &Val, sql: &mut SqlBuf) -> String {
    match (kind, v) {
        (_, Val::Num(d)) => sql.typed(Bind::Numeric(d.canonical())),
        (_, Val::Ref(u)) => sql.typed(Bind::Text(u.to_string())),
        (_, Val::Text(s)) => {
            let ph = sql.push(Bind::Text(s.clone()));
            format!("{ph}::text COLLATE \"C\"")
        }
    }
}

fn leaf_sql(leaf: &Leaf, kind: FieldKind, v: &str, sql: &mut SqlBuf) -> String {
    match leaf {
        Leaf::Exists => format!("{v} IS NOT NULL"),
        Leaf::Eq(x) => format!("{v} = {}", val_ph(kind, x, sql)),
        Leaf::In(xs) => {
            if xs.is_empty() {
                return "FALSE".into();
            }
            let parts: Vec<String> = xs
                .iter()
                .map(|x| format!("{v} = {}", val_ph(kind, x, sql)))
                .collect();
            format!("({})", parts.join(" OR "))
        }
        Leaf::Range { lo, hi } => {
            let mut parts = Vec::new();
            if let Some((x, inc)) = lo {
                let op = if *inc { ">=" } else { ">" };
                parts.push(format!("{v} {op} {}", val_ph(kind, x, sql)));
            }
            if let Some((x, inc)) = hi {
                let op = if *inc { "<=" } else { "<" };
                parts.push(format!("{v} {op} {}", val_ph(kind, x, sql)));
            }
            if parts.is_empty() {
                format!("{v} IS NOT NULL")
            } else {
                format!("({})", parts.join(" AND "))
            }
        }
        Leaf::StartsWith(p) => {
            let ph = sql.push(Bind::Text(p.clone()));
            format!("starts_with({v}, {ph}::text)")
        }
        Leaf::Contains(p) => {
            let ph = sql.push(Bind::Text(format!("%{}%", like_escape(p))));
            format!("{v} LIKE {ph}::text")
        }
        Leaf::EndsWith(p) => {
            let ph = sql.push(Bind::Text(format!("%{}", like_escape(p))));
            format!("{v} LIKE {ph}::text")
        }
    }
}

/// 条件式（2 値論理）。
pub(crate) fn expr_sql(q: &BoundQuery, e: &Expr, sql: &mut SqlBuf) -> Result<String, DataError> {
    Ok(match e {
        Expr::True => "TRUE".into(),
        Expr::False => "FALSE".into(),
        Expr::Atom(a) => atom_sql(q, a, sql)?,
        Expr::Not(inner) => format!("NOT ({})", expr_sql(q, inner, sql)?),
        Expr::And(v) | Expr::Or(v) => {
            let j = if matches!(e, Expr::And(_)) {
                " AND "
            } else {
                " OR "
            };
            let parts = v
                .iter()
                .map(|c| expr_sql(q, c, sql))
                .collect::<Result<Vec<_>, _>>()?;
            format!("({})", parts.join(j))
        }
    })
}

/// 本体経路の WHERE の後半（tenant_id・行述語・条件）。`tenant_int` / `table_int` の条件は各文が持つ。`policy` は v1 の述語（別名 `r`）。
pub(crate) fn where_sql(
    q: &BoundQuery,
    tenant_id: &str,
    policy: &str,
    sql: &mut SqlBuf,
) -> Result<String, DataError> {
    let t = sql.push(Bind::Text(tenant_id.to_string()));
    let cond = expr_sql(q, &q.filter, sql)?;
    Ok(format!("r.tenant_id = {t} AND ({policy}) AND ({cond})"))
}

/// 1 ページ（全ソートキーで並べ、位置より後ろの先頭 `limit` 件）。列は候補 SQL と同じ
/// （`record_int`, `k0..`）。
pub(crate) fn page_sql(
    low: &Lowerer<'_>,
    where_clause: &str,
    after: Option<&Pos>,
    limit: i64,
    sql: &mut SqlBuf,
) -> Result<String, DataError> {
    let q = low.q;
    let mut cols = String::new();
    let mut outer = String::new();
    let mut order = Vec::new();
    for (i, k) in q.sort.iter().enumerate() {
        let v = value_expr(q.field(k.field))?;
        let _ = write!(cols, ", {v} AS k{i}");
        let _ = write!(outer, ", z.k{i}::text AS k{i}");
        let dir = if k.desc { "DESC" } else { "ASC" };
        order.push(format!("(z.k{i} IS NULL), z.k{i} {dir}"));
    }
    let rdir = if q.sort.first().is_some_and(|k| k.desc) {
        "DESC"
    } else {
        "ASC"
    };
    order.push(format!("z.record_int {rdir}"));
    let keyset = after.map_or_else(|| "TRUE".to_string(), |p| low.keyset_sql(p, sql));
    let lim = sql.typed(Bind::I64(limit));
    Ok(format!(
        "SELECT z.record_int{outer} FROM (SELECT r.record_int{cols} FROM data_record r \
         WHERE r.tenant_int = $1 AND r.table_int = $2 AND {where_clause}) z WHERE {keyset} ORDER BY {} LIMIT {lim}",
        order.join(", ")
    ))
}

/// 上限付き件数。
pub(crate) fn count_sql(where_clause: &str, cap: i64, sql: &mut SqlBuf) -> String {
    let ph = sql.typed(Bind::I64(cap + 1));
    format!(
        "SELECT count(*) FROM (SELECT 1 FROM data_record r \
         WHERE r.tenant_int = $1 AND r.table_int = $2 AND {where_clause} LIMIT {ph}) s"
    )
}

/// 集計（列は索引経路と同じ: `grp_count`, `g0..`, `m0..`）。
pub(crate) fn aggregate_sql(q: &BoundQuery, where_clause: &str) -> Result<String, DataError> {
    use crate::query::ir::Metric;
    let mut select = vec!["count(*)::int8 AS grp_count".to_string()];
    let mut group = Vec::new();
    for (i, g) in q.group_by.iter().enumerate() {
        let f = q.field(*g);
        let jk = json_key(f.id);
        if !is_json_key(&jk) {
            return Err(DataError::Internal(format!("JSONB キー '{jk}' が不正です")));
        }
        // 索引経路と同じ表現（number は numeric の text 出力）。
        let v = if f.kind == FieldKind::Num {
            format!("((r.data ->> '{jk}'))::numeric::text")
        } else {
            format!("(r.data ->> '{jk}')")
        };
        select.push(format!("{v} AS g{i}"));
        group.push(v);
    }
    for (i, (m, field)) in q.metrics.iter().enumerate() {
        let Some(fid) = field else {
            select.push(format!("count(*)::float8 AS m{i}"));
            continue;
        };
        let jk = json_key(*fid);
        if !is_json_key(&jk) {
            return Err(DataError::Internal(format!("JSONB キー '{jk}' が不正です")));
        }
        let func = match m {
            Metric::Sum => "sum",
            Metric::Avg => "avg",
            Metric::Min => "min",
            Metric::Max => "max",
            Metric::Count => "count",
        };
        select.push(format!(
            "({func}(((r.data ->> '{jk}'))::numeric))::float8 AS m{i}"
        ));
    }
    let group_sql = if group.is_empty() {
        String::new()
    } else {
        format!(" GROUP BY {}", group.join(", "))
    };
    Ok(format!(
        "SELECT {} FROM data_record r \
         WHERE r.tenant_int = $1 AND r.table_int = $2 AND {where_clause}{group_sql}",
        select.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]
    use super::*;
    use crate::model::{FieldDef, FieldType, TableSchema};
    use std::collections::HashSet;

    fn bound(filter: serde_json::Value) -> BoundQuery {
        let mut tags = FieldDef::new("tags", FieldType::MultiSelect);
        tags.indexed = true;
        tags.options = vec!["a".into()];
        let mut amount = FieldDef::new("amount", FieldType::Number);
        amount.indexed = true;
        let mut s = TableSchema {
            fields: vec![tags, amount],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        let q: crate::query::ir::DataQuery =
            serde_json::from_value(serde_json::json!({ "filter": filter })).unwrap();
        crate::query::bound::bind_query(&s, &HashSet::new(), &q).unwrap()
    }

    #[test]
    fn two_valued_logic_is_fixed_with_coalesce() {
        let q =
            bound(serde_json::json!({"not": {"cmp": {"field": "amount", "op": "lt", "value": 5}}}));
        let mut sql = SqlBuf::scoped(1, 2);
        let s = expr_sql(&q, &q.filter, &mut sql).unwrap();
        assert_eq!(
            s,
            "NOT (coalesce((((r.data ->> 'f17'))::numeric < $3::numeric), false))"
        );
    }

    #[test]
    fn multi_select_eq_means_contains() {
        let q = bound(serde_json::json!({"cmp": {"field": "tags", "op": "eq", "value": "a"}}));
        let mut sql = SqlBuf::scoped(1, 2);
        let s = expr_sql(&q, &q.filter, &mut sql).unwrap();
        assert!(s.contains("jsonb_array_elements_text"), "{s}");
        assert!(!s.contains("'a'"), "値はバインド: {s}");
    }
}
