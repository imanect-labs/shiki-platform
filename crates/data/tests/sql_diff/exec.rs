//! 1 件を両側で実行して比べる。
//!
//! - Shiki: テナントごとに表を作り、行を 1 件ずつ作成し、[`data::DataStore::run_sql`] で読む
//!   （/data の SQL 画面と chat の `data_query` が通る同じ入口）。読取の経路・強制戦略を変えた複数の
//!   ストアで同じ SQL を流す。
//! - 参照: 同じ DB の `sqldiff` スキーマに素の表を作り、同じ SQL（表名と [`super::allow`] の書き換えだけ違う）を流す。

use std::sync::Arc;

use data::{
    DataError, DataStore, FieldDef, FieldType, ForcedStrategy, QueryTuning, ReadPath, SqlRequest,
};
use serde_json::{Map, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::allow::rejection_allowed;
use super::model::{Case, Pred, Query, Side, Ty, Val};

/// 比べる Shiki 側の構成（読取の経路と強制戦略）。
pub struct Variant {
    pub name: &'static str,
    pub store: DataStore,
}

/// 既定・本体経路・各強制戦略（CI の `data-strategies` ジョブと同じ並び）。
pub fn variants(pool: &PgPool) -> Vec<Variant> {
    let mk = |name: &'static str, tuning: QueryTuning| Variant {
        name,
        store: DataStore::new(
            pool.clone(),
            Arc::new(crate::common::AllowAll),
            Arc::new(crate::common::FixedResolver),
        )
        .with_query_tuning(tuning),
    };
    let forced = |s: &str| QueryTuning {
        force: ForcedStrategy::parse(s),
        ..QueryTuning::default()
    };
    vec![
        mk("index", QueryTuning::default()),
        mk(
            "body",
            QueryTuning {
                read_path: ReadPath::Body,
                ..QueryTuning::default()
            },
        ),
        mk("sort", forced("sort")),
        mk("filter", forced("filter")),
        mk("union", forced("union")),
        mk("intersect", forced("intersect")),
        mk("batch", forced("batch")),
    ]
}

/// 実行の結果（セルは正規化前の文字列・値なしは `None`）。
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Rows {
        rows: Vec<Vec<Option<String>>>,
        suppressed: bool,
    },
    Rejected(String),
    Error(String),
}

/// 比べた結果。
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Same,
    /// 許容リストの理由で比べなかった。
    Skipped(String),
    /// 食い違い（どの構成で・何が）。
    Differ {
        variant: String,
        detail: String,
    },
}

fn field(c: &super::model::Col) -> FieldDef {
    let ty = match c.ty {
        Ty::Text => FieldType::Text,
        Ty::Number => FieldType::Number,
        Ty::Date => FieldType::Date,
        Ty::Select => FieldType::Select,
    };
    let mut f = FieldDef::new(&c.name, ty);
    f.indexed = c.indexed;
    if c.ty == Ty::Select {
        f.options = super::gen::OPTIONS.iter().map(|s| (*s).into()).collect();
    }
    f
}

fn json_val(v: &Val) -> Value {
    match v {
        Val::Str(s) => Value::String(s.clone()),
        Val::Num(n) => serde_json::from_str(n).unwrap_or_else(|_| Value::String(n.clone())),
    }
}

/// Shiki 側に表と行を作る（どの構成のストアからも読める・同じ DB）。表の id とテナントを返す。
pub async fn load_shiki(store: &DataStore, case: &Case) -> Result<(String, Uuid), String> {
    let tenant = crate::common::unique_tenant();
    let who = crate::common::ctx(&tenant, "u");
    let mut s = crate::common::schema(case.cols.iter().map(field).collect());
    // 0 行の集計以外は伏せない（`small-cell`）。
    s.aggregate_min_rows = Some(1);
    let t = store
        .create_table(
            &who,
            data::NewDataTable {
                name: "t".into(),
                schema: s,
            },
            None,
        )
        .await
        .map_err(|e| format!("表を作れない: {e:?}"))?;
    for r in &case.rows {
        let obj: Map<String, Value> = case
            .cols
            .iter()
            .zip(r)
            .filter_map(|(c, v)| v.as_ref().map(|v| (c.name.clone(), json_val(v))))
            .collect();
        store
            .create_record(&who, t.id, Value::Object(obj), None)
            .await
            .map_err(|e| format!("行を作れない: {e:?}"))?;
    }
    Ok((tenant, t.id))
}

