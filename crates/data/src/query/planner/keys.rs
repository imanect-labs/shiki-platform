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
    fn numbers_compare_numerically() {
        let n = |s: &str| SortVal::Num(Dec::parse(s).unwrap());
        assert_eq!(n("9").cmp_dir(&n("10"), false), Ordering::Less);
        assert_eq!(n("1.0").cmp_dir(&n("1"), false), Ordering::Equal);
    }
}
