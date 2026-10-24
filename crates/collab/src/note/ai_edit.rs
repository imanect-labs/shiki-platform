//! AI 共同編集の編集オペレーション（Task 11P.4・design §4.8.1）。
//!
//! エージェントの `document.edit` は**共同編集参加者**として、人間と同じ Yjs ドキュメント
//! に変更を適用する（別コピーを作らない）。CRDT の収束保証は Yjs が担うため、人間の
//! 並行編集とも自然に収束する。編集は md アンカー指定のブロック操作で表現する:
//!
//! - `Append`: 末尾にブロックを追加（非破壊・最も安全）
//! - `ReplaceSection`: 見出しテキスト一致の節（見出し〜次の同/上位見出しの手前）を置換
//! - `InsertAfterHeading`: 見出し直後にブロックを挿入
//! - `DeleteSection` / `MoveSection`: 節を見出しごと消す・別の見出しの前へ移す（#886）
//! - `ReplaceAll`: 文書全体を置換（最終手段）
//!
//! 本文の語句で場所を決める細かい操作（`ReplaceText` 等）は [`super::ai_text`]。
//!
//! **既定は直接適用**（AI 名義・Yjs undo 可）。サジェストモードは挿入テキストに
//! `aiSuggestion`、消す側のテキストに `aiDeletion` の印を付け、エディタが承認/棄却 UI を
//! 出す（印は md に落とさず Yjs snapshot 側の正本に保つ・PIT-37③）。消す側も印に留めるので、
//! 棄却すれば元の本文が残る。

use serde::Deserialize;
use yrs::{
    GetString, MapRef, TransactionMut, Xml, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut,
};

use super::ai_text;
use super::ast::Block;
use super::md_parse::parse_markdown;
use super::yjs_map;
use super::yjs_meta::write_meta_pair;
use super::yjs_write::mark_deleted;

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
    /// `heading` の節（見出しを含む）を消す。
    DeleteSection { heading: String },
    /// `heading` の節（見出しを含む）を `before_heading` の見出しの前へ移す（無ければ末尾へ）。
    MoveSection {
        heading: String,
        #[serde(default)]
        before_heading: Option<String>,
    },
    /// 1 ブロック内の語句 `find` を `replace` に差し替える（`heading` で節を絞れる）。
    ReplaceText {
        find: String,
        replace: String,
        #[serde(default)]
        heading: Option<String>,
    },
    /// 語句 `find` を含む最上位のブロックを markdown で置き換える。
    ReplaceBlock {
        find: String,
        markdown: String,
        #[serde(default)]
        heading: Option<String>,
    },
    /// 語句 `find` を含むブロックの直後へ markdown を入れる。
    InsertAfter {
        find: String,
        markdown: String,
        #[serde(default)]
        heading: Option<String>,
    },
    /// 文書全体を markdown で置換する（最終手段）。
    ReplaceAll { markdown: String },
    /// メタデータ（frontmatter 型属性）を 1 件設定する（tags は `,` 区切り可）。
    SetMeta { key: String, value: String },
}

/// 操作の適用結果サマリ（ツールが観測テキストに載せる）。
#[derive(Debug, Default)]
pub struct EditReport {
    pub applied: usize,
    /// 適用できなかった操作（「何番目の操作・何を・なぜ」の説明）。
    pub skipped: Vec<String>,
}

/// 編集オペ列をトランザクションに適用する（`suggest` なら提案マーク付き）。
///
/// 見つからない見出し・一意でない語句等は理由付きで skip として記録し、適用可能な操作は
/// 進める（部分適用）。
pub fn apply_ops(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    meta: &yrs::MapRef,
    ops: &[EditOp],
    mode: EditMode,
) -> EditReport {
    let mut report = EditReport::default();
    for (i, op) in ops.iter().enumerate() {
        match apply_one(txn, fragment, meta, op, mode) {
            Ok(()) => report.applied += 1,
            Err(why) => report
                .skipped
                .push(format!("{} 番目の {}: {why}", i + 1, op.name())),
        }
    }
    report
}

impl EditOp {
    /// 操作名（ツールの入力の `op`）。
    pub fn name(&self) -> &'static str {
        match self {
            EditOp::Append { .. } => "append",
            EditOp::ReplaceSection { .. } => "replace_section",
            EditOp::InsertAfterHeading { .. } => "insert_after_heading",
            EditOp::DeleteSection { .. } => "delete_section",
            EditOp::MoveSection { .. } => "move_section",
            EditOp::ReplaceText { .. } => "replace_text",
            EditOp::ReplaceBlock { .. } => "replace_block",
            EditOp::InsertAfter { .. } => "insert_after",
            EditOp::ReplaceAll { .. } => "replace_all",
            EditOp::SetMeta { .. } => "set_meta",
        }
    }
}

/// 任意の見出し指定（厳格なスキーマのモデルは空文字で埋めてくる）。
fn scope(heading: Option<&String>) -> Option<&str> {
    heading.map(|h| h.trim()).filter(|h| !h.is_empty())
}

