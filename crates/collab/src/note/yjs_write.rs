//! ノート AST → Yjs（XmlFragment 構築）（Task 11P.2/11P.4）。
//!
//! [`super::yjs_map`] の読み取り（Yjs → AST）と対。書き込み（全置換・部分挿入・AI 提案
//! マーク付与）をここに集約する。

use std::collections::HashMap;
use std::sync::Arc;

use yrs::types::Attrs;
use yrs::{
    Any, Text, TransactionMut, Xml, XmlElementPrelim, XmlElementRef, XmlFragment, XmlFragmentRef,
    XmlOut, XmlTextPrelim,
};

use super::ast::{Block, Inline, Marks, Table};

// ---------------------------------------------------------------------------
// AST → Yjs
// ---------------------------------------------------------------------------

/// フラグメント内容を AST で**全置換**する（インポート経路）。
pub fn write_blocks(txn: &mut TransactionMut<'_>, fragment: &XmlFragmentRef, blocks: &[Block]) {
    let len = fragment.len(txn);
    if len > 0 {
        fragment.remove_range(txn, 0, len);
    }
    for (i, block) in blocks.iter().enumerate() {
        append_block(txn, fragment, i as u32, block);
    }
}

/// ブロック列を `at` へ挿入する（AI 編集・Task 11P.4）。
///
/// `suggest=true` なら挿入した全ブロックのテキストランに `aiSuggestion` マークを付ける
/// （エディタが承認/棄却 UI を出す・md には落とさず Yjs snapshot 側に保つ）。
pub fn insert_blocks_at(
    txn: &mut TransactionMut<'_>,
    fragment: &XmlFragmentRef,
    at: u32,
    blocks: &[Block],
    suggest: bool,
) {
    let clamped = at.min(fragment.len(txn));
    for (i, block) in blocks.iter().enumerate() {
        let index = clamped + i as u32;
        append_block(txn, fragment, index, block);
        if suggest {
            if let Some(XmlOut::Element(el)) = fragment.get(txn, index) {
                mark_suggestion(txn, &el);
            }
        }
    }
}

/// 要素配下の全テキストに `aiSuggestion` フォーマット属性を付ける（提案マーク）。
fn mark_suggestion(txn: &mut TransactionMut<'_>, el: &XmlElementRef) {
    // 子を先に収集してから編集する（走査中の変更を避ける）。
    let children: Vec<XmlOut> = el.children(txn).collect();
    for child in children {
        match child {
            XmlOut::Text(text) => {
                let len = text.len(txn);
                if len > 0 {
                    let mut attrs: Attrs = HashMap::new();
                    attrs.insert(Arc::from("aiSuggestion"), Any::Bool(true));
                    text.format(txn, 0, len, attrs);
                }
            }
            XmlOut::Element(inner) => mark_suggestion(txn, &inner),
            XmlOut::Fragment(_) => {}
        }
    }
}

/// `parent` の `index` 位置へ 1 ブロックを構築する。
fn append_block<F: XmlFragment>(
    txn: &mut TransactionMut<'_>,
    parent: &F,
    index: u32,
    block: &Block,
) {
    match block {
        Block::Paragraph(inlines) => {
            let el = parent.insert(txn, index, XmlElementPrelim::empty("paragraph"));
            write_inlines(txn, &el, inlines);
        }
        Block::Heading { level, content } => {
            let el = parent.insert(txn, index, XmlElementPrelim::empty("heading"));
            el.insert_attribute(txn, "level", Any::BigInt(i64::from(*level)));
            write_inlines(txn, &el, content);
        }
        Block::CodeBlock { language, code } => {
            let el = parent.insert(txn, index, XmlElementPrelim::empty("codeBlock"));
            if !language.is_empty() {
                el.insert_attribute(txn, "language", Any::from(language.as_str()));
            }
            el.insert(txn, 0, XmlTextPrelim::new(code.as_str()));
        }
        Block::Embed { payload } => {
            let el = parent.insert(txn, index, XmlElementPrelim::empty("shikiEmbed"));
            el.insert_attribute(txn, "payload", Any::from(payload.as_str()));
        }
        Block::Blockquote(blocks) => {
            let el = parent.insert(txn, index, XmlElementPrelim::empty("blockquote"));
            for (i, b) in blocks.iter().enumerate() {
                append_block(txn, &el, i as u32, b);
            }
        }
        Block::BulletList(items) => {
            let el = parent.insert(txn, index, XmlElementPrelim::empty("bulletList"));
            write_list_items(txn, &el, items, "listItem");
        }
        Block::OrderedList { start, items } => {
            let el = parent.insert(txn, index, XmlElementPrelim::empty("orderedList"));
            if *start != 1 {
                el.insert_attribute(txn, "start", Any::BigInt(*start as i64));
            }
            write_list_items(txn, &el, items, "listItem");
        }
        Block::TaskList(items) => {
            let el = parent.insert(txn, index, XmlElementPrelim::empty("taskList"));
            for (i, item) in items.iter().enumerate() {
                let item_el = el.insert(txn, i as u32, XmlElementPrelim::empty("taskItem"));
                item_el.insert_attribute(txn, "checked", Any::Bool(item.checked));
                for (j, b) in item.content.iter().enumerate() {
                    append_block(txn, &item_el, j as u32, b);
                }
            }
        }
        Block::Table(table) => write_table(txn, parent, index, table),
        Block::HorizontalRule => {
            parent.insert(txn, index, XmlElementPrelim::empty("horizontalRule"));
        }
    }
}

