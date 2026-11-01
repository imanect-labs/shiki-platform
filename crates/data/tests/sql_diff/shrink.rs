//! 食い違いを、同じ食い違いが残る最小の 1 件まで縮める。
//!
//! 行を半分ずつ・1 行ずつ減らし、条件式を部分木に置き換え、射影・集計・並べ替え・`LIMIT` を外し、
//! 問い合わせが使わない列を消す。候補を順に試し、食い違いが残る最初の候補へ進む（貪欲）。

use sqlx::PgPool;

use super::exec::{check, children, Variant, Verdict};
use super::model::{Case, Pred, Query};

/// 条件式を 1 段だけ簡単にした候補（部分木への置き換え・定数への置き換え）。
fn simpler(p: &Pred) -> Vec<Pred> {
    let mut out = children(p);
    if !matches!(p, Pred::Const(_)) {
        out.push(Pred::Const(true));
    }
    let wrap = |inner: Vec<Pred>, f: &dyn Fn(Pred) -> Pred| -> Vec<Pred> {
        inner.into_iter().map(f).collect()
    };
    match p {
        Pred::Not(x) => out.extend(wrap(simpler(x), &|y| Pred::Not(Box::new(y)))),
        Pred::Unknown(x, n) => out.extend(wrap(simpler(x), &|y| Pred::Unknown(Box::new(y), *n))),
        Pred::Truth(x, v, n) => {
            out.extend(wrap(simpler(x), &|y| Pred::Truth(Box::new(y), *v, *n)));
        }
        Pred::And(a, b) => {
            out.extend(wrap(simpler(a), &|y| Pred::And(Box::new(y), b.clone())));
            out.extend(wrap(simpler(b), &|y| Pred::And(a.clone(), Box::new(y))));
        }
        Pred::Or(a, b) => {
            out.extend(wrap(simpler(a), &|y| Pred::Or(Box::new(y), b.clone())));
            out.extend(wrap(simpler(b), &|y| Pred::Or(a.clone(), Box::new(y))));
        }
        Pred::In(c, vs, n) if vs.len() > 1 => {
            for i in 0..vs.len() {
                let mut v = vs.clone();
                v.remove(i);
                out.push(Pred::In(c.clone(), v, *n));
            }
        }
        _ => {}
    }
    out
}

fn query_candidates(q: &Query) -> Vec<Query> {
    let mut out = Vec::new();
    match q {
        Query::Rows {
            cols,
            filter,
            order,
            limit,
        } => {
            let with = |cols: Vec<String>,
                        filter: Option<Pred>,
                        order: Vec<(String, bool)>,
                        limit: Option<u32>| Query::Rows {
                cols,
                filter,
                order,
                limit,
            };
            if limit.is_some() {
                out.push(with(cols.clone(), filter.clone(), order.clone(), None));
            }
            if order.len() > 1 {
                out.push(with(
                    cols.clone(),
                    filter.clone(),
                    order[order.len() - 1..].to_vec(),
                    *limit,
                ));
            }
            if !order.is_empty() && limit.is_none() {
                out.push(with(cols.clone(), filter.clone(), vec![], None));
            }
            if cols.len() > 1 {
                for i in 0..cols.len() {
                    let mut c = cols.clone();
                    c.remove(i);
                    out.push(with(c, filter.clone(), order.clone(), *limit));
                }
            }
            if let Some(p) = filter {
                out.push(with(cols.clone(), None, order.clone(), *limit));
                for s in simpler(p) {
                    out.push(with(cols.clone(), Some(s), order.clone(), *limit));
                }
            }
        }
        Query::Agg {
            group,
            aggs,
            filter,
            having,
        } => {
            let with = |group: Option<String>, aggs, filter: Option<Pred>, having| Query::Agg {
                group,
                aggs,
                filter,
                having,
            };
            if having.is_some() {
                out.push(with(group.clone(), aggs.clone(), filter.clone(), None));
            }
            if group.is_some() && having.is_none() {
                out.push(with(None, aggs.clone(), filter.clone(), None));
            }
            if aggs.len() > 1 {
                for i in 0..aggs.len() {
                    let mut a = aggs.clone();
                    a.remove(i);
                    out.push(with(group.clone(), a, filter.clone(), *having));
                }
            }
            if let Some(p) = filter {
                out.push(with(group.clone(), aggs.clone(), None, *having));
                for s in simpler(p) {
                    out.push(with(group.clone(), aggs.clone(), Some(s), *having));
                }
            }
        }
    }
    out
}

/// 問い合わせが名指ししている列（`k` は常に残す）。
fn used_cols(c: &Case) -> Vec<String> {
    let s = format!("{:?}", c.query);
    c.cols
        .iter()
        .filter(|col| col.name == "k" || s.contains(&format!("\"{}\"", col.name)))
        .map(|col| col.name.clone())
        .collect()
}

fn candidates(c: &Case) -> Vec<Case> {
    let mut out = Vec::new();
    let n = c.rows.len();
    if n > 1 {
        out.push(Case {
            rows: c.rows[..n / 2].to_vec(),
            ..c.clone()
        });
        out.push(Case {
            rows: c.rows[n / 2..].to_vec(),
            ..c.clone()
        });
    }
    for i in 0..n {
        let mut rows = c.rows.clone();
        rows.remove(i);
        out.push(Case { rows, ..c.clone() });
    }
    for q in query_candidates(&c.query) {
        out.push(Case {
            query: q,
            ..c.clone()
        });
    }
    let keep = used_cols(c);
    if keep.len() < c.cols.len() {
        let idx: Vec<usize> = (0..c.cols.len())
            .filter(|i| keep.contains(&c.cols[*i].name))
            .collect();
        out.push(Case {
            cols: idx.iter().map(|i| c.cols[*i].clone()).collect(),
            rows: c
                .rows
                .iter()
                .map(|r| idx.iter().map(|i| r[*i].clone()).collect())
                .collect(),
            query: c.query.clone(),
        });
    }
    out
}

/// 食い違いが残る限り縮める（試す回数に上限）。縮めた 1 件と、その食い違いを返す。
pub async fn shrink(
    pool: &PgPool,
    variants: &[Variant],
    case: Case,
    first: Verdict,
    mutate: Option<&dyn Fn(String) -> String>,
) -> (Case, Verdict) {
    let mut cur = case;
    let mut verdict = first;
    let mut budget = 400;
    'outer: loop {
        for cand in candidates(&cur) {
            if budget == 0 {
                break 'outer;
            }
            budget -= 1;
            let v = check(pool, variants, &cand, mutate).await;
            let still = matches!(&v, Verdict::Differ { variant, .. } if variant != "参照" && variant != "読込");
            if still {
                cur = cand;
                verdict = v;
                continue 'outer;
            }
        }
        break;
    }
    (cur, verdict)
}
