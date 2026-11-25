//! ランダムな表と単純な SQL の生成（種から決定的に作る）。
//!
//! 値は小さな集合から選び、同じ値・値なし・境界（空文字・`%` `_` を含む文字列・負数・小数）が
//! よく出るようにする。食い違いは「似た値どうしの比較」や「値なし」で起きやすいため。

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::Rng;

use super::allow::MAX_LIMIT;
use super::model::{Agg, Case, Col, Func, Op, Pred, Query, Ty, Val};

const TEXTS: &[&str] = &[
    "a", "b", "ab", "ba", "A", "abc", "a%b", "a_b", "x y", "日本", "",
];
const NUMBERS: &[&str] = &[
    "-2", "-1", "0", "1", "2", "3", "0.5", "-0.5", "2.25", "10", "100", "0.1",
];
const DATES: &[&str] = &[
    "2025-12-31",
    "2026-01-01",
    "2026-01-02",
    "2026-01-03",
    "2026-02-01",
];
/// 単一選択の選択肢（表の定義にも使う）。
pub const OPTIONS: &[&str] = &["red", "green", "blue"];
const PATTERNS: &[&str] = &[
    "a%", "%b", "%a%", "ab", "a_", "_b", "%", "a%b", "A%", "%_%", "",
];

fn pool(ty: Ty) -> &'static [&'static str] {
    match ty {
        Ty::Text => TEXTS,
        Ty::Number => NUMBERS,
        Ty::Date => DATES,
        Ty::Select => OPTIONS,
    }
}

fn val(ty: Ty, s: &str) -> Val {
    if ty.numeric() {
        Val::Num(s.into())
    } else {
        Val::Str(s.into())
    }
}

/// 列の型に合う値（たまに集合の外の値）。
fn pick(rng: &mut StdRng, ty: Ty) -> Val {
    if rng.gen_bool(0.1) {
        let outside: &[&str] = match ty {
            Ty::Number => &["1.5", "-10", "1000"],
            Ty::Date => &["2024-01-01", "2030-01-01"],
            Ty::Text | Ty::Select => &["zz", "purple"],
        };
        return val(ty, outside.choose(rng).unwrap());
    }
    val(ty, pool(ty).choose(rng).unwrap())
}

pub fn case(rng: &mut StdRng) -> Case {
    let mut cols = vec![Col {
        name: "k".into(),
        ty: Ty::Number,
        indexed: true,
        searchable: false,
    }];
    for i in 0..rng.gen_range(2..=4) {
        let ty = *[Ty::Text, Ty::Number, Ty::Date, Ty::Select]
            .choose(rng)
            .unwrap();
        cols.push(Col {
            name: format!("c{i}"),
            ty,
            // 索引の無い列での絞り込みは 400（`field-not-indexed`）なので、比べられる件数のために多めに張る。
            indexed: rng.gen_bool(0.85),
            // 部分一致・後方一致の LIKE を比べるため、テキストの列には半分ほど付ける。
            searchable: ty == Ty::Text && rng.gen_bool(0.5),
        });
    }
    // ときどき既定のページ（`DEFAULT_LIMIT`）を超える行数にして、カーソルでのページ送りも比べる。
    let n = if rng.gen_bool(0.15) {
        rng.gen_range(60..=150)
    } else {
        rng.gen_range(0..=25)
    };
    let null_p = rng.gen_range(0.0..0.5);
    let rows = (1..=n)
        .map(|k| {
            cols.iter()
                .map(|c| {
                    if c.name == "k" {
                        Some(Val::Num(k.to_string()))
                    } else if rng.gen_bool(null_p) {
                        None
                    } else {
                        Some(val(c.ty, pool(c.ty).choose(rng).unwrap()))
                    }
                })
                .collect()
        })
        .collect();
    let mut c = Case {
        cols,
        rows,
        query: Query::Rows {
            cols: vec!["k".into()],
            filter: None,
            order: vec![],
            limit: None,
        },
    };
    c.query = query(rng, &c);
    c
}

