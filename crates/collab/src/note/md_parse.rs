//! 正規化 md → ノート AST（Task 11P.2・pulldown-cmark）。
//!
//! セキュリティ契約（Task 11P.6 / FR-8）: **生 HTML はどの流入経路でも実行可能な形に
//! しない**。ブロック HTML は `html` コードブロックへ縮退し、インライン HTML は
//! リテラル文字列として扱う（レンダリング側は常にテキスト/コード表示）。

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::ast::{Block, Inline, Marks, Table, TaskItem, EMBED_FENCE_LANG};

/// md 本文（frontmatter を除いた部分）を AST へパースする。
pub fn parse_markdown(md: &str) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(md, options);
    let mut builder = Builder::default();
    for event in parser {
        builder.event(event);
    }
    builder.finish()
}

/// 構築中のブロックコンテナ 1 段。
enum Frame {
    Blocks(Vec<Block>),
    /// リスト項目の集合（bullet/ordered/task は close 時に判定）。
    List {
        start: Option<u64>,
        items: Vec<ListItemAcc>,
    },
    /// 表（ヘッダ→行の順で埋まる）。
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        current_row: Vec<Vec<Inline>>,
    },
}

struct ListItemAcc {
    checked: Option<bool>,
    blocks: Vec<Block>,
}

/// インライン蓄積（段落・見出し・セル共通）。
#[derive(Default)]
struct InlineAcc {
    inlines: Vec<Inline>,
    marks: MarkStack,
}

#[derive(Default)]
struct MarkStack {
    bold: u32,
    italic: u32,
    strike: u32,
    link: Vec<String>,
}

impl MarkStack {
    fn current(&self, code: bool) -> Marks {
        Marks {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code,
            link: self.link.last().cloned(),
        }
    }
}

impl InlineAcc {
    fn push_text(&mut self, text: &str, code: bool) {
        if text.is_empty() {
            return;
        }
        let marks = self.marks.current(code);
        // 同一マークの連続ランは結合する（正規形を安定させる）。
        if let Some(Inline::Text {
            text: prev,
            marks: prev_marks,
        }) = self.inlines.last_mut()
        {
            if *prev_marks == marks {
                prev.push_str(text);
                return;
            }
        }
        self.inlines.push(Inline::Text {
            text: text.to_string(),
            marks,
        });
    }
}

#[derive(Default, Clone, Copy, PartialEq)]
enum InlineKind {
    #[default]
    Paragraph,
    Heading(u8),
    TableCell,
}

#[derive(Default)]
struct Builder {
    /// ブロックコンテナのスタック（底は文書ルート）。
    frames: Vec<Frame>,
    /// 構築中のインライン（Some の間はインラインコンテキスト）。
    inline: Option<InlineAcc>,
    /// インラインの行き先（段落/見出し/表セル）。
    inline_kind: InlineKind,
    /// 構築中のコードブロック（言語, 本文）。
    code: Option<(String, String)>,
    /// ブロック HTML の蓄積（`html` コードブロックへ縮退する）。
    html: Option<String>,
}