fn cell(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

pub async fn run_shiki(store: &DataStore, tenant: &str, case: &Case) -> Outcome {
    let who = crate::common::ctx(tenant, "u");
    let sql = case.sql("t", Side::Shiki);
    // `LIMIT` は 1 ページの件数（`limit-is-page`）: 最初のページだけを比べる。
    let paged = !matches!(&case.query, Query::Rows { limit: Some(_), .. });
    let mut rows = Vec::new();
    let mut suppressed = false;
    let mut cursor = None;
    for _ in 0..100 {
        let r = match store
            .run_sql(
                &who,
                &SqlRequest {
                    sql: sql.clone(),
                    cursor: cursor.clone(),
                    with_count: false,
                },
                None,
            )
            .await
        {
            Ok(r) => r,
            Err(DataError::Rejected(r)) => return Outcome::Rejected(r.message.clone()),
            Err(e) => return Outcome::Error(format!("{e:?}")),
        };
        suppressed |= r.suppressed;
        rows.extend(r.rows.iter().map(|row| row.iter().map(cell).collect()));
        match r.next_cursor {
            Some(c) if paged => cursor = Some(c),
            _ => return Outcome::Rows { rows, suppressed },
        }
    }
    Outcome::Error("ページングが終わらない".into())
}

/// 参照側の表を作って SQL を流し、表を消す。
pub async fn run_pg(pool: &PgPool, case: &Case, sql_of: impl Fn(&str) -> String) -> Outcome {
    let table = format!("sqldiff.t_{}", Uuid::new_v4().simple());
    let mut conn = match pool.acquire().await {
        Ok(c) => c,
        Err(e) => return Outcome::Error(format!("{e}")),
    };
    let _ = sqlx::query("CREATE SCHEMA IF NOT EXISTS sqldiff")
        .execute(&mut *conn)
        .await;
    for st in case.pg_setup(&table) {
        if let Err(e) = sqlx::query(&st).execute(&mut *conn).await {
            let _ = sqlx::query(&format!("DROP TABLE IF EXISTS {table}"))
                .execute(&mut *conn)
                .await;
            return Outcome::Error(format!("参照表を作れない: {e}: {st}"));
        }
    }
    let out = match sqlx::query(&sql_of(&table)).fetch_all(&mut *conn).await {
        Ok(rows) => Outcome::Rows {
            rows: rows
                .iter()
                .map(|r| {
                    (0..r.len())
                        .map(|i| r.try_get::<Option<String>, _>(i).ok().flatten())
                        .collect()
                })
                .collect(),
            suppressed: false,
        },
        Err(e) => Outcome::Error(format!("{e}")),
    };
    let _ = sqlx::query(&format!("DROP TABLE IF EXISTS {table}"))
        .execute(&mut *conn)
        .await;
    out
}

/// 数値の 10 進表記をそろえる（`2.50` と `2.5`、`-0` と `0`、平均の桁）。
fn norm_num(s: &str) -> String {
    match s.parse::<f64>() {
        Ok(f) => {
            let r = (f * 1e9).round() / 1e9;
            let r = if r == 0.0 { 0.0 } else { r };
            format!("{r}")
        }
        Err(_) => s.to_string(),
    }
}

fn normalize(case: &Case, rows: &[Vec<Option<String>>]) -> Vec<Vec<Option<String>>> {
    let numeric = case.out_numeric();
    let mut out: Vec<Vec<Option<String>>> = rows
        .iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(i, v)| {
                    v.as_ref().map(|s| {
                        if numeric.get(i).copied().unwrap_or(false) {
                            norm_num(s)
                        } else {
                            s.clone()
                        }
                    })
                })
                .collect()
        })
        .collect();
    if !case.ordered() {
        out.sort();
    }
    out
}

