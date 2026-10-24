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

/// 以前の書き込みが BigInt で残した数値の属性（見出しの level・番号付きリストの start）を
/// Number に直す（#886）。直した要素の数を返す（0 なら何も書かない）。
pub fn repair_number_attrs<F: XmlFragment>(txn: &mut TransactionMut<'_>, parent: &F) -> usize {
    let children: Vec<XmlOut> = parent.children(txn).collect();
    let mut fixed = 0;
    for child in children {
        let XmlOut::Element(el) = child else {
            continue;
        };
        let key = match &**el.tag() {
            "heading" => Some("level"),
            "orderedList" => Some("start"),
            _ => None,
        };
        if let Some(key) = key {
            if let Some(yrs::Out::Any(Any::BigInt(n))) = el.get_attribute(txn, key) {
                #[allow(clippy::cast_precision_loss)] // 見出しの段・リストの開始番号は小さい。
                el.insert_attribute(txn, key, Any::Number(n as f64));
                fixed += 1;
            }
        }
        fixed += repair_number_attrs(txn, &el);
    }
    fixed
}

/// 要素配下の全テキストに `aiSuggestion` フォーマット属性を付ける（提案マーク）。
fn mark_suggestion(txn: &mut TransactionMut<'_>, el: &XmlElementRef) {
    mark_all(txn, el, "aiSuggestion");
}

/// 要素配下の全テキストに `aiDeletion`（削除の提案）を付ける（#886）。人が承認すると消え、
/// 棄却すると印だけが外れて元に戻る。
pub fn mark_deleted(txn: &mut TransactionMut<'_>, el: &XmlElementRef) {
    mark_all(txn, el, "aiDeletion");
}

fn mark_all(txn: &mut TransactionMut<'_>, el: &XmlElementRef, key: &str) {
    // 文字を持たない要素（区切り線・埋め込み・空の段落）と、文字が書式を持てないコードブロックは
    // 要素の属性に付ける（エディタはノードの属性として読み、承認/棄却で同じように扱う）。
    let tag = el.tag().to_string();
    if tag == "codeBlock" || !has_text(txn, el) {
        el.insert_attribute(txn, key, Any::Bool(true));
        return;
    }
    // 表は丸ごとの提案であることを要素の属性で示す（セルの文字だけへの提案と区別する）。
    // 中の文字にも印を付けて、取り消し線などで見えるようにする。
    if tag == "table" {
        el.insert_attribute(txn, key, Any::Bool(true));
    }
    // 子を先に収集してから編集する（走査中の変更を避ける）。
    let children: Vec<XmlOut> = el.children(txn).collect();
    for child in children {
        match child {
            XmlOut::Text(text) => {
                let len = text.len(txn);
                if len > 0 {
                    let mut attrs: Attrs = HashMap::new();
                    attrs.insert(Arc::from(key), Any::Bool(true));
                    text.format(txn, 0, len, attrs);
                }
            }
            XmlOut::Element(inner) => mark_all(txn, &inner, key),
            XmlOut::Fragment(_) => {}
        }
    }
}

/// 要素の中に 1 文字でも文字があるか。
fn has_text(txn: &TransactionMut<'_>, el: &XmlElementRef) -> bool {
    el.children(txn).any(|c| match c {
        XmlOut::Text(t) => t.len(txn) > 0,
        XmlOut::Element(e) => has_text(txn, &e),
        XmlOut::Fragment(_) => false,
    })
}

/// リストのブロック（同じ種類のリスト）の項目を、既存のリスト `list` の `at` 番目へ入れる
/// （#886・`insert_after` でリストの途中に項目を足す）。
pub fn insert_list_items_at(
    txn: &mut TransactionMut<'_>,
    list: &XmlElementRef,
    at: u32,
    block: &Block,
    suggest: bool,
) {
    let at = at.min(list.len(txn));
    let count = match block {
        Block::BulletList(items) | Block::OrderedList { items, .. } => {
            for (i, item) in items.iter().enumerate() {
                let item_el = list.insert(txn, at + i as u32, XmlElementPrelim::empty("listItem"));
                for (j, b) in item.iter().enumerate() {
                    append_block(txn, &item_el, j as u32, b);
                }
            }
            items.len()
        }
        Block::TaskList(items) => {
            for (i, item) in items.iter().enumerate() {
                let item_el = list.insert(txn, at + i as u32, XmlElementPrelim::empty("taskItem"));
                item_el.insert_attribute(txn, "checked", Any::Bool(item.checked));
                for (j, b) in item.content.iter().enumerate() {
                    append_block(txn, &item_el, j as u32, b);
                }
            }
            items.len()
        }
        _ => 0,
    };
    if suggest {
        for i in 0..count as u32 {
            if let Some(XmlOut::Element(el)) = list.get(txn, at + i) {
                mark_suggestion(txn, &el);
            }
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
            // 数値の属性は JS の number で読まれる形（Number）で書く。BigInt だとエディタの
            // 見出しが `levels.includes(2n)` に外れて全部 H1 で描かれる（#886）。
            el.insert_attribute(txn, "level", Any::Number(f64::from(*level)));
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
                #[allow(clippy::cast_precision_loss)] // リストの開始番号は 2^52 より十分小さい。
                el.insert_attribute(txn, "start", Any::Number(*start as f64));
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