fn heading_index(
    txn: &TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    heading: &str,
) -> Result<u32, String> {
    find_heading(txn, fragment, heading).ok_or_else(|| format!("見出し「{heading}」が見つからない"))
}

/// フラグメント直下の `start..end` を消す（提案モードは削除の提案の印を付けるだけ）。
fn remove_blocks(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    start: u32,
    end: u32,
    mode: EditMode,
) {
    if end <= start {
        return;
    }
    if mode == EditMode::Suggest {
        for i in start..end {
            if let Some(XmlOut::Element(el)) = fragment.get(txn, i) {
                mark_deleted(txn, &el);
            }
        }
    } else {
        fragment.remove_range(txn, start, end - start);
    }
}

fn apply_one(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    meta: &MapRef,
    op: &EditOp,
    mode: EditMode,
) -> Result<(), String> {
    match op {
        EditOp::Append { markdown } => {
            let blocks = parse_markdown(markdown);
            let at = fragment.len(txn);
            insert_blocks(txn, fragment, at, &blocks, mode);
        }
        EditOp::ReplaceAll { markdown } => {
            let blocks = parse_markdown(markdown);
            let len = fragment.len(txn);
            remove_blocks(txn, fragment, 0, len, mode);
            // 提案モードは元の本文を残すので、その後ろへ入れる。
            let at = fragment.len(txn);
            insert_blocks(txn, fragment, at, &blocks, mode);
        }
        EditOp::InsertAfterHeading { heading, markdown } => {
            let idx = heading_index(txn, fragment, heading)?;
            let blocks = parse_markdown(markdown);
            insert_blocks(txn, fragment, idx + 1, &blocks, mode);
        }
        EditOp::ReplaceSection { heading, markdown } => {
            let idx = heading_index(txn, fragment, heading)?;
            let level = heading_level(txn, fragment, idx);
            let end = section_end(txn, fragment, idx, level);
            // 見出しの次〜節末を消して置換内容を入れる（見出し自体は残す）。
            remove_blocks(txn, fragment, idx + 1, end, mode);
            let at = if mode == EditMode::Suggest {
                end
            } else {
                idx + 1
            };
            let blocks = parse_markdown(markdown);
            insert_blocks(txn, fragment, at, &blocks, mode);
        }
        EditOp::DeleteSection { heading } => {
            let idx = heading_index(txn, fragment, heading)?;
            let level = heading_level(txn, fragment, idx);
            let end = section_end(txn, fragment, idx, level);
            remove_blocks(txn, fragment, idx, end, mode);
        }
        EditOp::MoveSection {
            heading,
            before_heading,
        } => move_section(txn, fragment, heading, scope(before_heading.as_ref()), mode)?,
        EditOp::ReplaceText {
            find,
            replace,
            heading,
        } => ai_text::replace_text(txn, fragment, find, replace, scope(heading.as_ref()), mode)?,
        EditOp::ReplaceBlock {
            find,
            markdown,
            heading,
        } => ai_text::replace_block(txn, fragment, find, markdown, scope(heading.as_ref()), mode)?,
        EditOp::InsertAfter {
            find,
            markdown,
            heading,
        } => ai_text::insert_after(txn, fragment, find, markdown, scope(heading.as_ref()), mode)?,
        EditOp::SetMeta { key, value } => write_meta_pair(txn, meta, key, value),
    }
    Ok(())
}

/// 節（見出しを含む）を `before` の見出しの前へ移す（無ければ末尾へ）。
///
/// Yjs に移動は無いので、読んで消して入れ直す。提案モードは元の節に削除の提案の印を付け、
/// 移動先へ写しを提案として入れる。
fn move_section(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    heading: &str,
    before: Option<&str>,
    mode: EditMode,
) -> Result<(), String> {
    let idx = heading_index(txn, fragment, heading)?;
    let level = heading_level(txn, fragment, idx);
    let end = section_end(txn, fragment, idx, level);
    let target = match before {
        Some(b) => heading_index(txn, fragment, b)?,
        None => fragment.len(txn),
    };
    if target == idx || target == end {
        return Ok(());
    }
    if idx < target && target < end {
        return Err(format!("移動先の見出しが「{heading}」の節の中にある"));
    }
    let blocks = yjs_map::read_block_range(txn, fragment, idx, end);
    if mode == EditMode::Suggest {
        remove_blocks(txn, fragment, idx, end, mode);
        insert_blocks(txn, fragment, target, &blocks, mode);
    } else {
        fragment.remove_range(txn, idx, end - idx);
        let at = if target > idx {
            target - (end - idx)
        } else {
            target
        };
        insert_blocks(txn, fragment, at, &blocks, mode);
    }
    Ok(())
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
pub(super) fn find_heading(
    txn: &TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    heading: &str,
) -> Option<u32> {
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
pub(super) fn heading_level(txn: &TransactionMut<'_>, fragment: &XmlFragmentRef, idx: u32) -> u8 {
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
pub(super) fn section_end(
    txn: &TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    start: u32,
    level: u8,
) -> u32 {
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

pub(super) fn element_text(txn: &TransactionMut<'_>, el: &XmlElementRef) -> String {
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
