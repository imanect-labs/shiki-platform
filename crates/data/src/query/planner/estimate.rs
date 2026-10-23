//! カーディナリティ推定（query-optimizer.md §4.4）。推定は**区間 [lo, hi] と信頼度**で持つ。
//!
//! | 述語 | 推定 | 信頼度 |
//! |---|---|---|
//! | `Eq c`（値頻度表あり） | `cnt(c) / n_rows` | 正確 |
//! | `Eq c`（最頻値に含まれる） | `freq(c)` | 統計 |
//! | `Eq c`（それ以外） | `(1 − Σ最頻値の頻度) / (異なり数 − 最頻値の数)` | 統計 |
//! | `In [..]` | 各要素の和（単値）・`1 − Π(1 − sᵢ)`（多値） | 要素の最弱 |
//! | `Range` / `StartsWith` | ヒストグラムの区間内を線形補間 | 統計 |
//! | `Contains` / `EndsWith` | 固定値 0.01（必要なら件数探索） | 推測 |
//! | `Exists` | 値を持つ行の割合 | 統計 |
//! | `Not X` | `1 − s(X)` | X に同じ |
//! | `All[..]` | **指数バックオフ** `s₁ · s₂^(1/2) · s₃^(1/4) · …`（小さい順） | 最弱 |
//! | `Any[..]` | `1 − Π(1 − sᵢ)` | 最弱 |
//! | `owner = me` 等の行述語の選言 | 値頻度表（owner 擬似列） | 正確 |
//! | 個別共有 `id ∈ S` | `|S| / n_rows` | 正確 |
//!
//! `All` を積にしないのは、業務データの条件が独立でないことの方が多いから（積は過小推定に倒れ、
//! 「ソート索引から入って延々と歩く」最悪側を招く）。

use std::collections::HashMap;

use super::catalog::{FieldStats, TableStats};
use super::guard::Driver;
use crate::index::IndexTable;
use crate::model::{FieldId, SYSTEM_FIELD_OWNER};
use crate::numeric::Dec;
use crate::query::bound::{Atom, BoundQuery, Expr, FieldKind, Leaf, Val};

/// 推定の信頼度（弱い順に大きい）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Conf {
    Exact,
    Stats,
    Guess,
}

/// 選択率の推定（0〜1）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Est {
    pub lo: f64,
    pub mid: f64,
    pub hi: f64,
    pub conf: Conf,
}

/// 推測（統計の無い述語）の既定値。
pub(crate) const GUESS: f64 = 0.01;

impl Est {
    pub(crate) fn exact(p: f64) -> Est {
        let p = p.clamp(0.0, 1.0);
        Est {
            lo: p,
            mid: p,
            hi: p,
            conf: Conf::Exact,
        }
    }

    /// 統計による推定（`spread` 倍の幅）。
    pub(crate) fn stats(p: f64, spread: f64) -> Est {
        let p = p.clamp(0.0, 1.0);
        Est {
            lo: p / spread,
            mid: p,
            hi: (p * spread).min(1.0),
            conf: Conf::Stats,
        }
    }

    /// 推測（幅は 100 倍）。
    pub(crate) fn guess(p: f64) -> Est {
        let p = p.clamp(0.0, 1.0);
        Est {
            lo: p / 100.0,
            mid: p,
            hi: (p * 100.0).min(1.0),
            conf: Conf::Guess,
        }
    }

    /// 件数探索の結果（`count < cap` なら正確・上限に達したら [cap, n]）。
    pub(crate) fn probed(count: u64, cap: u64, n: f64) -> Est {
        #[allow(clippy::cast_precision_loss)]
        let (c, k) = (count as f64, cap as f64);
        let n = n.max(1.0);
        if count < cap {
            Est::exact(c / n)
        } else {
            Est {
                lo: (k / n).min(1.0),
                mid: ((k * 10.0) / n).min(1.0),
                hi: 1.0,
                conf: Conf::Stats,
            }
        }
    }

    pub(crate) fn not(self) -> Est {
        Est {
            lo: 1.0 - self.hi,
            mid: 1.0 - self.mid,
            hi: 1.0 - self.lo,
            conf: self.conf,
        }
    }

    /// 指数バックオフ（小さい順に `s₁ · s₂^(1/2) · s₃^(1/4) …`・端ごとに独立に並べる）。
    pub(crate) fn and(v: &[Est]) -> Est {
        if v.is_empty() {
            return Est::exact(1.0);
        }
        let backoff = |mut xs: Vec<f64>| {
            xs.sort_by(f64::total_cmp);
            let mut out = 1.0;
            let mut e = 1.0;
            for x in xs {
                out *= x.clamp(0.0, 1.0).powf(e);
                e /= 2.0;
            }
            out
        };
        Est {
            lo: backoff(v.iter().map(|e| e.lo).collect()),
            mid: backoff(v.iter().map(|e| e.mid).collect()),
            hi: backoff(v.iter().map(|e| e.hi).collect()),
            conf: v.iter().map(|e| e.conf).max().unwrap_or(Conf::Exact),
        }
    }

