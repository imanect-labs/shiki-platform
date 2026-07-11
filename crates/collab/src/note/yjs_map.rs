//! ノート AST ⇔ Yjs（XmlFragment / Map "meta"）の相互変換（Task 11P.2）。
//!
//! Yjs 側の構造は y-prosemirror / TipTap の慣例に合わせる:
//! - 本文はフラグメント **"default"**（TipTap Collaboration の既定 field）。
//! - ブロックノードは `XmlElement`（tag = PM ノード名: paragraph / heading / bulletList /
//!   orderedList / listItem / taskList / taskItem / codeBlock / blockquote / table /
//!   tableRow / tableHeader / tableCell / horizontalRule / shikiEmbed / hardBreak）。
//! - インラインは `XmlText`。マークは formatting attributes（キー = マーク名、
//!   値 = マーク attrs のマップ。link は `{href}`）。
//! - メタデータは Map **"meta"**（title/icon/thread_id: 文字列、tags: 文字列配列、
//!   その他キー: 文字列）。

use std::collections::HashMap;
use std::sync::Arc;

use yrs::types::text::YChange;
use yrs::types::Attrs;
use yrs::{
    Any, GetString, Out, ReadTxn, Text, TransactionMut, Xml, XmlElementPrelim, XmlElementRef,
    XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim,
};

use super::ast::{Block, Inline, Marks, Table, TaskItem};

/// TipTap Collaboration 既定の本文フラグメント名。
pub const FRAGMENT_NAME: &str = "default";
/// メタデータ Map 名（本プラットフォームの規約）。
pub const META_MAP_NAME: &str = "meta";

// ---------------------------------------------------------------------------
// Yjs → AST
// ---------------------------------------------------------------------------

/// フラグメント直下のブロック列を読む。
pub fn read_blocks<T: ReadTxn>(txn: &T, fragment: &XmlFragmentRef) -> Vec<Block> {
    children_to_blocks(txn, fragment.children(txn).collect())
}

fn children_to_blocks<T: ReadTxn>(txn: &T, children: Vec<XmlOut>) -> Vec<Block> {
    let mut blocks = Vec::new();
    for child in children {
        match child {
            XmlOut::Element(el) => blocks.push(read_block(txn, &el)),
            // ブロック位置に裸の XmlText が来た場合は段落として救済する。
            XmlOut::Text(t) => {
                let inlines = text_to_inlines(txn, &t);
                if !inlines.is_empty() {
                    blocks.push(Block::Paragraph(inlines));
                }
            }
            XmlOut::Fragment(_) => {}
        }
    }
    blocks
}

fn read_block<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> Block {
    let tag: &str = el.tag();
    match tag {
        "heading" => Block::Heading {
            level: attr_u8(txn, el, "level").unwrap_or(1).clamp(1, 6),
            content: read_inlines(txn, el),
        },
        "bulletList" => Block::BulletList(read_list_items(txn, el)),
        "orderedList" => Block::OrderedList {
            start: attr_u64(txn, el, "start").unwrap_or(1),
            items: read_list_items(txn, el),
        },
        "taskList" => Block::TaskList(read_task_items(txn, el)),
        "codeBlock" => Block::CodeBlock {
            language: attr_string(txn, el, "language").unwrap_or_default(),
            code: plain_text_of(txn, el),
        },
        "shikiEmbed" => Block::Embed {
            payload: attr_string(txn, el, "payload").unwrap_or_default(),
        },
        "blockquote" => Block::Blockquote(children_to_blocks(txn, el.children(txn).collect())),
        "table" => Block::Table(read_table(txn, el)),
        "horizontalRule" => Block::HorizontalRule,
        // paragraph・未知ブロックはインライン内容を段落として読む（情報を落とさない）。
        _ => Block::Paragraph(read_inlines(txn, el)),
    }
}

fn read_list_items<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> Vec<Vec<Block>> {
    el.children(txn)
        .filter_map(|c| match c {
            XmlOut::Element(item) => Some(children_to_blocks(txn, item.children(txn).collect())),
            _ => None,
        })
        .collect()
}

fn read_task_items<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> Vec<TaskItem> {
    el.children(txn)
        .filter_map(|c| match c {
            XmlOut::Element(item) => Some(TaskItem {
                checked: attr_bool(txn, &item, "checked").unwrap_or(false),
                content: children_to_blocks(txn, item.children(txn).collect()),
            }),
            _ => None,
        })
        .collect()
}

fn read_table<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> Table {
    let mut header: Vec<Vec<Inline>> = Vec::new();
    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
    for row in el.children(txn) {
        let XmlOut::Element(row_el) = row else {
            continue;
        };
        let mut cells: Vec<Vec<Inline>> = Vec::new();
        let mut is_header_row = false;
        for cell in row_el.children(txn) {
            let XmlOut::Element(cell_el) = cell else {
                continue;
            };
            if &**cell_el.tag() == "tableHeader" {
                is_header_row = true;
            }
            cells.push(cell_inlines(txn, &cell_el));
        }
        if is_header_row && header.is_empty() {
            header = cells;
        } else {
            rows.push(cells);
        }
    }
    // GFM はヘッダ必須: ヘッダ行が無ければ先頭行を昇格する。
    if header.is_empty() && !rows.is_empty() {
        header = rows.remove(0);
    }
    Table { header, rows }
}

