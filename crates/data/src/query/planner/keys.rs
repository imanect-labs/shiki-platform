//! ソート値・継続位置・全順序（query-optimizer.md §6.3・data-platform §3.3 / §5.3）。
//!
//! 全順序は **(第 1 キー, …, 第 k キー, `record_int`)**。各キーは指定の向きで並べ、値なしは
//! 昇順・降順とも末尾に固定する。`record_int` はキーの向きに合わせたタイブレーカ（第 1 キーの向き）。
//! 戦略はすべてこの同じ全順序の上で「位置より後ろ」を定義するので、ページごとに戦略が変わっても
//! 重複・欠落は起きない（静的な集合に対して）。
//!
//! text のソート値は**索引に格納されたプレフィクス**（切り詰め時）。完全値ではない。

use std::cmp::Ordering;

use serde_json::Value;

use crate::cursor::CursorPayload;
use crate::numeric::Dec;
use crate::query::bound::{BSort, BoundQuery, FieldKind};
use crate::DataError;

/// 1 キー分のソート値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SortVal {
    Text(String),
    Num(Dec),
    Missing,
}

impl SortVal {
    pub(crate) fn is_missing(&self) -> bool {
        matches!(self, SortVal::Missing)
    }

    /// 同じ向きでの比較（値なしは常に後ろ）。
    fn cmp_dir(&self, other: &SortVal, desc: bool) -> Ordering {
        let base = match (self, other) {
            (SortVal::Missing, SortVal::Missing) => return Ordering::Equal,
            (SortVal::Missing, _) => return Ordering::Greater,
            (_, SortVal::Missing) => return Ordering::Less,
            (SortVal::Text(a), SortVal::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
            (SortVal::Num(a), SortVal::Num(b)) => a.cmp(b),
            // 型違いは束縛で起きない。決定的にするだけ。
            (SortVal::Text(_), SortVal::Num(_)) => Ordering::Less,
            (SortVal::Num(_), SortVal::Text(_)) => Ordering::Greater,
        };
        if desc {
            base.reverse()
        } else {
            base
        }
    }

    /// SQL から受けた text 表現（num は numeric の text 出力）を型に合わせて読む。
    pub(crate) fn from_db(kind: FieldKind, v: Option<String>) -> SortVal {
        match (kind, v) {
            (_, None) => SortVal::Missing,
            (FieldKind::Num, Some(s)) => Dec::parse(&s).map_or(SortVal::Missing, SortVal::Num),
            (_, Some(s)) => SortVal::Text(s),
        }
    }

    /// SQL へ渡す text 表現（num は 10 進正準形）。
    pub(crate) fn to_db(&self) -> Option<String> {
        match self {
            SortVal::Text(s) => Some(s.clone()),
            SortVal::Num(d) => Some(d.canonical()),
            SortVal::Missing => None,
        }
    }
}

/// 全順序の上の位置（ソート値列 ＋ `record_int`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pos {
    pub vals: Vec<SortVal>,
    pub rint: i64,
}

impl Pos {
    /// 第 1 キーが値なし（区間 2）か。
    pub(crate) fn segment(&self) -> u8 {
        if self.vals.first().is_some_and(SortVal::is_missing) {
            2
        } else {
            1
        }
    }
}

/// 全順序での比較。
#[allow(clippy::many_single_char_names)] // 比較の両辺（a/b・x/y）
pub(crate) fn cmp_pos(sort: &[BSort], a: &Pos, b: &Pos) -> Ordering {
    for (i, k) in sort.iter().enumerate() {
        let (x, y) = (a.vals.get(i), b.vals.get(i));
        if let (Some(x), Some(y)) = (x, y) {
            let o = x.cmp_dir(y, k.desc);
            if o != Ordering::Equal {
                return o;
            }
        }
    }
    let desc = sort.first().is_some_and(|k| k.desc);
    let o = a.rint.cmp(&b.rint);
    if desc {
        o.reverse()
    } else {
        o
    }
}

/// 位置 → カーソルの中身（戦略に依存しない・§6.3）。
pub(crate) fn to_payload(pos: &Pos) -> CursorPayload {
    CursorPayload {
        sort_values: pos
            .vals
            .iter()
            .map(|v| v.to_db().map_or(Value::Null, Value::String))
            .collect(),
        record_int: pos.rint,
        segment: pos.segment(),
        snapshot_seq: None,
    }
}

/// カーソルの中身 → 位置（ソート指定と食い違えば 422・束縛は AAD でも検証済み）。
pub(crate) fn from_payload(q: &BoundQuery, p: &CursorPayload) -> Result<Pos, DataError> {
    let invalid = || DataError::Invalid("カーソルが不正です".into());
    if p.sort_values.len() != q.sort.len() {
        return Err(invalid());
    }
    let mut vals = Vec::with_capacity(q.sort.len());
    for (k, v) in q.sort.iter().zip(&p.sort_values) {
        let kind = q.field(k.field).kind;
        vals.push(match v {
            Value::Null => SortVal::Missing,
            Value::String(s) => match SortVal::from_db(kind, Some(s.clone())) {
                SortVal::Missing => return Err(invalid()),
                ok => ok,
            },
            _ => return Err(invalid()),
        });
    }
    let pos = Pos {
        vals,
        rint: p.record_int,
    };
    if pos.segment() != p.segment {
        return Err(invalid());
    }
    Ok(pos)
}

// ---------------------------------------------------------------------------
// 複合索引のキー（順序を保つバイト列・data-platform §5.3）
// ---------------------------------------------------------------------------

/// 値ありの印（値なし `0x02` より前＝値なしは昇順・降順とも末尾）。
const PRESENT: u8 = 0x01;
const MISSING: u8 = 0x02;