    /// 和（`1 − Π(1 − sᵢ)`）。
    pub(crate) fn or(v: &[Est]) -> Est {
        if v.is_empty() {
            return Est::exact(0.0);
        }
        let union = |xs: &mut dyn Iterator<Item = f64>| {
            1.0 - xs.fold(1.0, |acc, x| acc * (1.0 - x.clamp(0.0, 1.0)))
        };
        Est {
            lo: union(&mut v.iter().map(|e| e.lo)),
            mid: union(&mut v.iter().map(|e| e.mid)),
            hi: union(&mut v.iter().map(|e| e.hi)),
            conf: v.iter().map(|e| e.conf).max().unwrap_or(Conf::Exact),
        }
    }

    /// 単値の `In` のような排他な和（`Σ sᵢ`・1 で頭打ち）。
    fn sum(v: &[Est]) -> Est {
        Est {
            lo: v.iter().map(|e| e.lo).sum::<f64>().min(1.0),
            mid: v.iter().map(|e| e.mid).sum::<f64>().min(1.0),
            hi: v.iter().map(|e| e.hi).sum::<f64>().min(1.0),
            conf: v.iter().map(|e| e.conf).max().unwrap_or(Conf::Exact),
        }
    }
}

/// 推定器（1 計画分）。
pub(crate) struct Estimator<'a> {
    pub stats: &'a TableStats,
    /// 値頻度表から引いた件数（計画ごと）。
    pub counts: &'a HashMap<(FieldId, String), u64>,
    pub q: &'a BoundQuery,
}

