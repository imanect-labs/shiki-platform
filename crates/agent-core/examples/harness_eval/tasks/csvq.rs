//! データ分析系の課題で共有する CSV の表と `query_csv` ツール（絞り込み・結合・集計・並べ替え）。
//!
//! 製品の構造化データ基盤（crates/data）のクエリに近い粒度にする: モデルは SQL を書かず、
//! JSON の引数で絞り込み・結合・集計を組み立てる。生の CSV は `read_file` でも読めるが、
//! 大きいので全部は窓に載らない（クエリを使う判断ができるかも見る）。

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use agent_core::Tool;
use serde_json::{json, Value};

use super::{arg_str, FnTool};

#[derive(Clone)]
pub(crate) struct Table {
    pub(crate) cols: Vec<String>,
    pub(crate) rows: Vec<Vec<String>>,
}

impl Table {
    pub(crate) fn new(cols: &[&str]) -> Self {
        Table {
            cols: cols.iter().map(|c| (*c).to_string()).collect(),
            rows: Vec::new(),
        }
    }

    pub(crate) fn to_csv(&self) -> String {
        let mut s = self.cols.join(",");
        s.push('\n');
        for r in &self.rows {
            s.push_str(&r.join(","));
            s.push('\n');
        }
        s
    }

    fn col(&self, name: &str) -> Result<usize, String> {
        self.cols.iter().position(|c| c == name).ok_or_else(|| {
            format!(
                "unknown column `{name}`。使える列: {}",
                self.cols.join(", ")
            )
        })
    }
}

pub(crate) type Tables = Arc<BTreeMap<String, Table>>;

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => a.cmp(b),
    }
}

fn matches(cell: &str, op: &str, value: &str) -> Result<bool, String> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    Ok(match op {
        "=" | "==" => compare(cell, value) == Equal,
        "!=" => compare(cell, value) != Equal,
        ">" => compare(cell, value) == Greater,
        ">=" => compare(cell, value) != Less,
        "<" => compare(cell, value) == Less,
        "<=" => compare(cell, value) != Greater,
        "contains" => cell.contains(value),
        "starts_with" => cell.starts_with(value),
        other => {
            return Err(format!(
                "unknown op `{other}`（=, !=, >, >=, <, <=, contains, starts_with）"
            ))
        }
    })
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn fmt_num(x: f64) -> String {
    if (x - x.round()).abs() < 1e-9 {
        format!("{}", x.round() as i64)
    } else {
        format!("{x:.2}")
    }
}

/// 結合して 1 枚の表にする（右表の列は `<表名>.<列>`）。
fn joined(tables: &Tables, base: &Table, j: &Value) -> Result<Table, String> {
    let file = arg_str(j, "file")?;
    let right = tables
        .get(file)
        .ok_or_else(|| format!("unknown file `{file}`"))?;
    let lo = base.col(arg_str(j, "left_on")?)?;
    let ro = right.col(arg_str(j, "right_on")?)?;
    let how = j.get("how").and_then(Value::as_str).unwrap_or("inner");
    if how != "inner" && how != "left" {
        return Err("join.how は inner か left".into());
    }
    let stem = file.trim_end_matches(".csv");
    let mut cols = base.cols.clone();
    cols.extend(right.cols.iter().map(|c| format!("{stem}.{c}")));
    let mut index: HashMap<&str, Vec<&Vec<String>>> = HashMap::new();
    for r in &right.rows {
        index.entry(r[ro].as_str()).or_default().push(r);
    }
    let mut rows = Vec::new();
    for l in &base.rows {
        match index.get(l[lo].as_str()) {
            Some(rs) => {
                for r in rs {
                    let mut row = l.clone();
                    row.extend(r.iter().cloned());
                    rows.push(row);
                }
            }
            None if how == "left" => {
                let mut row = l.clone();
                row.extend(std::iter::repeat_n(String::new(), right.cols.len()));
                rows.push(row);
            }
            None => {}
        }
    }
    Ok(Table { cols, rows })
}