impl Builder {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&t);
                } else if let Some(html) = &mut self.html {
                    html.push_str(&t);
                } else {
                    self.ensure_inline().push_text(&t, false);
                }
            }
            Event::Code(t) => self.ensure_inline().push_text(&t, true),
            // 生 HTML はコード表示へ縮退（ブロック）・リテラル文字列化（インライン）。
            Event::Html(t) => {
                self.html.get_or_insert_default().push_str(&t);
            }
            Event::InlineHtml(t) => self.ensure_inline().push_text(&t, false),
            Event::HardBreak => {
                self.ensure_inline().inlines.push(Inline::HardBreak);
            }
            // SoftBreak（折返し）は正規形では空白 1 つ。
            Event::SoftBreak => self.ensure_inline().push_text(" ", false),
            Event::Rule => {
                self.flush_html();
                self.push_block(Block::HorizontalRule);
            }
            Event::TaskListMarker(checked) => {
                // Item 開始直後: Blocks フレームの下の List フレームの末尾項目に付く。
                let idx = self.frames.len().checked_sub(2);
                if let Some(idx) = idx {
                    if let Some(Frame::List { items, .. }) = self.frames.get_mut(idx) {
                        if let Some(item) = items.last_mut() {
                            item.checked = Some(checked);
                        }
                    }
                }
            }
            // 脚注・数式は有効化していない。
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        self.flush_html();
        match tag {
            Tag::Paragraph => {
                self.flush_loose_inline();
                self.inline = Some(InlineAcc::default());
                self.inline_kind = InlineKind::Paragraph;
            }
            Tag::Heading { level, .. } => {
                self.flush_loose_inline();
                self.inline = Some(InlineAcc::default());
                self.inline_kind = InlineKind::Heading(level as u8);
            }
            Tag::BlockQuote(_) => {
                self.flush_loose_inline();
                self.frames.push(Frame::Blocks(Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(start) => {
                // tight リスト項目（Paragraph タグ無し）の途中でネストが始まる場合、
                // 蓄積中のインラインを段落として確定してからフレームを積む。
                self.flush_loose_inline();
                self.frames.push(Frame::List {
                    start,
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                if let Some(Frame::List { items, .. }) = self.frames.last_mut() {
                    items.push(ListItemAcc {
                        checked: None,
                        blocks: Vec::new(),
                    });
                }
                // 項目内ブロックの受け皿。
                self.frames.push(Frame::Blocks(Vec::new()));
            }
            Tag::Table(_) => self.frames.push(Frame::Table {
                header: Vec::new(),
                rows: Vec::new(),
                current_row: Vec::new(),
            }),
            Tag::TableHead | Tag::TableRow => {
                if let Some(Frame::Table { current_row, .. }) = self.frames.last_mut() {
                    current_row.clear();
                }
            }
            Tag::TableCell => {
                self.inline = Some(InlineAcc::default());
                self.inline_kind = InlineKind::TableCell;
            }
            Tag::Emphasis => self.mark(|m| m.italic += 1),
            Tag::Strong => self.mark(|m| m.bold += 1),
            Tag::Strikethrough => self.mark(|m| m.strike += 1),
            Tag::Link { dest_url, .. } => self.mark(|m| m.link.push(dest_url.to_string())),
            // 画像はドライブ参照埋め込み（11P.6）へ寄せる。alt はテキストとして残る。
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => {
                let acc = self.inline.take().unwrap_or_default();
                let block = match self.inline_kind {
                    InlineKind::Heading(level) => Block::Heading {
                        level,
                        content: acc.inlines,
                    },
                    _ => Block::Paragraph(acc.inlines),
                };
                self.inline_kind = InlineKind::Paragraph;
                self.push_block(block);
            }
            TagEnd::BlockQuote(_) => {
                if let Some(Frame::Blocks(blocks)) = self.frames.pop() {
                    self.push_block(Block::Blockquote(blocks));
                }
            }
            TagEnd::CodeBlock => {
                if let Some((lang, mut code)) = self.code.take() {
                    if code.ends_with('\n') {
                        code.pop();
                    }
                    let block = if lang == EMBED_FENCE_LANG {
                        Block::Embed { payload: code }
                    } else {
                        Block::CodeBlock {
                            language: lang,
                            code,
                        }
                    };
                    self.push_block(block);
                }
            }
            TagEnd::Item => {
                // tight リスト項目のインライン（Paragraph タグ無し）を段落として確定する。
                self.flush_loose_inline();
                if let Some(Frame::Blocks(blocks)) = self.frames.pop() {
                    if let Some(Frame::List { items, .. }) = self.frames.last_mut() {
                        if let Some(item) = items.last_mut() {
                            item.blocks = blocks;
                        }
                    }
                }
            }
            TagEnd::List(_) => {
                if let Some(Frame::List { start, items }) = self.frames.pop() {
                    for block in finish_list(start, items) {
                        self.push_block(block);
                    }
                }
            }
            TagEnd::TableHead => {
                if let Some(Frame::Table {
                    header,
                    current_row,
                    ..
                }) = self.frames.last_mut()
                {
                    *header = std::mem::take(current_row);
                }
            }
            TagEnd::TableRow => {
                if let Some(Frame::Table {
                    rows, current_row, ..
                }) = self.frames.last_mut()
                {
                    rows.push(std::mem::take(current_row));
                }
            }
            TagEnd::TableCell => {
                let acc = self.inline.take().unwrap_or_default();
                if let Some(Frame::Table { current_row, .. }) = self.frames.last_mut() {
                    current_row.push(acc.inlines);
                }
                self.inline_kind = InlineKind::Paragraph;
            }
            TagEnd::Table => {
                if let Some(Frame::Table { header, rows, .. }) = self.frames.pop() {
                    self.push_block(Block::Table(Table { header, rows }));
                }
            }
            TagEnd::Emphasis => self.mark(|m| m.italic = m.italic.saturating_sub(1)),
            TagEnd::Strong => self.mark(|m| m.bold = m.bold.saturating_sub(1)),
            TagEnd::Strikethrough => self.mark(|m| m.strike = m.strike.saturating_sub(1)),
            TagEnd::Link => self.mark(|m| {
                m.link.pop();
            }),
            _ => {}
        }
    }

    fn mark(&mut self, f: impl FnOnce(&mut MarkStack)) {
        f(&mut self.ensure_inline().marks);
    }

    fn ensure_inline(&mut self) -> &mut InlineAcc {
        self.inline.get_or_insert_default()
    }

    /// ブロックコンテキスト外に蓄積されたインライン（tight リスト項目等）を段落へ確定する。
    fn flush_loose_inline(&mut self) {
        if self.inline_kind != InlineKind::Paragraph {
            return; // 見出し・セルの途中でブロックは始まらない（GFM）。
        }
        if let Some(acc) = self.inline.take() {
            if !acc.inlines.is_empty() {
                self.push_block(Block::Paragraph(acc.inlines));
            }
        }
    }

    /// ブロック HTML の蓄積を `html` コードブロックとして確定する（実行不能な形へ縮退）。
    fn flush_html(&mut self) {
        if let Some(mut html) = self.html.take() {
            while html.ends_with('\n') {
                html.pop();
            }
            if !html.is_empty() {
                self.push_block(Block::CodeBlock {
                    language: "html".into(),
                    code: html,
                });
            }
        }
    }

    fn push_block(&mut self, block: Block) {
        match self.frames.last_mut() {
            Some(Frame::Blocks(blocks)) => blocks.push(block),
            Some(Frame::List { items, .. }) => {
                // Item 外に落ちた場合の防御（通常は Blocks フレームが受ける）。
                if let Some(item) = items.last_mut() {
                    item.blocks.push(block);
                }
            }
            Some(Frame::Table { .. }) | None => {
                // Table 中にブロックは来ない（GFM）。ルートへ落とす。
                if self.frames.is_empty() {
                    self.frames.push(Frame::Blocks(vec![block]));
                } else if let Some(Frame::Blocks(blocks)) = self.frames.first_mut() {
                    blocks.push(block);
                }
            }
        }
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush_html();
        // 未クローズのフレームを畳む（不正 md への防御・fail-closed で内容は保持）。
        while self.frames.len() > 1 {
            match self.frames.pop() {
                Some(Frame::Blocks(blocks)) => {
                    for b in blocks {
                        self.push_block(b);
                    }
                }
                Some(Frame::List { start, items }) => {
                    for block in finish_list(start, items) {
                        self.push_block(block);
                    }
                }
                Some(Frame::Table { header, rows, .. }) => {
                    self.push_block(Block::Table(Table { header, rows }));
                }
                None => break,
            }
        }
        match self.frames.pop() {
            Some(Frame::Blocks(blocks)) => blocks,
            Some(Frame::List { start, items }) => finish_list(start, items),
            Some(Frame::Table { header, rows, .. }) => {
                vec![Block::Table(Table { header, rows })]
            }
            None => Vec::new(),
        }
    }
}

/// リスト項目の checked 有無で bullet / ordered / task を確定する。
///
/// GFM は 1 つのリストにタスク項目と通常項目を混在できるが、TipTap の taskList は
/// taskItem のみを許すため、**連続する同種項目ごとにリストを分割**して正規化する
/// （分割後も再パース→再描画で安定する）。
fn finish_list(start: Option<u64>, items: Vec<ListItemAcc>) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut seq_no = start;
    let mut task_run: Vec<TaskItem> = Vec::new();
    let mut plain_run: Vec<Vec<Block>> = Vec::new();
    let flush_plain =
        |blocks: &mut Vec<Block>, plain_run: &mut Vec<Vec<Block>>, seq_no: &mut Option<u64>| {
            if plain_run.is_empty() {
                return;
            }
            let items = std::mem::take(plain_run);
            let count = items.len() as u64;
            if let Some(n) = *seq_no {
                blocks.push(Block::OrderedList { start: n, items });
                *seq_no = Some(n + count);
            } else {
                blocks.push(Block::BulletList(items));
            }
        };
    let flush_task = |blocks: &mut Vec<Block>, task_run: &mut Vec<TaskItem>| {
        if !task_run.is_empty() {
            blocks.push(Block::TaskList(std::mem::take(task_run)));
        }
    };
    for item in items {
        if let Some(checked) = item.checked {
            flush_plain(&mut blocks, &mut plain_run, &mut seq_no);
            task_run.push(TaskItem {
                checked,
                content: item.blocks,
            });
        } else {
            flush_task(&mut blocks, &mut task_run);
            plain_run.push(item.blocks);
        }
    }
    flush_plain(&mut blocks, &mut plain_run, &mut seq_no);
    flush_task(&mut blocks, &mut task_run);
    blocks
}