impl Estimator<'_> {
    fn n(&self) -> f64 {
        self.stats.n_rows.max(1.0)
    }

    /// 原子式の選択率。
    pub(crate) fn atom(&self, a: &Atom) -> Est {
        if self.stats.n_rows <= 0.0 {
            return Est::exact(0.0);
        }
        let f = self.q.field(a.field);
        let table = match a.leaf {
            Leaf::Contains(_) | Leaf::EndsWith(_) => return Est::guess(GUESS),
            _ => f.value_table(),
        };
        if table == IndexTable::Link {
            // 参照先ごとの件数は持たない（件数探索で埋める）。
            let frac = self
                .stats
                .links
                .get(&(a.field, false))
                .map_or(GUESS, |l| l.link_frac);
            return match a.leaf {
                Leaf::Exists => Est::stats(frac, 1.2),
                _ => Est::guess(GUESS.min(frac.max(1e-6))),
            };
        }
        let Some(fs) = self.stats.field(a.field, table) else {
            return Est::guess(GUESS);
        };
        let has = (fs.n_values / self.n()).clamp(0.0, 1.0);
        let multi = f.multi();
        match &a.leaf {
            Leaf::Exists => Est::stats(has, 1.1),
            Leaf::Eq(v) => self.eq(a.field, f.kind, fs, has, v),
            Leaf::In(vs) => {
                let parts: Vec<Est> = vs
                    .iter()
                    .map(|v| self.eq(a.field, f.kind, fs, has, v))
                    .collect();
                if multi {
                    Est::or(&parts)
                } else {
                    Est::sum(&parts)
                }
            }
            Leaf::Range { lo, hi } => {
                let p = range_frac(f.kind, fs, lo.as_ref(), hi.as_ref());
                Est::stats(p * has, 2.0)
            }
            Leaf::StartsWith(p) => {
                let lo = Some((Val::Text(p.clone()), true));
                let hi = succ(p).map(|s| (Val::Text(s), false));
                let frac = range_frac(f.kind, fs, lo.as_ref(), hi.as_ref());
                Est::stats(frac * has, 3.0)
            }
            Leaf::Contains(_) | Leaf::EndsWith(_) => Est::guess(GUESS),
        }
    }

    fn eq(&self, field: FieldId, kind: FieldKind, fs: &FieldStats, has: f64, v: &Val) -> Est {
        let key = val_key(kind, v);
        if self.stats.counts_fields.contains(&field) {
            if let Some(k) = &key {
                let c = self.counts.get(&(field, k.clone())).copied().unwrap_or(0);
                #[allow(clippy::cast_precision_loss)]
                return Est::exact(c as f64 / self.n());
            }
        }
        let Some(k) = key else {
            return Est::guess(GUESS);
        };
        if let Some((_, fr)) = fs.mcv.iter().find(|(m, _)| same(kind, m, &k)) {
            return Est::stats(fr * has, 1.3);
        }
        #[allow(clippy::cast_precision_loss)]
        let rest_n = (fs.n_distinct - fs.mcv.len() as f64).max(1.0);
        let rest_f = (1.0 - fs.mcv.iter().map(|(_, f)| f).sum::<f64>()).max(0.0);
        // 最頻値 100 件に入らない値は、最頻値の最小頻度を超えない。
        let cap = fs.mcv.last().map_or(1.0, |(_, f)| *f);
        Est::stats((rest_f / rest_n).min(cap) * has, 4.0)
    }

    /// 条件式の選択率。
    pub(crate) fn expr(&self, e: &Expr) -> Est {
        match e {
            Expr::True => Est::exact(1.0),
            Expr::False => Est::exact(0.0),
            Expr::Atom(a) => self.atom(a),
            Expr::Not(x) => self.expr(x).not(),
            Expr::And(v) => Est::and(&v.iter().map(|x| self.expr(x)).collect::<Vec<_>>()),
            Expr::Or(v) => Est::or(&v.iter().map(|x| self.expr(x)).collect::<Vec<_>>()),
        }
    }

    /// 行述語の選言 1 つの選択率（認可駆動の候補件数）。
    pub(crate) fn driver(&self, d: &Driver) -> Est {
        if self.stats.n_rows <= 0.0 {
            return Est::exact(0.0);
        }
        let by_counts = |field: FieldId, vals: &[String]| -> Option<Est> {
            if !self.stats.counts_fields.contains(&field) {
                return None;
            }
            #[allow(clippy::cast_precision_loss)]
            let n: f64 = vals
                .iter()
                .map(|v| self.counts.get(&(field, v.clone())).copied().unwrap_or(0) as f64)
                .sum();
            Some(Est::exact(n / self.n()))
        };
        match d {
            #[allow(clippy::cast_precision_loss)]
            Driver::Shared(ids) => Est::exact(ids.len() as f64 / self.n()),
            Driver::Owner(u) => by_counts(SYSTEM_FIELD_OWNER, std::slice::from_ref(u))
                .unwrap_or_else(|| Est::guess(GUESS)),
            Driver::Text { field, vals } => by_counts(*field, vals).unwrap_or_else(|| {
                let Some(fs) = self.stats.field(*field, IndexTable::Text) else {
                    return Est::guess(GUESS);
                };
                let has = (fs.n_values / self.n()).clamp(0.0, 1.0);
                let parts: Vec<Est> = vals
                    .iter()
                    .map(|v| self.eq(*field, FieldKind::Text, fs, has, &Val::Text(v.clone())))
                    .collect();
                Est::sum(&parts)
            }),
            Driver::Num { field, val } => {
                let Some(fs) = self.stats.field(*field, IndexTable::Num) else {
                    return Est::guess(GUESS);
                };
                let has = (fs.n_values / self.n()).clamp(0.0, 1.0);
                match Dec::parse(val) {
                    Some(d) => self.eq(*field, FieldKind::Num, fs, has, &Val::Num(d)),
                    None => Est::guess(GUESS),
                }
            }
        }
    }

    /// 値頻度表から引くべき (列, 値)（Q の等値・`In` と、行述語の選言）。
    pub(crate) fn wanted(
        stats: &TableStats,
        q: &BoundQuery,
        drivers: &[Driver],
    ) -> Vec<(FieldId, String)> {
        let mut out = Vec::new();
        let mut atoms = Vec::new();
        q.filter.atoms(&mut atoms);
        for a in atoms {
            if !stats.counts_fields.contains(&a.field) {
                continue;
            }
            let kind = q.field(a.field).kind;
            let vals: Vec<&Val> = match &a.leaf {
                Leaf::Eq(v) => vec![v],
                Leaf::In(v) => v.iter().collect(),
                _ => continue,
            };
            out.extend(
                vals.into_iter()
                    .filter_map(|v| val_key(kind, v))
                    .map(|k| (a.field, k)),
            );
        }
        for d in drivers {
            match d {
                Driver::Owner(u) if stats.counts_fields.contains(&SYSTEM_FIELD_OWNER) => {
                    out.push((SYSTEM_FIELD_OWNER, u.clone()));
                }
                Driver::Text { field, vals } if stats.counts_fields.contains(field) => {
                    out.extend(vals.iter().map(|v| (*field, v.clone())));
                }
                _ => {}
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// 比較値の索引上の表現（text はプレフィクス・num は正準形）。
fn val_key(kind: FieldKind, v: &Val) -> Option<String> {
    match (kind, v) {
        (FieldKind::Num, Val::Num(d)) => Some(d.canonical()),
        (_, Val::Text(s)) => Some(
            crate::index::normalize::prefix_at_char_boundary(s, crate::index::MAX_INDEX_KEY_BYTES)
                .0
                .to_string(),
        ),
        _ => None,
    }
}

fn same(kind: FieldKind, stored: &str, key: &str) -> bool {
    match kind {
        FieldKind::Num => Dec::parse(stored)
            .zip(Dec::parse(key))
            .is_some_and(|(a, b)| a == b),
        _ => stored == key,
    }
}

/// 前方一致の上端（最後のコードポイントを 1 進めた文字列・§3.2 規則 5）。
fn succ(p: &str) -> Option<String> {
    let mut chars: Vec<char> = p.chars().collect();
    while let Some(c) = chars.pop() {
        let mut n = u32::from(c) + 1;
        if (0xD800..=0xDFFF).contains(&n) {
            n = 0xE000;
        }
        if let Some(nc) = char::from_u32(n) {
            chars.push(nc);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

/// 値の比較（ヒストグラムの境界と比較値）。
fn cmp_vals(kind: FieldKind, a: &str, b: &Val) -> Option<std::cmp::Ordering> {
    match (kind, b) {
        (FieldKind::Num, Val::Num(d)) => Dec::parse(a).map(|x| x.cmp(d)),
        (_, Val::Text(s)) => Some(a.as_bytes().cmp(s.as_bytes())),
        _ => None,
    }
}

/// 値を持つ行のうち `[lo, hi]` に入る割合（ヒストグラムの区間内を線形補間・区間は 1/100 刻み）。
fn range_frac(
    kind: FieldKind,
    fs: &FieldStats,
    lo: Option<&(Val, bool)>,
    hi: Option<&(Val, bool)>,
) -> f64 {
    let h = &fs.hist;
    if h.len() < 2 {
        return 0.3;
    }
    #[allow(clippy::cast_precision_loss)]
    let buckets = (h.len() - 1) as f64;
    // 比較値以下の割合（境界の何番目までが値以下か・同じ区間の中は半分とみなす）。
    let below = |v: &Val| -> f64 {
        let mut le = 0usize;
        for b in h {
            match cmp_vals(kind, b, v) {
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal) => le += 1,
                Some(std::cmp::Ordering::Greater) => break,
                None => return 0.5,
            }
        }
        if le == 0 {
            0.0
        } else if le >= h.len() {
            1.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let at = le as f64 - 0.5;
            (at / buckets).clamp(0.0, 1.0)
        }
    };
    let a = lo.map_or(0.0, |(v, _)| below(v));
    let b = hi.map_or(1.0, |(v, _)| below(v));
    // 1 区間ぶんの幅は最低限残す（境界が粗い分の過小推定を避ける）。
    (b - a).max(1.0 / (buckets * 2.0)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_not_a_product() {
        let a = Est::exact(0.1);
        let b = Est::exact(0.1);
        let and = Est::and(&[a, b]);
        // 積なら 0.01、バックオフは 0.1 · 0.1^(1/2) ≈ 0.0316。
        assert!((and.mid - 0.1 * 0.1f64.sqrt()).abs() < 1e-9);
        assert_eq!(and.conf, Conf::Exact);
        let g = Est::and(&[a, Est::guess(0.5)]);
        assert_eq!(g.conf, Conf::Guess, "最弱の信頼度");
    }

    #[test]
    fn or_not_and_probe_intervals() {
        let o = Est::or(&[Est::exact(0.5), Est::exact(0.5)]);
        assert!((o.mid - 0.75).abs() < 1e-9);
        let n = Est::stats(0.2, 2.0).not();
        assert!((n.lo - 0.6).abs() < 1e-9 && (n.hi - 0.9).abs() < 1e-9);
        let p = Est::probed(10, 1_000, 1e5);
        assert_eq!(p.conf, Conf::Exact);
        let q = Est::probed(1_000, 1_000, 1e5);
        assert!(q.lo > 0.009 && q.hi >= 1.0);
    }

    #[test]
    fn histogram_interpolates_ranges() {
        let fs = FieldStats {
            n_values: 100.0,
            hist: (0..=100).map(|i| format!("{i:03}")).collect(),
            ..Default::default()
        };
        let lo = (Val::Text("010".into()), true);
        let hi = (Val::Text("030".into()), false);
        let f = range_frac(FieldKind::Text, &fs, Some(&lo), Some(&hi));
        assert!((f - 0.2).abs() < 0.02, "{f}");
        assert_eq!(succ("ab").as_deref(), Some("ac"));
    }
}
