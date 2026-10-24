//! 見出しと節の範囲（AI 編集・#886）。
//!
//! 節 = 見出しから、次の同じか上の段の見出しの手前まで。見出しは文字（前後の空白と大小を
//! 無視）で引き、**一意でなければ選ばない**（同じ名前の見出しが複数あるときに、黙って最初の
//! 節を書き換えない）。親の見出しを `親 > 子` の形で添えれば、親の節の中から子を引く。
//!
//! 最上位のブロックは 1 回の走査で読む（`get(i)` は先頭から数え直すので、繰り返すと O(n²)）。

use yrs::types::text::YChange;
use yrs::{Any, Out, ReadTxn, Text, Xml, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut};

use super::ai_text::DELETION;
use super::yjs_map::is_on;

/// 最上位の見出し 1 つ。
struct Heading {
    index: u32,
    level: u8,
    text: String,
}

/// 最上位の見出しの一覧と、最上位のブロックの数。
fn headings<T: ReadTxn>(txn: &T, fragment: &XmlFragmentRef) -> (Vec<Heading>, u32) {
    let mut out = Vec::new();
    let mut total = 0u32;
    for (i, child) in fragment.children(txn).enumerate() {
        total += 1;
        if let XmlOut::Element(el) = child {
            if &**el.tag() == "heading" {
                out.push(Heading {
                    index: i as u32,
                    level: level_of(txn, &el),
                    text: visible_text(txn, &el).trim().to_lowercase(),
                });
            }
        }
    }
    (out, total)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // level は 1..=6 に有界。
fn level_of<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> u8 {
    match el.get_attribute(txn, "level") {
        Some(Out::Any(Any::BigInt(n))) => n.clamp(1, 6) as u8,
        Some(Out::Any(Any::Number(n))) => n.clamp(1.0, 6.0) as u8,
        _ => 1,
    }
}

/// `index` の見出しの節の終わり（次の同じか上の段の見出し、無ければ文書の末尾）。
fn end_of(heads: &[Heading], index: u32, level: u8, total: u32) -> u32 {
    heads
        .iter()
        .find(|h| h.index > index && h.level <= level)
        .map_or(total, |h| h.index)
}

/// 見出し（`親 > 子` の形も可）の節を `(見出しの位置, 節の終わり)` で返す。
pub(super) fn section<T: ReadTxn>(
    txn: &T,
    fragment: &XmlFragmentRef,
    spec: &str,
) -> Result<(u32, u32), String> {
    let (heads, total) = headings(txn, fragment);
    let parts: Vec<&str> = spec
        .split(" > ")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("見出しが空".into());
    }
    let (mut from, mut to) = (0u32, total);
    let mut found = (0u32, total);
    for part in parts {
        let target = part.to_lowercase();
        let hits: Vec<&Heading> = heads
            .iter()
            .filter(|h| h.index >= from && h.index < to && h.text == target)
            .collect();
        let h = match hits.as_slice() {
            [] => return Err(format!("見出し「{part}」が見つからない")),
            [h] => *h,
            many => {
                return Err(format!(
                    "見出し「{part}」が {} 個ある。「親の見出し > {part}」の形で親を添える",
                    many.len()
                ))
            }
        };
        found = (h.index, end_of(&heads, h.index, h.level, total));
        (from, to) = (h.index + 1, found.1);
    }
    Ok(found)
}

/// 見出しの位置だけ（挿入先・移動先）。
pub(super) fn heading_at<T: ReadTxn>(
    txn: &T,
    fragment: &XmlFragmentRef,
    spec: &str,
) -> Result<u32, String> {
    section(txn, fragment, spec).map(|(i, _)| i)
}

/// 要素の中の、削除の提案が付いていない文字（書式のタグは含まない）。
pub(super) fn visible_text<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> String {
    let mut out = String::new();
    for child in el.children(txn) {
        match child {
            XmlOut::Text(t) => {
                for d in t.diff(txn, YChange::identity) {
                    let deleted = d
                        .attributes
                        .as_ref()
                        .is_some_and(|a| is_on(a.get(DELETION)));
                    if let (Out::Any(Any::String(s)), false) = (d.insert, deleted) {
                        out.push_str(&s);
                    }
                }
            }
            XmlOut::Element(e) => out.push_str(&visible_text(txn, &e)),
            XmlOut::Fragment(_) => {}
        }
    }
    out
}
