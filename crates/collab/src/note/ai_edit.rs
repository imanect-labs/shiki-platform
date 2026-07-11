//! AI 共同編集の編集オペレーション（Task 11P.4・design §4.8.1）。
//!
//! エージェントの `document.edit` は**共同編集参加者**として、人間と同じ Yjs ドキュメント
//! に変更を適用する（別コピーを作らない）。CRDT の収束保証は Yjs が担うため、人間の
//! 並行編集とも自然に収束する。編集は md アンカー指定のブロック操作で表現する:
//!
//! - `Append`: 末尾にブロックを追加（非破壊・最も安全）
//! - `ReplaceSection`: 見出しテキスト一致の節（見出し〜次の同/上位見出しの手前）を置換
//! - `InsertAfterHeading`: 見出し直後にブロックを挿入
//! - `ReplaceAll`: 文書全体を置換（最終手段）
//!
//! **既定は直接適用**（AI 名義・Yjs undo 可）。サジェストモードは挿入テキストに
//! `aiSuggestion` マークを付け、エディタが承認/棄却 UI を出す（マークは md に落とさず
//! Yjs snapshot 側の正本に保つ・PIT-37③）。

use serde::Deserialize;
use yrs::{
    GetString, MapRef, TransactionMut, Xml, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut,
};

use super::ast::Block;
use super::md_parse::parse_markdown;
use super::yjs_map;
use super::yjs_meta::write_meta_pair;

/// AI 編集の適用モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditMode {
    /// 直接適用（既定・AI 名義で本文へ反映）。
    #[default]
    Direct,
    /// サジェスト（提案マーク付きで挿入・人間が承認/棄却）。
    Suggest,
}

/// 1 つの編集操作（md で内容を指定する）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    /// 文書末尾に markdown を追記する。
    Append { markdown: String },
    /// `heading` に一致する節の本文（見出しの次〜次の同/上位見出しの手前）を置換する。
    ReplaceSection { heading: String, markdown: String },
    /// `heading` に一致する見出しの直後に markdown を挿入する。
    InsertAfterHeading { heading: String, markdown: String },
    /// 文書全体を markdown で置換する（最終手段）。
    ReplaceAll { markdown: String },
    /// メタデータ（frontmatter 型属性）を 1 件設定する（tags は `,` 区切り可）。
    SetMeta { key: String, value: String },
}

/// 操作の適用結果サマリ（ツールが観測テキストに載せる）。
#[derive(Debug, Default)]
pub struct EditReport {
    pub applied: usize,
    pub skipped: Vec<String>,
}

/// 編集オペ列をトランザクションに適用する（`suggest` なら提案マーク付き）。
///
/// 見つからない見出し等は skip として記録し、適用可能な操作は進める（部分適用）。
pub fn apply_ops(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    meta: &yrs::MapRef,
    ops: &[EditOp],
    mode: EditMode,
) -> EditReport {
    let mut report = EditReport::default();
    for op in ops {
        let ok = apply_one(txn, fragment, meta, op, mode);
        if ok {
            report.applied += 1;
        } else {
            report.skipped.push(describe(op));
        }
    }
    report
}

fn apply_one(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    meta: &MapRef,
    op: &EditOp,
    mode: EditMode,
) -> bool {
    match op {
        EditOp::Append { markdown } => {
            let blocks = parse_markdown(markdown);
            let at = fragment.len(txn);
            insert_blocks(txn, fragment, at, &blocks, mode);
            true
        }
        EditOp::ReplaceAll { markdown } => {
            let blocks = parse_markdown(markdown);
            let len = fragment.len(txn);
            if len > 0 {
                fragment.remove_range(txn, 0, len);
            }
            insert_blocks(txn, fragment, 0, &blocks, mode);
            true
        }
        EditOp::InsertAfterHeading { heading, markdown } => {
            match find_heading(txn, fragment, heading) {
                Some(idx) => {
                    let blocks = parse_markdown(markdown);
                    insert_blocks(txn, fragment, idx + 1, &blocks, mode);
                    true
                }
                None => false,
            }
        }
        EditOp::ReplaceSection { heading, markdown } => {
            match find_heading(txn, fragment, heading) {
                Some(idx) => {
                    let level = heading_level(txn, fragment, idx);
                    let end = section_end(txn, fragment, idx, level);
                    // 見出しの次〜節末を削除して置換内容を挿入する（見出し自体は残す）。
                    let body_start = idx + 1;
                    if end > body_start {
                        fragment.remove_range(txn, body_start, end - body_start);
                    }
                    let blocks = parse_markdown(markdown);
                    insert_blocks(txn, fragment, body_start, &blocks, mode);
                    true
                }
                None => false,
            }
        }
        EditOp::SetMeta { key, value } => {
            write_meta_pair(txn, meta, key, value);
            true
        }
    }
}