/// 全順序（[`cmp_pos`]）と同じ順に並ぶバイト列。各キーは `[印][値]`（降順のキーは値のバイトを反転）、
/// 末尾に `record_int`（第 1 キーの向き）。prefix-free な符号化なので連結しても順序が保たれる。
/// バイト列を反転する（降順のキー）。
fn invert(b: &mut [u8]) {
    for x in b {
        *x = !*x;
    }
}

pub(crate) fn encode_pos(kinds: &[(FieldKind, bool)], pos: &Pos) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    for (i, (kind, desc)) in kinds.iter().enumerate() {
        match pos.vals.get(i).unwrap_or(&SortVal::Missing) {
            SortVal::Missing => out.push(MISSING),
            v => {
                out.push(PRESENT);
                let mut b = encode_val(*kind, v);
                if *desc {
                    invert(&mut b);
                }
                out.extend_from_slice(&b);
            }
        }
    }
    let mut r = (pos.rint as u64 ^ (1 << 63)).to_be_bytes();
    if kinds.first().is_some_and(|k| k.1) {
        invert(&mut r);
    }
    out.extend_from_slice(&r);
    out
}

fn encode_val(kind: FieldKind, v: &SortVal) -> Vec<u8> {
    match (kind, v) {
        (_, SortVal::Num(d)) => encode_num(d),
        (_, SortVal::Text(s)) => {
            // 0x00 を 0x00 0xFF にエスケープし 0x00 0x01 で終端（prefix-free・バイト順を保つ）。
            let mut out = Vec::with_capacity(s.len() + 2);
            for b in s.as_bytes() {
                out.push(*b);
                if *b == 0 {
                    out.push(0xFF);
                }
            }
            out.extend_from_slice(&[0x00, 0x01]);
            out
        }
        (_, SortVal::Missing) => Vec::new(),
    }
}

/// 数値: ゼロ `0x80`、正 `0x81 ‖ exp(u16) ‖ 有効数字 ‖ 0x00`、負は `0x7F ‖ 正の符号化の反転`。
fn encode_num(d: &Dec) -> Vec<u8> {
    let Some((neg, exp, digits)) = d.sci() else {
        return vec![0x80];
    };
    let e = u16::try_from((exp + 32_768).clamp(0, 65_535)).unwrap_or(0);
    let mut body = e.to_be_bytes().to_vec();
    body.extend_from_slice(digits.as_bytes());
    body.push(0x00);
    if neg {
        invert(&mut body);
        let mut out = vec![0x7F];
        out.extend(body);
        out
    } else {
        let mut out = vec![0x81];
        out.extend(body);
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn t(s: &str) -> SortVal {
        SortVal::Text(s.into())
    }

    #[test]
    fn missing_is_last_in_both_directions() {
        for desc in [false, true] {
            let sort = [BSort { field: 16, desc }];
            let a = Pos {
                vals: vec![t("a")],
                rint: 1,
            };
            let m = Pos {
                vals: vec![SortVal::Missing],
                rint: 0,
            };
            assert_eq!(cmp_pos(&sort, &a, &m), Ordering::Less, "desc={desc}");
        }
    }

    #[test]
    fn record_int_breaks_ties_in_key_direction() {
        let a = Pos {
            vals: vec![t("x")],
            rint: 1,
        };
        let b = Pos {
            vals: vec![t("x")],
            rint: 2,
        };
        let asc = [BSort {
            field: 16,
            desc: false,
        }];
        let desc = [BSort {
            field: 16,
            desc: true,
        }];
        assert_eq!(cmp_pos(&asc, &a, &b), Ordering::Less);
        assert_eq!(cmp_pos(&desc, &a, &b), Ordering::Greater);
    }

    #[test]
    fn composite_encoding_preserves_the_total_order() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0
            }
        }
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let nums = [
            "-12.5", "-1", "-0.05", "0", "0.05", "0.5", "1", "1.25", "10", "100", "99.999",
        ];
        let texts = ["", "a", "a\0", "a\0b", "ab", "b", "承認", "\u{10FFFF}"];
        let gen = |rng: &mut Rng, k: FieldKind| -> SortVal {
            let n = rng.next();
            if n.is_multiple_of(7) {
                return SortVal::Missing;
            }
            match k {
                FieldKind::Num => {
                    SortVal::Num(Dec::parse(nums[(n % nums.len() as u64) as usize]).unwrap())
                }
                _ => SortVal::Text(texts[(n % texts.len() as u64) as usize].to_string()),
            }
        };
        for desc in [(false, false), (true, false), (false, true), (true, true)] {
            let kinds = [(FieldKind::Text, desc.0), (FieldKind::Num, desc.1)];
            let sort = [
                BSort {
                    field: 16,
                    desc: desc.0,
                },
                BSort {
                    field: 17,
                    desc: desc.1,
                },
            ];
            let pos: Vec<Pos> = (0..300)
                .map(|_| Pos {
                    vals: vec![
                        gen(&mut rng, FieldKind::Text),
                        gen(&mut rng, FieldKind::Num),
                    ],
                    rint: (rng.next() % 50) as i64,
                })
                .collect();
            for a in &pos {
                for b in &pos {
                    assert_eq!(
                        encode_pos(&kinds, a).cmp(&encode_pos(&kinds, b)),
                        cmp_pos(&sort, a, b),
                        "{a:?} vs {b:?} desc={desc:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn numbers_compare_numerically() {
        let n = |s: &str| SortVal::Num(Dec::parse(s).unwrap());
        assert_eq!(n("9").cmp_dir(&n("10"), false), Ordering::Less);
        assert_eq!(n("1.0").cmp_dir(&n("1"), false), Ordering::Equal);
    }
}