/// `query_csv` の本体。
pub(crate) fn query(tables: &Tables, v: &Value) -> Result<String, String> {
    let file = arg_str(v, "file")?;
    let base = tables.get(file).ok_or_else(|| {
        format!(
            "unknown file `{file}`。ある表: {}",
            tables.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;
    let mut t = match v.get("join") {
        Some(j) if !j.is_null() => joined(tables, base, j)?,
        _ => base.clone(),
    };
    if let Some(filters) = v.get("filter").and_then(Value::as_array) {
        for f in filters {
            let c = t.col(arg_str(f, "column")?)?;
            let op = arg_str(f, "op")?.to_string();
            let val = value_str(f.get("value").unwrap_or(&Value::Null));
            let mut err = None;
            t.rows.retain(|r| match matches(&r[c], &op, &val) {
                Ok(b) => b,
                Err(e) => {
                    err = Some(e);
                    false
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
        }
    }
    let group_by: Vec<String> = v
        .get("group_by")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let aggs = v
        .get("aggregate")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = if !aggs.is_empty() || !group_by.is_empty() {
        aggregate(&t, &group_by, &aggs)?
    } else if let Some(sel) = v.get("select").and_then(Value::as_array) {
        let idx: Vec<usize> = sel
            .iter()
            .map(|s| t.col(s.as_str().unwrap_or("")))
            .collect::<Result<_, _>>()?;
        Table {
            cols: idx.iter().map(|&i| t.cols[i].clone()).collect(),
            rows: t
                .rows
                .iter()
                .map(|r| idx.iter().map(|&i| r[i].clone()).collect())
                .collect(),
        }
    } else {
        t
    };
    if let Some(sorts) = v.get("sort").and_then(Value::as_array) {
        let keys: Vec<(usize, bool)> = sorts
            .iter()
            .map(|s| {
                Ok((
                    out.col(arg_str(s, "column")?)?,
                    s.get("desc").and_then(Value::as_bool).unwrap_or(false),
                ))
            })
            .collect::<Result<_, String>>()?;
        out.rows.sort_by(|a, b| {
            for &(c, desc) in &keys {
                let o = compare(&a[c], &b[c]);
                let o = if desc { o.reverse() } else { o };
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        });
    }
    let limit = v
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(50, |l| l as usize)
        .clamp(1, 200);
    let total = out.rows.len();
    let mut s = out.cols.join(",");
    s.push('\n');
    for r in out.rows.iter().take(limit) {
        s.push_str(&r.join(","));
        s.push('\n');
    }
    let _ = write!(s, "（{total} 行中 {} 行を表示）", total.min(limit));
    Ok(s)
}

fn aggregate(t: &Table, group_by: &[String], aggs: &[Value]) -> Result<Table, String> {
    let gi: Vec<usize> = group_by
        .iter()
        .map(|g| t.col(g))
        .collect::<Result<_, _>>()?;
    let mut specs = Vec::new();
    for a in aggs {
        let f = arg_str(a, "fn")?.to_string();
        let c = match a.get("column").and_then(Value::as_str) {
            Some(c) => Some(t.col(c)?),
            None if f == "count" => None,
            None => return Err(format!("aggregate `{f}` には column が要ります")),
        };
        if !["sum", "count", "avg", "min", "max", "count_distinct"].contains(&f.as_str()) {
            return Err(format!(
                "unknown aggregate `{f}`（sum, count, avg, min, max, count_distinct）"
            ));
        }
        let name = a.get("as").and_then(Value::as_str).map_or_else(
            || match c {
                Some(c) => format!("{f}_{}", t.cols[c]),
                None => f.clone(),
            },
            String::from,
        );
        specs.push((f, c, name));
    }
    let mut groups: BTreeMap<Vec<String>, Vec<&Vec<String>>> = BTreeMap::new();
    for r in &t.rows {
        groups
            .entry(gi.iter().map(|&i| r[i].clone()).collect())
            .or_default()
            .push(r);
    }
    let mut cols: Vec<String> = group_by.to_vec();
    cols.extend(specs.iter().map(|s| s.2.clone()));
    let mut rows = Vec::new();
    for (key, rs) in groups {
        let mut row = key;
        for (f, c, _) in &specs {
            let vals: Vec<f64> =
                c.map_or_else(Vec::new, |c| rs.iter().filter_map(|r| num(&r[c])).collect());
            let cell = match f.as_str() {
                "count" => rs.len().to_string(),
                "count_distinct" => {
                    let c = c.unwrap_or(0);
                    let set: std::collections::HashSet<&str> =
                        rs.iter().map(|r| r[c].as_str()).collect();
                    set.len().to_string()
                }
                "sum" => fmt_num(vals.iter().sum()),
                "avg" if vals.is_empty() => String::new(),
                "avg" => fmt_num(vals.iter().sum::<f64>() / vals.len() as f64),
                "min" => vals
                    .iter()
                    .copied()
                    .reduce(f64::min)
                    .map(fmt_num)
                    .unwrap_or_default(),
                _ => vals
                    .iter()
                    .copied()
                    .reduce(f64::max)
                    .map(fmt_num)
                    .unwrap_or_default(),
            };
            row.push(cell);
        }
        rows.push(row);
    }
    Ok(Table { cols, rows })
}

/// データ分析のツール一式（表の説明・クエリ）。
pub(crate) fn tools(tables: &Tables) -> Vec<Arc<dyn Tool>> {
    let t = tables.clone();
    let q = tables.clone();
    vec![
        FnTool::new(
            "describe_csv",
            "CSV ファイルの列名・行数・先頭 3 行を返す。file を省略すると全ファイルの一覧。",
            json!({"type": "object", "properties": {"file": {"type": "string"}}}),
            true,
            move |v| {
                let Some(file) = v.get("file").and_then(Value::as_str) else {
                    return Ok(t
                        .iter()
                        .map(|(k, tb)| format!("{k}: {} 行・列 {}", tb.rows.len(), tb.cols.join(", ")))
                        .collect::<Vec<_>>()
                        .join("\n"));
                };
                let tb = t.get(file).ok_or_else(|| format!("unknown file `{file}`"))?;
                let mut s = format!("{file}: {} 行\n{}\n", tb.rows.len(), tb.cols.join(","));
                for r in tb.rows.iter().take(3) {
                    s.push_str(&r.join(","));
                    s.push('\n');
                }
                Ok(s)
            },
        ),
        FnTool::new(
            "query_csv",
            "CSV に問い合わせる（SQL ではなく JSON で指定）。順に join → filter → group_by/aggregate（または select）→ sort → limit を適用する。\
             filter: [{column, op(=,!=,>,>=,<,<=,contains,starts_with), value}]（すべて AND・数値は数値として比べる）。\
             join: {file, left_on, right_on, how(inner|left)}（右表の列は `<ファイル名から.csvを除いた名前>.<列>` で参照・left で一致しない行の右表の列は空文字）。\
             aggregate: [{fn(sum|count|avg|min|max|count_distinct), column, as}]。sort: [{column, desc}]。limit は既定 50・最大 200。",
            json!({"type": "object", "properties": {
                "file": {"type": "string"},
                "join": {"type": "object", "properties": {
                    "file": {"type": "string"}, "left_on": {"type": "string"},
                    "right_on": {"type": "string"}, "how": {"type": "string", "enum": ["inner", "left"]}}},
                "filter": {"type": "array", "items": {"type": "object", "properties": {
                    "column": {"type": "string"}, "op": {"type": "string"}, "value": {}}, "required": ["column", "op", "value"]}},
                "group_by": {"type": "array", "items": {"type": "string"}},
                "aggregate": {"type": "array", "items": {"type": "object", "properties": {
                    "fn": {"type": "string"}, "column": {"type": "string"}, "as": {"type": "string"}}, "required": ["fn"]}},
                "select": {"type": "array", "items": {"type": "string"}},
                "sort": {"type": "array", "items": {"type": "object", "properties": {
                    "column": {"type": "string"}, "desc": {"type": "boolean"}}, "required": ["column"]}},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200}
            }, "required": ["file"]}),
            true,
            move |v| query(&q, v),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> Tables {
        let mut o = Table::new(&["order_id", "cust", "amount"]);
        o.rows = vec![
            vec!["1".into(), "a".into(), "100".into()],
            vec!["2".into(), "a".into(), "50".into()],
            vec!["3".into(), "b".into(), "70".into()],
        ];
        let mut c = Table::new(&["id", "seg"]);
        c.rows = vec![
            vec!["a".into(), "法人".into()],
            vec!["b".into(), "個人".into()],
            vec!["c".into(), "個人".into()],
        ];
        Arc::new(BTreeMap::from([
            ("orders.csv".into(), o),
            ("customers.csv".into(), c),
        ]))
    }

    #[test]
    fn join_group_and_sort() {
        let out = query(
            &tables(),
            &json!({"file": "orders.csv", "join": {"file": "customers.csv", "left_on": "cust", "right_on": "id"},
                    "group_by": ["customers.seg"], "aggregate": [{"fn": "sum", "column": "amount", "as": "total"}],
                    "sort": [{"column": "total", "desc": true}]}),
        )
        .unwrap();
        assert!(
            out.starts_with("customers.seg,total\n法人,150\n個人,70\n"),
            "{out}"
        );
        let anti = query(
            &tables(),
            &json!({"file": "customers.csv", "join": {"file": "orders.csv", "left_on": "id", "right_on": "cust", "how": "left"},
                    "filter": [{"column": "orders.order_id", "op": "=", "value": ""}], "select": ["id"]}),
        )
        .unwrap();
        assert!(anti.starts_with("id\nc\n"), "{anti}");
    }
}