/// ブロック列を `at` に挿入する（direct: そのまま／suggest: 提案マーク付与）。
fn insert_blocks(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    at: u32,
    blocks: &[Block],
    mode: EditMode,
) {
    yjs_map::insert_blocks_at(txn, fragment, at, blocks, mode == EditMode::Suggest);
}

/// テキスト一致（trim・大小無視）で最初の見出し要素の index を返す。
fn find_heading(txn: &TransactionMut<'_>, fragment: &XmlFragmentRef, heading: &str) -> Option<u32> {
    let target = heading.trim().to_lowercase();
    for (idx, child) in fragment.children(txn).enumerate() {
        if let XmlOut::Element(el) = child {
            if &**el.tag() == "heading" && element_text(txn, &el).trim().to_lowercase() == target {
                return Some(idx as u32);
            }
        }
    }
    None
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // level は 1..=6 に有界。
fn heading_level(txn: &TransactionMut<'_>, fragment: &XmlFragmentRef, idx: u32) -> u8 {
    fragment
        .get(txn, idx)
        .and_then(|node| match node {
            XmlOut::Element(el) => match el.get_attribute(txn, "level") {
                Some(yrs::Out::Any(yrs::Any::BigInt(n))) => Some(n as u8),
                Some(yrs::Out::Any(yrs::Any::Number(n))) => Some(n as u8),
                _ => Some(1),
            },
            _ => None,
        })
        .unwrap_or(1)
}

/// 節の終端（次の同/上位レベル見出し、または文書末尾）を返す。
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // level は 1..=6 に有界。
fn section_end(txn: &TransactionMut<'_>, fragment: &XmlFragmentRef, start: u32, level: u8) -> u32 {
    let total = fragment.len(txn);
    let mut i = start + 1;
    while i < total {
        if let Some(XmlOut::Element(el)) = fragment.get(txn, i) {
            if &**el.tag() == "heading" {
                let l = match el.get_attribute(txn, "level") {
                    Some(yrs::Out::Any(yrs::Any::BigInt(n))) => n as u8,
                    Some(yrs::Out::Any(yrs::Any::Number(n))) => n as u8,
                    _ => 1,
                };
                if l <= level {
                    return i;
                }
            }
        }
        i += 1;
    }
    total
}

fn element_text(txn: &TransactionMut<'_>, el: &XmlElementRef) -> String {
    let mut out = String::new();
    for child in el.children(txn) {
        match child {
            XmlOut::Text(t) => out.push_str(&t.get_string(txn)),
            XmlOut::Element(e) => out.push_str(&element_text(txn, &e)),
            XmlOut::Fragment(_) => {}
        }
    }
    out
}

fn describe(op: &EditOp) -> String {
    match op {
        EditOp::Append { .. } => "append".into(),
        EditOp::ReplaceAll { .. } => "replace_all".into(),
        EditOp::InsertAfterHeading { heading, .. } => {
            format!("insert_after_heading（見出し「{heading}」が見つからない）")
        }
        EditOp::ReplaceSection { heading, .. } => {
            format!("replace_section（見出し「{heading}」が見つからない）")
        }
        EditOp::SetMeta { key, .. } => format!("set_meta（{key}）"),
    }
}
