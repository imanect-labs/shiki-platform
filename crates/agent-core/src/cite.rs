//! 引用番号の採番（issue #508）。
//!
//! doc_search は呼び出しごとに結果を返すだけで、番号は振らない。結果の本文には
//! [`placeholder`] で仮の印（`⟦cite:i⟧`、i はその呼び出しの何件目か）を書いておき、ループが
//! **呼び出し順に**通し番号（`cite_id`）を振って `[n]` に置き換える。
//!
//! - 応答の中で番号は一意。2 回目の検索の結果は前回の続きの番号になる。
//! - 同じチャンクが再びヒットしたら同じ番号を使い回す（本文の `[n]` が一つの箇所を指し続ける）。
//! - 台帳はチェックポイントに載せる（再開しても番号が続く）。
//! - サブエージェントの報告に含まれる子の番号は、[`relabel_to_placeholders`] で仮の印に戻して
//!   から親の台帳で振り直す（親子で番号の空間を共有する）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::tool::Citation;

const OPEN: &str = "⟦cite:";
const CLOSE: char = '⟧';

/// その呼び出しの `local` 件目（0 起点）の引用を指す仮の印。
#[must_use]
pub fn placeholder(local: usize) -> String {
    format!("{OPEN}{local}{CLOSE}")
}

/// 応答内の引用番号の台帳。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationLedger {
    /// chunk_id → 番号。
    by_chunk: HashMap<String, u32>,
    /// 最後に振った番号。
    last: u32,
}

impl CitationLedger {
    /// 引用に番号を振り、本文の仮の印を `[n]` に置き換える。
    ///
    /// 印の i は `citations` の添字。範囲外の印は消す（モデルに壊れた番号を見せない）。
    pub fn number(&mut self, citations: &mut [Citation], text: &mut String) {
        for c in citations.iter_mut() {
            c.cite_id = match self.by_chunk.get(&c.chunk_id) {
                Some(n) => *n,
                None => {
                    self.last += 1;
                    self.by_chunk.insert(c.chunk_id.clone(), self.last);
                    self.last
                }
            };
        }
        if text.contains(OPEN) {
            *text = replace_placeholders(text, |i| citations.get(i).map(|c| c.cite_id));
        }
    }
}

/// 仮の印を `[n]` に置き換える（`lookup` が None の印は消す）。
fn replace_placeholders(text: &str, lookup: impl Fn(usize) -> Option<u32>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with(CLOSE) {
            if let Some(n) = after[..digits].parse::<usize>().ok().and_then(&lookup) {
                out.push_str(&format!("[{n}]"));
            }
            rest = &after[digits + CLOSE.len_utf8()..];
        } else {
            // 印の形をしていない（本文にたまたま現れた）ものはそのまま残す。
            out.push_str(OPEN);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// 子（サブエージェント）の本文中の `[n]` を、親に渡す引用列の仮の印へ戻す。
///
/// `n` は子の台帳の番号（`citations[i].cite_id`）。対応する引用が無い `[n]` は本文の一部として
/// そのまま残す（配列の添字などを壊さない）。
#[must_use]
pub fn relabel_to_placeholders(text: &str, citations: &[Citation]) -> String {
    let index: HashMap<u32, usize> = citations
        .iter()
        .enumerate()
        .rev() // 同じ番号が複数あれば先頭を採る
        .filter(|(_, c)| c.cite_id > 0)
        .map(|(i, c)| (c.cite_id, i))
        .collect();
    if index.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('[') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let hit = (digits > 0 && after[digits..].starts_with(']'))
            .then(|| after[..digits].parse::<u32>().ok())
            .flatten()
            .and_then(|n| index.get(&n));
        match hit {
            Some(i) => {
                out.push_str(&placeholder(*i));
                rest = &after[digits + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cite(chunk: &str) -> Citation {
        Citation {
            cite_id: 0,
            node_id: "n".into(),
            chunk_id: chunk.into(),
            snippet: String::new(),
            page: None,
            heading_path: Vec::new(),
            score: 0.0,
            version: None,
            anchor: None,
            quote: None,
            boxes: Vec::new(),
        }
    }

    #[test]
    fn numbers_continue_across_calls_and_reuse_same_chunk() {
        let mut ledger = CitationLedger::default();

        let mut first = vec![cite("a"), cite("b")];
        let mut t1 = format!("{} 規程\n{} 手引き", placeholder(0), placeholder(1));
        ledger.number(&mut first, &mut t1);
        assert_eq!(t1, "[1] 規程\n[2] 手引き");
        assert_eq!(first[1].cite_id, 2);

        // 2 回目の検索: 新しいチャンクは 3 から。再ヒットした b は 2 のまま。
        let mut second = vec![cite("c"), cite("b")];
        let mut t2 = format!("{} 協定\n{} 手引き", placeholder(0), placeholder(1));
        ledger.number(&mut second, &mut t2);
        assert_eq!(t2, "[3] 協定\n[2] 手引き");
        assert_eq!(
            second.iter().map(|c| c.cite_id).collect::<Vec<_>>(),
            vec![3, 2]
        );
    }

    #[test]
    fn out_of_range_placeholder_is_dropped_and_lookalikes_kept() {
        let mut ledger = CitationLedger::default();
        let mut cs = vec![cite("a")];
        let mut t = format!("{}{} ⟦cite:x⟧", placeholder(0), placeholder(5));
        ledger.number(&mut cs, &mut t);
        assert_eq!(t, "[1] ⟦cite:x⟧");
    }

    #[test]
    fn ledger_survives_checkpoint_roundtrip() {
        let mut ledger = CitationLedger::default();
        ledger.number(&mut [cite("a"), cite("b")], &mut String::new());
        let back: CitationLedger =
            serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
        let mut more = vec![cite("c")];
        let mut back = back;
        back.number(&mut more, &mut String::new());
        assert_eq!(more[0].cite_id, 3);
    }

    #[test]
    fn child_numbers_map_back_to_parent_numbering() {
        // 子は a=1, b=2 で書いた。親は既に x を 1 番で振っている。
        let mut child = vec![cite("a"), cite("b")];
        CitationLedger::default().number(&mut child, &mut String::new());
        let report = "条件は A[1]、期限は B[2]。配列 arr[0] と [9] は触らない。";
        let mut text = relabel_to_placeholders(report, &child);

        let mut parent = CitationLedger::default();
        parent.number(&mut [cite("x")], &mut String::new());
        parent.number(&mut child, &mut text);
        assert_eq!(text, "条件は A[2]、期限は B[3]。配列 arr[0] と [9] は触らない。");
    }
}
