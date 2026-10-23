//! IR → SQL の書き出し（検証用・`policy_threat_it` などの IR のクエリを SQL 前段面でも流して結果を比べる）。
//!
//! IR の条件は 2 値論理なので、各葉を `(…) IS TRUE` で 2 値に固定して書き出す（SQL 側の 3 値論理の翻訳を
//! 通っても IR と同じ行を選ぶ）。書き出せない IR（部分一致検索・スナップショットなど）は `None`。

use std::fmt::Write as _;

use serde_json::Value;

use crate::query::ir::{CondOp, Condition, DataQuery, HavingCond, Metric, MetricSpec};

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn col(s: &str) -> String {
    match s.split_once('.') {
        Some((a, k)) => format!("{a}.{}", ident(k)),
        None => ident(s),
    }
}

fn lit(v: &Value) -> Option<String> {
    Some(match v {
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Number(n) => n.to_string(),
        _ => return None,
    })
}

fn cond(c: &Condition) -> Option<String> {
    Some(match c {
        Condition::All(v) if v.is_empty() => "TRUE".into(),
        Condition::Any(v) if v.is_empty() => "FALSE".into(),
        Condition::All(v) | Condition::Any(v) => {
            let j = if matches!(c, Condition::All(_)) {
                " AND "
            } else {
                " OR "
            };
            let parts = v.iter().map(cond).collect::<Option<Vec<_>>>()?;
            format!("({})", parts.join(j))
        }
        Condition::Not(x) => format!("(NOT {})", cond(x)?),
        Condition::Link(_) => return None,
        Condition::Cmp(cmp) => {
            let fld = col(&cmp.field);
            let v = cmp.value.as_ref();
            let op =
                |o: &str| -> Option<String> { Some(format!("(({fld} {o} {}) IS TRUE)", lit(v?)?)) };
            match cmp.op {
                CondOp::Eq => op("=")?,
                CondOp::Neq => format!("(NOT (({fld} = {}) IS TRUE))", lit(v?)?),
                CondOp::Lt => op("<")?,
                CondOp::Lte => op("<=")?,
                CondOp::Gt => op(">")?,
                CondOp::Gte => op(">=")?,
                CondOp::In => {
                    let arr = v?.as_array()?;
                    if arr.is_empty() {
                        return Some("FALSE".into());
                    }
                    let items = arr.iter().map(lit).collect::<Option<Vec<_>>>()?;
                    format!("(({fld} IN ({})) IS TRUE)", items.join(", "))
                }
                CondOp::Exists => format!("({fld} IS NOT NULL)"),
                CondOp::IsNull => format!("({fld} IS NULL)"),
                CondOp::StartsWith | CondOp::Contains | CondOp::EndsWith => {
                    let s = v?.as_str()?;
                    if s.contains(['%', '_', '\\']) {
                        return None;
                    }
                    let p = match cmp.op {
                        CondOp::StartsWith => format!("{s}%"),
                        CondOp::Contains => format!("%{s}%"),
                        _ => format!("%{s}"),
                    };
                    format!("(({fld} LIKE {}) IS TRUE)", lit(&Value::String(p))?)
                }
                CondOp::Matches => return None,
            }
        }
    })
}

fn metric(m: &MetricSpec) -> String {
    let f = match m.metric {
        Metric::Count => return "COUNT(*)".into(),
        Metric::Sum => "SUM",
        Metric::Avg => "AVG",
        Metric::Min => "MIN",
        Metric::Max => "MAX",
    };
    format!("{f}({})", m.field.as_deref().map(col).unwrap_or_default())
}

fn having(h: &HavingCond) -> Option<String> {
    Some(match h {
        HavingCond::All(v) | HavingCond::Any(v) => {
            let j = if matches!(h, HavingCond::All(_)) {
                " AND "
            } else {
                " OR "
            };
            let parts = v.iter().map(having).collect::<Option<Vec<_>>>()?;
            format!("({})", parts.join(j))
        }
        HavingCond::Not(x) => format!("(NOT {})", having(x)?),
        HavingCond::Cmp(c) => {
            let o = match c.op {
                CondOp::Eq => "=",
                CondOp::Neq => "<>",
                CondOp::Lt => "<",
                CondOp::Lte => "<=",
                CondOp::Gt => ">",
                CondOp::Gte => ">=",
                _ => return None,
            };
            format!("({} {o} {})", metric(&c.metric), c.value)
        }
    })
}

/// IR を SQL に書き出す（`table` は FROM のテーブル名・結合は扱わない）。
#[doc(hidden)]
pub fn render_sql(table: &str, q: &DataQuery, columns: &[&str]) -> Option<String> {
    if q.search.is_some() || q.snapshot || !q.joins.is_empty() || q.offset.unwrap_or(0) != 0 {
        return None;
    }
    let mut s = String::from("SELECT ");
    let aggregate = !q.aggregate.is_empty();
    let mut items: Vec<String> = Vec::new();
    if aggregate {
        items.extend(q.group_by.iter().map(|g| col(g)));
        items.extend(q.aggregate.iter().map(metric));
    } else {
        items.extend(columns.iter().map(|c| col(c)));
    }
    if items.is_empty() {
        return None;
    }
    s.push_str(&items.join(", "));
    let _ = write!(s, " FROM {}", ident(table));
    if let Some(f) = &q.filter {
        let _ = write!(s, " WHERE {}", cond(f)?);
    }
    if aggregate {
        if !q.group_by.is_empty() {
            let g: Vec<String> = q.group_by.iter().map(|g| col(g)).collect();
            let _ = write!(s, " GROUP BY {}", g.join(", "));
        }
        if let Some(h) = &q.having {
            let _ = write!(s, " HAVING {}", having(h)?);
        }
    } else if !q.sort.is_empty() {
        let o: Vec<String> = q
            .sort
            .iter()
            .map(|k| {
                format!(
                    "{}{}",
                    col(&k.field),
                    if k.descending { " DESC" } else { "" }
                )
            })
            .collect();
        let _ = write!(s, " ORDER BY {}", o.join(", "));
    }
    if let Some(n) = q.limit {
        if !aggregate {
            let _ = write!(s, " LIMIT {n}");
        }
    }
    Some(s)
}

/// 擬似列の名前（render の呼び出し側が射影に使う）。
#[doc(hidden)]
pub const RENDER_PSEUDO: &[&str] = &["_id", "_owner", "_created_at", "_updated_at"];