/// 集計を伏せたのが「0 行の集計」だけか（`small-cell`・K = 1）。
async fn only_empty_suppressed(pool: &PgPool, case: &Case) -> bool {
    let Query::Agg {
        group: None,
        filter,
        ..
    } = &case.query
    else {
        return false;
    };
    let count_case = Case {
        query: Query::Agg {
            group: None,
            aggs: vec![super::model::Agg {
                func: super::model::Func::Count,
                arg: None,
            }],
            filter: filter.clone(),
            having: None,
        },
        ..case.clone()
    };
    matches!(
        run_pg(pool, &count_case, |t| count_case.sql(t, Side::Pg)).await,
        Outcome::Rows { rows, .. } if rows == vec![vec![Some("0".to_string())]]
    )
}

/// 1 件を全構成で比べる（`mutate` は参照側の SQL を壊す自己検査用）。
pub async fn check(
    pool: &PgPool,
    variants: &[Variant],
    case: &Case,
    mutate: Option<&dyn Fn(String) -> String>,
) -> Verdict {
    let expected = run_pg(pool, case, |t| {
        let s = case.sql(t, Side::Pg);
        mutate.map_or(s.clone(), |m| m(s))
    })
    .await;
    let expected = match expected {
        Outcome::Rows { rows, .. } => normalize(case, &rows),
        other => {
            return Verdict::Differ {
                variant: "参照".into(),
                detail: format!("参照側が実行できない: {other:?}"),
            }
        }
    };
    let (tenant, _) = match load_shiki(&variants[0].store, case).await {
        Ok(t) => t,
        Err(e) => {
            return Verdict::Differ {
                variant: "読込".into(),
                detail: e,
            }
        }
    };
    compare(pool, variants, &tenant, case, expected).await
}

/// 読み込み済みの Shiki の表（テナント `tenant` の `t`）で、同じ表の別の問い合わせを比べる。
pub async fn check_loaded(
    pool: &PgPool,
    variants: &[Variant],
    tenant: &str,
    case: &Case,
) -> Verdict {
    match run_pg(pool, case, |t| case.sql(t, Side::Pg)).await {
        Outcome::Rows { rows, .. } => {
            let expected = normalize(case, &rows);
            compare(pool, variants, tenant, case, expected).await
        }
        other => Verdict::Differ {
            variant: "参照".into(),
            detail: format!("参照側が実行できない: {other:?}"),
        },
    }
}

async fn compare(
    pool: &PgPool,
    variants: &[Variant],
    tenant: &str,
    case: &Case,
    expected: Vec<Vec<Option<String>>>,
) -> Verdict {
    for v in variants {
        match run_shiki(&v.store, tenant, case).await {
            Outcome::Rejected(msg) => match rejection_allowed(&msg) {
                Some(why) => return Verdict::Skipped(format!("rejected: {why}")),
                None => {
                    return Verdict::Differ {
                        variant: v.name.into(),
                        detail: format!("許容リストに無い拒否: {msg}"),
                    }
                }
            },
            Outcome::Error(e) => match rejection_allowed(&e) {
                Some(why) => return Verdict::Skipped(format!("refused: {why}")),
                None => {
                    return Verdict::Differ {
                        variant: v.name.into(),
                        detail: format!("エラー: {e}"),
                    }
                }
            },
            Outcome::Rows { rows, suppressed } => {
                if suppressed {
                    if only_empty_suppressed(pool, case).await {
                        return Verdict::Skipped("small-cell".into());
                    }
                    return Verdict::Differ {
                        variant: v.name.into(),
                        detail: "K = 1 なのに行のある集計を伏せた".into(),
                    };
                }
                let got = normalize(case, &rows);
                if got != expected {
                    return Verdict::Differ {
                        variant: v.name.into(),
                        detail: format!("Shiki: {got:?}\n参照: {expected:?}"),
                    };
                }
            }
        }
    }
    Verdict::Same
}

/// 条件式の部分木を列挙する（縮小で使う）。
pub fn children(p: &Pred) -> Vec<Pred> {
    match p {
        Pred::Not(x) | Pred::Unknown(x, _) | Pred::Truth(x, _, _) => vec![(**x).clone()],
        Pred::And(a, b) | Pred::Or(a, b) => vec![(**a).clone(), (**b).clone()],
        _ => vec![],
    }
}
