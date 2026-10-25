//! ソート駆動のシーク（検証 D-3）: 第 1 ソートキーと同じ列の範囲条件で、駆動索引の走査の始点と終点を縛る。
//!
//! 「日付の範囲（1 か月）＋同じ日付列の昇順」で、ソート駆動が索引を**先頭から**歩き、範囲より前の行を
//! 全部読み飛ばしていた（統計が無いと 30 万行で statement timeout）。範囲は連言の 1 項なので、範囲の外の
//! 索引行は検査を通らない。始点を範囲の端（昇順なら下限・降順なら上限）へ、終点を反対の端へ置けば
//! 結果は同じで、読むのは範囲の中だけになる。統計の有無に関わらず効く（推定を使わない）。
//!
//! 範囲・等値・前方一致などの値を要する条件が第 1 キーにあれば、値の無い行（区間 2）も検査を通らないので
//! 区間 2 は歩かない。

use super::keys::SortVal;
use super::lower::SAFE_BOUND_BYTES;
use crate::query::bound::{Atom, BSort, Expr, FieldKind, Leaf, Val};

/// 第 1 ソートキーの走査の縛り。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SortSeek {
    /// 走査の始点（この値の同値群の先頭から・含む）。
    pub start: Option<SortVal>,
    /// 走査の終点（値, 含むか）。
    pub stop: Option<(SortVal, bool)>,
    /// 第 1 キーに値を要する条件がある（区間 2＝値の無い行は検査を通らない）。
    pub needs_value: bool,
}

/// 範囲の端を索引の値と同じ表現にする。長い text の端は切り詰め行と比べられないので使わない
/// （検査が本体で比べる・[`SAFE_BOUND_BYTES`]）。
fn sort_val(kind: FieldKind, v: &Val) -> Option<SortVal> {
    match (kind, v) {
        (FieldKind::Num, Val::Num(d)) => Some(SortVal::Num(d.clone())),
        (FieldKind::Text, Val::Text(s)) if s.len() <= SAFE_BOUND_BYTES => {
            Some(SortVal::Text(s.clone()))
        }
        _ => None,
    }
}

/// 連言の各項から、第 1 ソートキーの列の範囲（等値は両端が同じ範囲）を拾う。
pub(crate) fn sort_seek(conj: &[&Expr], key: BSort, kind: FieldKind) -> SortSeek {
    let mut seek = SortSeek::default();
    if !matches!(kind, FieldKind::Num | FieldKind::Text) {
        return seek;
    }
    for e in conj {
        let Expr::Atom(Atom { field, leaf }) = e else {
            continue;
        };
        if *field != key.field {
            continue;
        }
        let (lo, hi) = match leaf {
            Leaf::Range { lo, hi } => (lo.clone(), hi.clone()),
            Leaf::Eq(v) => (Some((v.clone(), true)), Some((v.clone(), true))),
            Leaf::In(_)
            | Leaf::StartsWith(_)
            | Leaf::Contains(_)
            | Leaf::EndsWith(_)
            | Leaf::Exists => {
                seek.needs_value = true;
                continue;
            }
        };
        seek.needs_value = true;
        let (first, last) = if key.desc { (hi, lo) } else { (lo, hi) };
        if seek.start.is_none() {
            seek.start = first.and_then(|(v, _)| sort_val(kind, &v));
        }
        if seek.stop.is_none() {
            seek.stop = last.and_then(|(v, inc)| sort_val(kind, &v).map(|s| (s, inc)));
        }
    }
    seek
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn text(s: &str) -> Val {
        Val::Text(s.into())
    }

    #[test]
    fn range_on_the_sort_key_bounds_both_ends_in_scan_order() {
        let range = Expr::atom(
            7,
            Leaf::Range {
                lo: Some((text("2025-01-01"), true)),
                hi: Some((text("2025-02-01"), false)),
            },
        );
        let other = Expr::atom(8, Leaf::Eq(text("x")));
        let conj = vec![&other, &range];
        let asc = sort_seek(
            &conj,
            BSort {
                field: 7,
                desc: false,
            },
            FieldKind::Text,
        );
        assert_eq!(asc.start, Some(SortVal::Text("2025-01-01".into())));
        assert_eq!(asc.stop, Some((SortVal::Text("2025-02-01".into()), false)));
        assert!(asc.needs_value);
        let desc = sort_seek(
            &conj,
            BSort {
                field: 7,
                desc: true,
            },
            FieldKind::Text,
        );
        assert_eq!(desc.start, Some(SortVal::Text("2025-02-01".into())));
        assert_eq!(desc.stop, Some((SortVal::Text("2025-01-01".into()), true)));
        // 別の列の範囲は縛らない。
        let none = sort_seek(
            &conj,
            BSort {
                field: 9,
                desc: false,
            },
            FieldKind::Text,
        );
        assert_eq!(none, SortSeek::default());
    }

    #[test]
    fn long_text_bounds_and_value_only_conditions_do_not_seek() {
        let long = "a".repeat(SAFE_BOUND_BYTES + 1);
        let range = Expr::atom(
            7,
            Leaf::Range {
                lo: Some((text(&long), true)),
                hi: None,
            },
        );
        let conj = vec![&range];
        let s = sort_seek(
            &conj,
            BSort {
                field: 7,
                desc: false,
            },
            FieldKind::Text,
        );
        assert_eq!(s.start, None, "長い端は切り詰め行と比べられない");
        assert!(s.needs_value);
        let sw = Expr::atom(7, Leaf::StartsWith("ab".into()));
        let conj = vec![&sw];
        let s = sort_seek(
            &conj,
            BSort {
                field: 7,
                desc: false,
            },
            FieldKind::Text,
        );
        assert_eq!((s.start, s.stop, s.needs_value), (None, None, true));
        // 否定の中の範囲は縛らない（連言の項ではない）。
        let not = Expr::Not(Box::new(range.clone()));
        let conj = vec![&not];
        let s = sort_seek(
            &conj,
            BSort {
                field: 7,
                desc: false,
            },
            FieldKind::Text,
        );
        assert_eq!(s, SortSeek::default());
    }
}