/// 同じ表に対する別の問い合わせ。
pub fn query(rng: &mut StdRng, c: &Case) -> Query {
    let filter = rng.gen_bool(0.8).then(|| pred(rng, c, 3));
    if rng.gen_bool(0.6) {
        let mut cols: Vec<String> = c.cols.iter().map(|c| c.name.clone()).collect();
        cols.shuffle(rng);
        cols.truncate(rng.gen_range(1..=cols.len().min(3)));
        let others: Vec<&Col> = c.cols.iter().filter(|c| c.name != "k").collect();
        let order = match rng.gen_range(0..3) {
            0 => vec![],
            1 => vec![("k".into(), rng.gen_bool(0.5))],
            _ => vec![
                (others.choose(rng).unwrap().name.clone(), rng.gen_bool(0.5)),
                ("k".into(), rng.gen_bool(0.5)),
            ],
        };
        let limit = (!order.is_empty() && rng.gen_bool(0.5)).then(|| {
            if rng.gen_bool(0.2) {
                rng.gen_range(40..=MAX_LIMIT)
            } else {
                rng.gen_range(0..=12)
            }
        });
        Query::Rows {
            cols,
            filter,
            order,
            limit,
        }
    } else {
        let others: Vec<&Col> = c.cols.iter().filter(|c| c.name != "k").collect();
        let group = rng
            .gen_bool(0.5)
            .then(|| others.choose(rng).unwrap().name.clone());
        let nums: Vec<&Col> = c.cols.iter().filter(|c| c.ty.numeric()).collect();
        let aggs = (0..rng.gen_range(1..=3))
            .map(|_| {
                let func = *[Func::Count, Func::Sum, Func::Avg, Func::Min, Func::Max]
                    .choose(rng)
                    .unwrap();
                let arg = match func {
                    Func::Count if rng.gen_bool(0.5) => None,
                    Func::Sum | Func::Avg => Some(nums.choose(rng).unwrap().name.clone()),
                    Func::Count | Func::Min | Func::Max => {
                        Some(c.cols.choose(rng).unwrap().name.clone())
                    }
                };
                Agg { func, arg }
            })
            .collect();
        let having = (group.is_some() && rng.gen_bool(0.3))
            .then(|| (*Op::ALL.choose(rng).unwrap(), rng.gen_range(0..4)));
        Query::Agg {
            group,
            aggs,
            filter,
            having,
        }
    }
}

/// 比較値（6 割は表に実際にある値・境界の `=` の行を踏ませるため）。
fn operand(rng: &mut StdRng, c: &Case, col: &Col) -> Val {
    let i = c.cols.iter().position(|x| x.name == col.name).unwrap_or(0);
    let present: Vec<&Val> = c.rows.iter().filter_map(|r| r[i].as_ref()).collect();
    if !present.is_empty() && rng.gen_bool(0.6) {
        return (*present.choose(rng).unwrap()).clone();
    }
    pick(rng, col.ty)
}

fn pred(rng: &mut StdRng, c: &Case, depth: u32) -> Pred {
    let leaf = depth == 0 || rng.gen_bool(0.45);
    if leaf {
        let p = leaf_pred(rng, c);
        // 否定は 3 値論理の F 側（`NOT (c < v)` = `c >= v`）を通すので、葉でもよく作る。
        return if rng.gen_bool(0.25) {
            Pred::Not(Box::new(p))
        } else {
            p
        };
    }
    inner_pred(rng, c, depth)
}

fn leaf_pred(rng: &mut StdRng, c: &Case) -> Pred {
    {
        let col = c.cols.choose(rng).unwrap();
        let name = col.name.clone();
        match rng.gen_range(0..10) {
            0..=3 => Pred::Cmp(name, *Op::ALL.choose(rng).unwrap(), operand(rng, c, col)),
            4 => Pred::IsNull(name, rng.gen_bool(0.5)),
            5 => {
                let n = rng.gen_range(1..=3);
                Pred::In(
                    name,
                    (0..n).map(|_| operand(rng, c, col)).collect(),
                    rng.gen_bool(0.3),
                )
            }
            6 => Pred::Between(
                name,
                operand(rng, c, col),
                operand(rng, c, col),
                rng.gen_bool(0.3),
            ),
            7 if !col.ty.numeric() => Pred::Like(
                name,
                (*PATTERNS.choose(rng).unwrap()).into(),
                rng.gen_bool(0.3),
                rng.gen_bool(0.2),
            ),
            8 => Pred::LitCmp(
                rng.gen_range(3..6),
                *Op::ALL.choose(rng).unwrap(),
                rng.gen_range(3..6),
            ),
            _ => Pred::Const(rng.gen_bool(0.5)),
        }
    }
}

fn inner_pred(rng: &mut StdRng, c: &Case, depth: u32) -> Pred {
    let a = Box::new(pred(rng, c, depth - 1));
    match rng.gen_range(0..6) {
        0 | 1 => Pred::And(a, Box::new(pred(rng, c, depth - 1))),
        2 | 3 => Pred::Or(a, Box::new(pred(rng, c, depth - 1))),
        4 => Pred::Not(a),
        _ if rng.gen_bool(0.5) => Pred::Unknown(a, rng.gen_bool(0.5)),
        _ => Pred::Truth(a, rng.gen_bool(0.5), rng.gen_bool(0.5)),
    }
}
