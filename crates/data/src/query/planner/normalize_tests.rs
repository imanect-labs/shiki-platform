//! 正規化の差分テスト（query-optimizer.md §9.2「正規化の 1 規則ずつ」・TLP）。
//!
//! 乱数で生成した式とレコード（値なし・多値・同値の偏りを含む）について、規則を 1 つずつ適用した前後で
//! 2 値論理の評価結果が一致することを確かめる。
#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    clippy::needless_pass_by_value,
    clippy::match_same_arms
)]

use super::*;
use crate::numeric::Dec;
use crate::query::bound::{FieldKind, Leaf};
use crate::query::eval::{eval, Rec};

/// 決定的な擬似乱数（xorshift64）。
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const SINGLE: FieldId = 16;
const MULTI: FieldId = 17;
const NUM: FieldId = 18;

fn fields() -> BTreeMap<FieldId, BField> {
    let mk = |id, kind| BField {
        id,
        key: format!("f{id}"),
        ty: None,
        kind,
        searchable: true,
        ref_table: None,
        derived: false,
    };
    [
        (SINGLE, mk(SINGLE, FieldKind::Text)),
        (MULTI, mk(MULTI, FieldKind::Multi)),
        (NUM, mk(NUM, FieldKind::Num)),
    ]
    .into()
}

const WORDS: [&str; 5] = ["a", "ab", "b", "ba", "\u{10FFFF}"];

fn text(r: &mut Rng) -> Val {
    Val::Text(WORDS[r.below(WORDS.len() as u64) as usize].to_string())
}

fn num(r: &mut Rng) -> Val {
    Val::Num(Dec::parse(&(r.below(5) as i64 - 2).to_string()).unwrap())
}

fn leaf(r: &mut Rng, field: FieldId) -> Leaf {
    let v = |r: &mut Rng| if field == NUM { num(r) } else { text(r) };
    match r.below(if field == MULTI { 6 } else { 7 }) {
        0 => Leaf::Eq(v(r)),
        1 => Leaf::In((0..r.below(3)).map(|_| v(r)).collect()),
        2 => Leaf::Exists,
        3 if field != NUM => Leaf::StartsWith(WORDS[r.below(3) as usize].to_string()),
        4 if field != NUM => Leaf::Contains("a".into()),
        5 if field != NUM => Leaf::EndsWith("b".into()),
        6 => Leaf::Range {
            lo: (r.below(2) == 0).then(|| (v(r), r.below(2) == 0)),
            hi: (r.below(2) == 0).then(|| (v(r), r.below(2) == 0)),
        },
        _ => Leaf::Eq(v(r)),
    }
}

fn expr(r: &mut Rng, depth: u32) -> Expr {
    let f = [SINGLE, MULTI, NUM][r.below(3) as usize];
    if depth == 0 {
        return Expr::atom(f, leaf(r, f));
    }
    match r.below(6) {
        0 => Expr::Not(Box::new(expr(r, depth - 1))),
        1 => Expr::And((0..r.below(4)).map(|_| expr(r, depth - 1)).collect()),
        2 => Expr::Or((0..r.below(4)).map(|_| expr(r, depth - 1)).collect()),
        3 => [Expr::True, Expr::False][r.below(2) as usize].clone(),
        _ => Expr::atom(f, leaf(r, f)),
    }
}

fn record(r: &mut Rng) -> Rec {
    let mut rec = Rec::new();
    if r.below(3) != 0 {
        rec.insert(SINGLE, vec![text(r)]);
    }
    let n = r.below(3);
    rec.insert(MULTI, (0..n).map(|_| text(r)).collect());
    if r.below(3) != 0 {
        rec.insert(NUM, vec![num(r)]);
    }
    rec
}