/// セル内容（tableCell > paragraph > inline）をフラットなインライン列へ。
fn cell_inlines<T: ReadTxn>(txn: &T, cell: &XmlElementRef) -> Vec<Inline> {
    let mut inlines = Vec::new();
    for child in cell.children(txn) {
        match child {
            XmlOut::Element(el) => inlines.extend(read_inlines(txn, &el)),
            XmlOut::Text(t) => inlines.extend(text_to_inlines(txn, &t)),
            XmlOut::Fragment(_) => {}
        }
    }
    inlines
}

/// 要素直下のインライン列（XmlText のマーク付きラン＋ hardBreak 等）。
fn read_inlines<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> Vec<Inline> {
    let mut inlines = Vec::new();
    for child in el.children(txn) {
        match child {
            XmlOut::Text(t) => inlines.extend(text_to_inlines(txn, &t)),
            XmlOut::Element(inline_el) => {
                if &**inline_el.tag() == "hardBreak" {
                    inlines.push(Inline::HardBreak);
                } else {
                    // 未知のインライン要素はテキスト内容で保全する。
                    let text = plain_text_of(txn, &inline_el);
                    if !text.is_empty() {
                        inlines.push(Inline::Text {
                            text,
                            marks: Marks::default(),
                        });
                    }
                }
            }
            XmlOut::Fragment(_) => {}
        }
    }
    inlines
}

fn text_to_inlines<T: ReadTxn>(txn: &T, text: &yrs::XmlTextRef) -> Vec<Inline> {
    text.diff(txn, YChange::identity)
        .into_iter()
        .filter_map(|diff| {
            let Out::Any(Any::String(s)) = diff.insert else {
                return None;
            };
            let marks = diff
                .attributes
                .map(|attrs| marks_from_attrs(&attrs))
                .unwrap_or_default();
            Some(Inline::Text {
                text: s.to_string(),
                marks,
            })
        })
        .collect()
}

fn marks_from_attrs(attrs: &Attrs) -> Marks {
    let mut marks = Marks::default();
    for (key, value) in attrs {
        if matches!(value, Any::Null) {
            continue;
        }
        match &**key {
            "bold" | "strong" => marks.bold = true,
            "italic" | "em" => marks.italic = true,
            "strike" | "strikethrough" => marks.strike = true,
            "code" => marks.code = true,
            "link" => {
                marks.link = match value {
                    Any::Map(map) => map.get("href").and_then(|v| match v {
                        Any::String(s) => Some(s.to_string()),
                        _ => None,
                    }),
                    Any::String(s) => Some(s.to_string()),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    marks
}

/// 要素配下の素のテキスト連結（codeBlock 等・マーク無視）。
fn plain_text_of<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> String {
    let mut out = String::new();
    for child in el.children(txn) {
        match child {
            XmlOut::Text(t) => out.push_str(&t.get_string(txn)),
            XmlOut::Element(e) => out.push_str(&plain_text_of(txn, &e)),
            XmlOut::Fragment(_) => {}
        }
    }
    out
}

// --- 属性読みの寛容ヘルパ（JS 側は number/string どちらもあり得る） ---

fn attr_any<T: ReadTxn>(txn: &T, el: &XmlElementRef, name: &str) -> Option<Any> {
    match el.get_attribute(txn, name)? {
        Out::Any(any) => Some(any),
        _ => None,
    }
}

fn attr_string<T: ReadTxn>(txn: &T, el: &XmlElementRef, name: &str) -> Option<String> {
    match attr_any(txn, el, name)? {
        Any::String(s) => Some(s.to_string()),
        Any::Number(n) => Some(n.to_string()),
        Any::BigInt(n) => Some(n.to_string()),
        Any::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[allow(clippy::cast_sign_loss)] // ガード付き（負値は None）。
fn attr_u64<T: ReadTxn>(txn: &T, el: &XmlElementRef, name: &str) -> Option<u64> {
    match attr_any(txn, el, name)? {
        Any::Number(n) if n >= 0.0 => Some(n as u64),
        Any::BigInt(n) if n >= 0 => Some(n as u64),
        Any::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn attr_u8<T: ReadTxn>(txn: &T, el: &XmlElementRef, name: &str) -> Option<u8> {
    attr_u64(txn, el, name).map(|v| v.min(u64::from(u8::MAX)) as u8)
}

fn attr_bool<T: ReadTxn>(txn: &T, el: &XmlElementRef, name: &str) -> Option<bool> {
    match attr_any(txn, el, name)? {
        Any::Bool(b) => Some(b),
        Any::String(s) => Some(&*s == "true"),
        _ => None,
    }
}

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
