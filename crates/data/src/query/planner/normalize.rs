//! 正規化（論理書き換え・query-optimizer.md §3.2）。**どの規則も意味を変えない**（§9.2 で 1 規則ずつ検証）。
//!
//! 適用順:
//! 1. 否定の押し下げ（NNF）: `Not` を葉まで降ろす（2 値論理なので De Morgan がそのまま成り立つ）
//! 2. 平坦化・重複除去: `And[And[a,b],c] → And[a,b,c]`、同一の子を 1 つに
//! 3. 定数畳み込み: `And[.., False] → False`、`Or[.., True] → True`、空の `In` は False
//! 4. 同一フィールドの併合: `Or[Eq a, Eq b] → In[a,b]`。**単値**フィールドの `And` だけ
//!    `Range ∧ Range → 交差`・`Eq a ∧ Eq b (a≠b) → False`（多値では「両方を含む」が成立するので併合しない）
//! 5. `StartsWith p → Range[p, succ(p))`（`succ` は最後のコードポイントを 1 進める・サロゲート域は飛ばす）
//!
//! 行述語（`PolicyExpr`）にはここを通さない（§3.3: P には定数畳み込みと平坦化以外を掛けない）。

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::model::FieldId;
use crate::query::bound::{Atom, BField, Expr, Leaf, Val};

/// 適用する規則の集合（差分テストで 1 規則ずつ切り替える）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rules(pub u8);

impl Rules {
    pub(crate) const NNF: u8 = 1;
    pub(crate) const FLATTEN: u8 = 2;
    pub(crate) const FOLD: u8 = 4;
    pub(crate) const MERGE: u8 = 8;
    pub(crate) const PREFIX: u8 = 16;
    pub(crate) const ALL: Rules = Rules(31);

    fn has(self, r: u8) -> bool {
        self.0 & r != 0
    }
}

/// 正規化する。
pub(crate) fn normalize(e: Expr, fields: &BTreeMap<FieldId, BField>, rules: Rules) -> Expr {
    let mut e = e;
    if rules.has(Rules::NNF) {
        e = nnf(e, false);
    }
    if rules.has(Rules::FLATTEN) {
        e = flatten(e);
    }
    if rules.has(Rules::FOLD) {
        e = fold(e);
    }
    if rules.has(Rules::MERGE) {
        e = merge(e, fields);
    }
    if rules.has(Rules::PREFIX) {
        e = prefix(e);
    }
    if rules.has(Rules::FLATTEN) {
        e = flatten(e);
    }
    if rules.has(Rules::FOLD) {
        e = fold(e);
    }
    e
}

/// 規則 1: 否定の押し下げ。
pub(crate) fn nnf(e: Expr, neg: bool) -> Expr {
    match e {
        Expr::True => {
            if neg {
                Expr::False
            } else {
                Expr::True
            }
        }
        Expr::False => {
            if neg {
                Expr::True
            } else {
                Expr::False
            }
        }
        Expr::Atom(a) => {
            if neg {
                Expr::Not(Box::new(Expr::Atom(a)))
            } else {
                Expr::Atom(a)
            }
        }
        Expr::Not(inner) => nnf(*inner, !neg),
        Expr::And(v) => {
            let kids = v.into_iter().map(|c| nnf(c, neg)).collect();
            if neg {
                Expr::Or(kids)
            } else {
                Expr::And(kids)
            }
        }
        Expr::Or(v) => {
            let kids = v.into_iter().map(|c| nnf(c, neg)).collect();
            if neg {
                Expr::And(kids)
            } else {
                Expr::Or(kids)
            }
        }
    }
}

/// 規則 2: 平坦化と重複除去。
pub(crate) fn flatten(e: Expr) -> Expr {
    match e {
        Expr::And(v) => Expr::And(splice(v, true)),
        Expr::Or(v) => Expr::Or(splice(v, false)),
        Expr::Not(inner) => Expr::Not(Box::new(flatten(*inner))),
        other => other,
    }
}