#[test]
fn each_rule_preserves_meaning() {
    let f = fields();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let recs: Vec<Rec> = (0..60).map(|_| record(&mut rng)).collect();
    for _ in 0..1_500 {
        let e = expr(&mut rng, 3);
        for rule in [
            Rules::NNF,
            Rules::FLATTEN,
            Rules::FOLD,
            Rules::MERGE,
            Rules::PREFIX,
            Rules::ALL.0,
        ] {
            let n = normalize(e.clone(), &f, Rules(rule));
            for rec in &recs {
                assert_eq!(
                    eval(&e, rec),
                    eval(&n, rec),
                    "rule {rule}: {e:?} → {n:?} on {rec:?}"
                );
            }
        }
    }
}

#[test]
fn tlp_partitions_every_query() {
    // Q = (Q ∧ p) ∪ (Q ∧ ¬p)、両者は交わらない（2 値論理なので区分は 2 つ）。
    let f = fields();
    let mut rng = Rng(42);
    let recs: Vec<Rec> = (0..60).map(|_| record(&mut rng)).collect();
    for _ in 0..500 {
        let q = expr(&mut rng, 2);
        let p = expr(&mut rng, 1);
        let qp = normalize(Expr::And(vec![q.clone(), p.clone()]), &f, Rules::ALL);
        let qn = normalize(
            Expr::And(vec![q.clone(), Expr::Not(Box::new(p))]),
            &f,
            Rules::ALL,
        );
        for rec in &recs {
            let (a, b) = (eval(&qp, rec), eval(&qn, rec));
            assert_eq!(eval(&q, rec), a || b);
            assert!(!(a && b));
        }
    }
}

#[test]
fn semantics_table_holds() {
    let f = fields();
    let missing = Rec::new();
    let a = Val::Text("a".into());
    // 値なしの neq は真・is_null は真・比較の否定は値なしを含む。
    let neq = Expr::Not(Box::new(Expr::atom(SINGLE, Leaf::Eq(a.clone()))));
    assert!(eval(&neq, &missing));
    let lt = Expr::atom(
        NUM,
        Leaf::Range {
            lo: None,
            hi: Some((Val::Num(Dec::parse("0").unwrap()), false)),
        },
    );
    assert!(eval(&Expr::Not(Box::new(lt.clone())), &missing));
    assert!(!eval(&lt, &missing));
    // 多値の And[Eq a, Eq b] は矛盾ではない（両方を含む）ので False に畳まない。
    let both = Expr::And(vec![
        Expr::atom(MULTI, Leaf::Eq(a.clone())),
        Expr::atom(MULTI, Leaf::Eq(Val::Text("b".into()))),
    ]);
    assert_ne!(normalize(both.clone(), &f, Rules::ALL), Expr::False);
    // 単値の And[Eq a, Eq b] は False。
    let single = Expr::And(vec![
        Expr::atom(SINGLE, Leaf::Eq(a.clone())),
        Expr::atom(SINGLE, Leaf::Eq(Val::Text("b".into()))),
    ]);
    assert_eq!(normalize(single, &f, Rules::ALL), Expr::False);
    // Or[Eq a, Eq b] → In[a, b]。
    let or = Expr::Or(vec![
        Expr::atom(SINGLE, Leaf::Eq(a.clone())),
        Expr::atom(SINGLE, Leaf::Eq(Val::Text("b".into()))),
    ]);
    assert_eq!(
        normalize(or, &f, Rules::ALL),
        Expr::atom(SINGLE, Leaf::In(vec![a, Val::Text("b".into())]))
    );
}

#[test]
fn succ_is_valid_utf8_and_skips_surrogates() {
    assert_eq!(succ("ab").as_deref(), Some("ac"));
    assert_eq!(succ("a\u{D7FF}").as_deref(), Some("a\u{E000}"));
    assert_eq!(succ("a\u{10FFFF}").as_deref(), Some("b"));
    assert_eq!(succ("\u{10FFFF}"), None);
    let next = char::from_u32(u32::from('認') + 1).unwrap();
    assert_eq!(succ("承認"), Some(format!("承{next}")));
}