fn write_list_items(
    txn: &mut TransactionMut<'_>,
    el: &XmlElementRef,
    items: &[Vec<Block>],
    item_tag: &str,
) {
    for (i, item) in items.iter().enumerate() {
        let item_el = el.insert(txn, i as u32, XmlElementPrelim::empty(item_tag.to_string()));
        for (j, b) in item.iter().enumerate() {
            append_block(txn, &item_el, j as u32, b);
        }
    }
}

fn write_table<F: XmlFragment>(
    txn: &mut TransactionMut<'_>,
    parent: &F,
    index: u32,
    table: &Table,
) {
    let table_el = parent.insert(txn, index, XmlElementPrelim::empty("table"));
    let mut row_idx = 0u32;
    if !table.header.is_empty() {
        let row_el = table_el.insert(txn, row_idx, XmlElementPrelim::empty("tableRow"));
        for (i, cell) in table.header.iter().enumerate() {
            write_table_cell(txn, &row_el, i as u32, "tableHeader", cell);
        }
        row_idx += 1;
    }
    for row in &table.rows {
        let row_el = table_el.insert(txn, row_idx, XmlElementPrelim::empty("tableRow"));
        for (i, cell) in row.iter().enumerate() {
            write_table_cell(txn, &row_el, i as u32, "tableCell", cell);
        }
        row_idx += 1;
    }
}

fn write_table_cell(
    txn: &mut TransactionMut<'_>,
    row_el: &XmlElementRef,
    index: u32,
    tag: &str,
    inlines: &[Inline],
) {
    let cell_el = row_el.insert(txn, index, XmlElementPrelim::empty(tag.to_string()));
    let para = cell_el.insert(txn, 0, XmlElementPrelim::empty("paragraph"));
    write_inlines(txn, &para, inlines);
}

/// インライン列を要素へ書く（Text はランごとにマーク付き挿入・HardBreak は要素）。
///
/// Yjs の formatting attribute は**後続挿入に継承される**ため、直前ランで有効だった
/// マークが現ランで無効なら `Any::Null` で明示的にクリアする（マーク漏れの防止）。
fn write_inlines(txn: &mut TransactionMut<'_>, el: &XmlElementRef, inlines: &[Inline]) {
    let mut child_idx: u32 = el.len(txn);
    let mut current_text: Option<yrs::XmlTextRef> = None;
    let mut prev_marks = Marks::default();
    for inline in inlines {
        match inline {
            Inline::Text { text, marks } => {
                let text_ref = if let Some(t) = &current_text {
                    t.clone()
                } else {
                    let t = el.insert(txn, child_idx, XmlTextPrelim::new(""));
                    child_idx += 1;
                    prev_marks = Marks::default();
                    current_text = Some(t.clone());
                    t
                };
                let offset = text_ref.len(txn);
                let attrs = attrs_delta(&prev_marks, marks);
                if attrs.is_empty() {
                    text_ref.insert(txn, offset, text);
                } else {
                    text_ref.insert_with_attributes(txn, offset, text, attrs);
                }
                prev_marks = marks.clone();
            }
            Inline::HardBreak => {
                el.insert(txn, child_idx, XmlElementPrelim::empty("hardBreak"));
                child_idx += 1;
                current_text = None;
            }
        }
    }
}

/// 現ランの formatting attributes（直前ランで有効だったマークの明示クリア込み）。
fn attrs_delta(prev: &Marks, cur: &Marks) -> Attrs {
    let mut attrs = attrs_from_marks(cur);
    let cleared: [(&str, bool); 5] = [
        ("bold", prev.bold && !cur.bold),
        ("italic", prev.italic && !cur.italic),
        ("strike", prev.strike && !cur.strike),
        ("code", prev.code && !cur.code),
        ("link", prev.link.is_some() && cur.link.is_none()),
    ];
    for (key, should_clear) in cleared {
        if should_clear {
            attrs.insert(Arc::from(key), Any::Null);
        }
    }
    attrs
}

fn attrs_from_marks(marks: &Marks) -> Attrs {
    let mut attrs: Attrs = HashMap::new();
    let empty = Any::from(HashMap::<String, Any>::new());
    if marks.bold {
        attrs.insert(Arc::from("bold"), empty.clone());
    }
    if marks.italic {
        attrs.insert(Arc::from("italic"), empty.clone());
    }
    if marks.strike {
        attrs.insert(Arc::from("strike"), empty.clone());
    }
    if marks.code {
        attrs.insert(Arc::from("code"), empty.clone());
    }
    if let Some(href) = &marks.link {
        let mut link: HashMap<String, Any> = HashMap::new();
        link.insert("href".into(), Any::from(href.as_str()));
        attrs.insert(Arc::from("link"), Any::from(link));
    }
    attrs
}