fn splice(v: Vec<Expr>, and: bool) -> Vec<Expr> {
    let mut out: Vec<Expr> = Vec::with_capacity(v.len());
    for c in v {
        let c = flatten(c);
        let kids = match c {
            Expr::And(k) if and => k,
            Expr::Or(k) if !and => k,
            other => vec![other],
        };
        for k in kids {
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

/// 規則 3: 定数畳み込み。
pub(crate) fn fold(e: Expr) -> Expr {
    match e {
        Expr::Atom(Atom {
            leaf: Leaf::In(ref v),
            ..
        }) if v.is_empty() => Expr::False,
        Expr::Not(inner) => match fold(*inner) {
            Expr::True => Expr::False,
            Expr::False => Expr::True,
            Expr::Not(x) => *x,
            other => Expr::Not(Box::new(other)),
        },
        Expr::And(v) => {
            let mut out = Vec::with_capacity(v.len());
            for c in v {
                match fold(c) {
                    Expr::True => {}
                    Expr::False => return Expr::False,
                    other => out.push(other),
                }
            }
            match out.len() {
                0 => Expr::True,
                1 => out.pop().unwrap_or(Expr::True),
                _ => Expr::And(out),
            }
        }
        Expr::Or(v) => {
            let mut out = Vec::with_capacity(v.len());
            for c in v {
                match fold(c) {
                    Expr::False => {}
                    Expr::True => return Expr::True,
                    other => out.push(other),
                }
            }
            match out.len() {
                0 => Expr::False,
                1 => out.pop().unwrap_or(Expr::False),
                _ => Expr::Or(out),
            }
        }
        other => other,
    }
}

/// 値の大小（同じ型どうしのみ。text はバイト順 = `COLLATE "C"`）。
pub(crate) fn cmp_val(a: &Val, b: &Val) -> Option<Ordering> {
    match (a, b) {
        (Val::Text(x), Val::Text(y)) => Some(x.as_bytes().cmp(y.as_bytes())),
        (Val::Num(x), Val::Num(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

/// 規則 4: 同一フィールドの併合。
pub(crate) fn merge(e: Expr, fields: &BTreeMap<FieldId, BField>) -> Expr {
    match e {
        Expr::Not(inner) => Expr::Not(Box::new(merge(*inner, fields))),
        Expr::Or(v) => {
            let v: Vec<Expr> = v.into_iter().map(|c| merge(c, fields)).collect();
            merge_or(v)
        }
        Expr::And(v) => {
            let v: Vec<Expr> = v.into_iter().map(|c| merge(c, fields)).collect();
            merge_and(v, fields)
        }
        Expr::Atom(Atom {
            field,
            leaf: Leaf::In(mut vals),
        }) => {
            dedup_vals(&mut vals);
            if vals.len() == 1 {
                Expr::atom(field, Leaf::Eq(vals.remove(0)))
            } else {
                Expr::atom(field, Leaf::In(vals))
            }
        }
        other => other,
    }
}

fn dedup_vals(vals: &mut Vec<Val>) {
    let mut out: Vec<Val> = Vec::with_capacity(vals.len());
    for v in vals.drain(..) {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    *vals = out;
}

/// `Or` の中の同一フィールドの `Eq` / `In` を 1 つの `In` に（単値・多値とも「いずれかを含む」で同値）。
fn merge_or(v: Vec<Expr>) -> Expr {
    let mut out: Vec<Expr> = Vec::with_capacity(v.len());
    let mut slot: BTreeMap<FieldId, usize> = BTreeMap::new();
    for c in v {
        let (field, vals) = match c {
            Expr::Atom(Atom {
                field,
                leaf: Leaf::Eq(x),
            }) => (field, vec![x]),
            Expr::Atom(Atom {
                field,
                leaf: Leaf::In(xs),
            }) => (field, xs),
            other => {
                out.push(other);
                continue;
            }
        };
        if let Some(&i) = slot.get(&field) {
            if let Expr::Atom(Atom {
                leaf: Leaf::In(acc),
                ..
            }) = &mut out[i]
            {
                acc.extend(vals);
                continue;
            }
        }
        slot.insert(field, out.len());
        out.push(Expr::atom(field, Leaf::In(vals)));
    }
    let out = out
        .into_iter()
        .map(|c| match c {
            Expr::Atom(Atom {
                field,
                leaf: Leaf::In(mut vals),
            }) => {
                dedup_vals(&mut vals);
                if vals.len() == 1 {
                    Expr::atom(field, Leaf::Eq(vals.remove(0)))
                } else {
                    Expr::atom(field, Leaf::In(vals))
                }
            }
            other => other,
        })
        .collect();
    Expr::Or(out)
}

/// 単値フィールドの値集合の制約（`And` の併合用）。
#[derive(Debug, Clone)]
enum Constraint {
    /// 取り得る値の有限集合（Eq / In）。
    Set(Vec<Val>),
    Range {
        lo: crate::query::bound::Bound,
        hi: crate::query::bound::Bound,
    },
}

fn merge_and(v: Vec<Expr>, fields: &BTreeMap<FieldId, BField>) -> Expr {
    let mut out: Vec<Expr> = Vec::with_capacity(v.len());
    let mut slot: BTreeMap<FieldId, (usize, Constraint)> = BTreeMap::new();
    for c in v {
        let single = |f: &FieldId| fields.get(f).is_some_and(|b| !b.multi());
        let (field, cons) = match &c {
            Expr::Atom(Atom { field, leaf }) if single(field) => match leaf {
                Leaf::Eq(x) => (*field, Constraint::Set(vec![x.clone()])),
                Leaf::In(xs) => (*field, Constraint::Set(xs.clone())),
                Leaf::Range { lo, hi } => (
                    *field,
                    Constraint::Range {
                        lo: lo.clone(),
                        hi: hi.clone(),
                    },
                ),
                _ => {
                    out.push(c);
                    continue;
                }
            },
            _ => {
                out.push(c);
                continue;
            }
        };
        match slot.remove(&field) {
            None => {
                slot.insert(field, (out.len(), cons));
                out.push(Expr::True); // 後で差し替える場所
            }
            Some((i, prev)) => match intersect(prev, cons) {
                Some(merged) => {
                    slot.insert(field, (i, merged));
                }
                None => return Expr::False,
            },
        }
    }
    for (field, (i, cons)) in slot {
        out[i] = match cons {
            Constraint::Set(mut vals) => {
                dedup_vals(&mut vals);
                match vals.len() {
                    0 => Expr::False,
                    1 => Expr::atom(field, Leaf::Eq(vals.remove(0))),
                    _ => Expr::atom(field, Leaf::In(vals)),
                }
            }
            Constraint::Range { lo, hi } => Expr::atom(field, Leaf::Range { lo, hi }),
        };
    }
    fold(Expr::And(out))
}

/// 2 つの制約の積。空なら `None`（= False）。
#[allow(clippy::many_single_char_names)] // 制約の両辺（a/b・x/y）
fn intersect(a: Constraint, b: Constraint) -> Option<Constraint> {
    match (a, b) {
        (Constraint::Set(x), Constraint::Set(y)) => {
            let v: Vec<Val> = x.into_iter().filter(|v| y.contains(v)).collect();
            (!v.is_empty()).then_some(Constraint::Set(v))
        }
        (Constraint::Set(x), Constraint::Range { lo, hi })
        | (Constraint::Range { lo, hi }, Constraint::Set(x)) => {
            let v: Vec<Val> = x
                .into_iter()
                .filter(|v| in_range(v, lo.as_ref(), hi.as_ref()))
                .collect();
            (!v.is_empty()).then_some(Constraint::Set(v))
        }
        (Constraint::Range { lo: l1, hi: h1 }, Constraint::Range { lo: l2, hi: h2 }) => {
            let lo = tighter(l1, l2, true);
            let hi = tighter(h1, h2, false);
            if let (Some((l, li)), Some((h, hi_inc))) = (&lo, &hi) {
                match cmp_val(l, h) {
                    Some(Ordering::Greater) => return None,
                    Some(Ordering::Equal) if !(*li && *hi_inc) => return None,
                    _ => {}
                }
            }
            Some(Constraint::Range { lo, hi })
        }
    }
}

/// 値が範囲に入るか（型違いは「入らない」側に倒さず保守的に残す＝併合しない）。
pub(crate) fn in_range(v: &Val, lo: Option<&(Val, bool)>, hi: Option<&(Val, bool)>) -> bool {
    let ok_lo = lo.is_none_or(|(l, inc)| match cmp_val(v, l) {
        Some(Ordering::Equal) => *inc,
        Some(Ordering::Less) => false,
        Some(Ordering::Greater) | None => true,
    });
    let ok_hi = hi.is_none_or(|(h, inc)| match cmp_val(v, h) {
        Some(Ordering::Equal) => *inc,
        Some(Ordering::Greater) => false,
        Some(Ordering::Less) | None => true,
    });
    ok_lo && ok_hi
}

/// 下端（`lower = true`）は大きい方、上端は小さい方。同値なら排他を優先。
fn tighter(
    a: crate::query::bound::Bound,
    b: crate::query::bound::Bound,
    lower: bool,
) -> crate::query::bound::Bound {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some((va, ia)), Some((vb, ib))) => match cmp_val(&va, &vb) {
            Some(Ordering::Equal) => Some((va, ia && ib)),
            Some(Ordering::Greater) => {
                if lower {
                    Some((va, ia))
                } else {
                    Some((vb, ib))
                }
            }
            Some(Ordering::Less) => {
                if lower {
                    Some((vb, ib))
                } else {
                    Some((va, ia))
                }
            }
            None => Some((va, ia)),
        },
    }
}

/// 規則 5: 前方一致を範囲へ。
pub(crate) fn prefix(e: Expr) -> Expr {
    match e {
        Expr::Atom(Atom {
            field,
            leaf: Leaf::StartsWith(p),
        }) => {
            if p.is_empty() {
                return Expr::atom(field, Leaf::Exists);
            }
            let hi = succ(&p).map(|s| (Val::Text(s), false));
            Expr::atom(
                field,
                Leaf::Range {
                    lo: Some((Val::Text(p), true)),
                    hi,
                },
            )
        }
        Expr::Not(inner) => Expr::Not(Box::new(prefix(*inner))),
        Expr::And(v) => Expr::And(v.into_iter().map(prefix).collect()),
        Expr::Or(v) => Expr::Or(v.into_iter().map(prefix).collect()),
        other => other,
    }
}

/// p を前方一致する文字列の上限（排他）: 最後のコードポイントを 1 進めた文字列。
/// U+10FFFF なら落として 1 つ前を進める。サロゲート域（U+D800..U+DFFF）は飛ばす。
/// 全て U+10FFFF なら上限なし（`None`）。最後のバイトを増やす作り方は不正な UTF-8 を生むので使わない。
pub(crate) fn succ(p: &str) -> Option<String> {
    let mut chars: Vec<char> = p.chars().collect();
    while let Some(last) = chars.pop() {
        let mut next = u32::from(last) + 1;
        if (0xD800..=0xDFFF).contains(&next) {
            next = 0xE000;
        }
        if let Some(c) = char::from_u32(next) {
            chars.push(c);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
