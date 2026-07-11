//! ノート本文の中間表現（AST）と正規化 md への描画（Task 11P.2）。
//!
//! md ⇔ Yjs の直接変換を避け、**AST を単一の正準層**とする:
//! `md --parse--> AST --build--> Yjs` / `Yjs --read--> AST --render--> md`。
//! 往復保証（PIT-37③）の対象は本 AST が表現する要素に閉じる:
//! 見出し・段落・リスト・チェックリスト・表・コードブロック・引用・埋め込み参照・
//! 水平線・強調系マーク・リンク・改行。これ以外（生 HTML 等）は**コードブロックへ
//! 縮退**させ、実行可能な形では絶対に残さない（stored XSS 遮断・Task 11P.6）。

/// インラインのマーク集合（TipTap のマーク名と 1:1）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // マークの有無集合であり状態機械ではない。
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// リンク先 href（`link` マーク）。
    pub link: Option<String>,
}

impl Marks {
    pub fn is_plain(&self) -> bool {
        !self.bold && !self.italic && !self.strike && !self.code && self.link.is_none()
    }
}

/// インライン要素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// マーク付きテキストラン。
    Text { text: String, marks: Marks },
    /// 強制改行（`\` ＋改行として描画）。
    HardBreak,
}

/// ブロック要素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    /// level は 1..=6。
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    CodeBlock {
        language: String,
        code: String,
    },
    /// 埋め込みブロック参照（Task 11P.6 の 3 種を JSON ペイロードで持つ）。
    /// md 表現は ```shiki-embed フェンス（payload はフェンス内 JSON）。
    Embed {
        payload: String,
    },
    Blockquote(Vec<Block>),
    BulletList(Vec<Vec<Block>>),
    OrderedList {
        start: u64,
        items: Vec<Vec<Block>>,
    },
    TaskList(Vec<TaskItem>),
    Table(Table),
    HorizontalRule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskItem {
    pub checked: bool,
    pub content: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// ヘッダ行（セルごとのインライン列）。
    pub header: Vec<Vec<Inline>>,
    /// データ行。
    pub rows: Vec<Vec<Vec<Inline>>>,
}

/// 埋め込みフェンスの言語タグ（md 表現の契約・11P.6 と共有）。
pub const EMBED_FENCE_LANG: &str = "shiki-embed";

// ---------------------------------------------------------------------------
// AST → 正規化 md
// ---------------------------------------------------------------------------

/// ブロック列を正規化 md に描画する（末尾は単一改行）。
pub fn render_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_block(&mut out, block, "");
    }
    out
}

/// 1 ブロックを `prefix`（引用等の行頭接頭辞）付きで描画する。
fn render_block(out: &mut String, block: &Block, prefix: &str) {
    match block {
        Block::Paragraph(inlines) => {
            push_prefixed_lines(out, prefix, &render_inlines(inlines));
        }
        Block::Heading { level, content } => {
            let hashes = "#".repeat((*level).clamp(1, 6) as usize);
            push_prefixed_lines(
                out,
                prefix,
                &format!("{hashes} {}", render_inlines(content)),
            );
        }
        Block::CodeBlock { language, code } => {
            let mut body = format!("```{language}\n");
            body.push_str(code);
            if !code.is_empty() && !code.ends_with('\n') {
                body.push('\n');
            }
            body.push_str("```");
            push_prefixed_lines(out, prefix, &body);
        }
        Block::Embed { payload } => {
            let body = format!("```{EMBED_FENCE_LANG}\n{payload}\n```");
            push_prefixed_lines(out, prefix, &body);
        }
        Block::Blockquote(blocks) => {
            let mut inner = String::new();
            for (i, b) in blocks.iter().enumerate() {
                if i > 0 {
                    inner.push('\n');
                }
                render_block(&mut inner, b, "");
            }
            let quoted = inner
                .trim_end_matches('\n')
                .lines()
                .map(|l| {
                    if l.is_empty() {
                        ">".to_string()
                    } else {
                        format!("> {l}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            push_prefixed_lines(out, prefix, &quoted);
        }
        Block::BulletList(items) => {
            for item in items {
                render_list_item(out, prefix, "- ", item);
            }
        }
        Block::OrderedList { start, items } => {
            for (i, item) in items.iter().enumerate() {
                let marker = format!("{}. ", start + i as u64);
                render_list_item(out, prefix, &marker, item);
            }
        }
        Block::TaskList(items) => {
            for item in items {
                let marker = if item.checked { "- [x] " } else { "- [ ] " };
                render_list_item(out, prefix, marker, &item.content);
            }
        }
        Block::Table(table) => {
            let mut body = String::new();
            let cols = table.header.len().max(1);
            body.push('|');
            for cell in &table.header {
                body.push(' ');
                body.push_str(&render_inlines_single_line(cell));
                body.push_str(" |");
            }
            body.push('\n');
            body.push('|');
            for _ in 0..cols {
                body.push_str(" --- |");
            }
            for row in &table.rows {
                body.push('\n');
                body.push('|');
                for cell in row {
                    body.push(' ');
                    body.push_str(&render_inlines_single_line(cell));
                    body.push_str(" |");
                }
            }
            push_prefixed_lines(out, prefix, &body);
        }
        Block::HorizontalRule => push_prefixed_lines(out, prefix, "---"),
    }
}

/// リスト 1 項目（複数ブロック可・ネスト可）を描画する。
/// 継続行はマーカー幅ぶんインデントする（CommonMark 準拠）。
fn render_list_item(out: &mut String, prefix: &str, marker: &str, item: &[Block]) {
    let cont = format!("{prefix}{}", " ".repeat(marker.len()));
    let mut inner = String::new();
    for (i, b) in item.iter().enumerate() {
        // 段落直後のネストリストは tight（空行なし）で続ける（正規形）。
        // それ以外のブロック間は空行区切り（loose）。
        let tight_list = matches!(
            b,
            Block::BulletList(_) | Block::OrderedList { .. } | Block::TaskList(_)
        );
        if i > 0 && !tight_list {
            inner.push('\n');
        }
        render_block(&mut inner, b, "");
    }
    let mut first = true;
    for line in inner.trim_end_matches('\n').lines() {
        if first {
            out.push_str(prefix);
            out.push_str(marker);
            out.push_str(line);
            first = false;
        } else if line.is_empty() {
            // 空行に継続インデントを付けない（trailing whitespace を作らない）。
        } else {
            out.push_str(&cont);
            out.push_str(line);
        }
        out.push('\n');
    }
    if first {
        // 空項目でもマーカー行は出す。
        out.push_str(prefix);
        out.push_str(marker.trim_end());
        out.push('\n');
    }
}

/// 複数行文字列を prefix 付きで out へ（末尾に改行 1 つ）。
fn push_prefixed_lines(out: &mut String, prefix: &str, body: &str) {
    for line in body.lines() {
        out.push_str(prefix);
        out.push_str(line);
        out.push('\n');
    }
}

/// インライン列を md に描画する（HardBreak は `\` 改行）。
pub fn render_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, marks } => out.push_str(&render_marked_text(text, marks)),
            Inline::HardBreak => out.push_str("\\\n"),
        }
    }
    out
}

/// 表セル用: HardBreak を空白に落とし単一行を保証する。
fn render_inlines_single_line(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, marks } => {
                let rendered = render_marked_text(text, marks).replace('\n', " ");
                // セル内の | はテーブル構造を壊すためエスケープする。
                out.push_str(&rendered.replace('|', "\\|"));
            }
            Inline::HardBreak => out.push(' '),
        }
    }
    out
}

/// マーク付きテキスト 1 ランを md に描画する。
///
/// ネスト順は外側から link → bold → italic → strike。code は排他
/// （コードスパン内では他マークを描画しない・md の意味論に合わせる）。
fn render_marked_text(text: &str, marks: &Marks) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut body = if marks.code {
        render_code_span(text)
    } else {
        let mut t = escape_inline(text);
        if marks.strike {
            t = format!("~~{t}~~");
        }
        if marks.italic {
            t = format!("*{t}*");
        }
        if marks.bold {
            t = format!("**{t}**");
        }
        t
    };
    if let Some(href) = &marks.link {
        body = format!("[{body}]({})", escape_link_dest(href));
    }
    body
}

/// コードスパン: 内容にバッククォートが含まれても壊れないようフェンス長を伸ばす。
fn render_code_span(text: &str) -> String {
    let max_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(max_run + 1);
    // 先頭/末尾がバッククォートの場合は空白で分離する（CommonMark の規則）。
    if text.starts_with('`') || text.ends_with('`') {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// インラインテキストの md 特殊文字エスケープ（正規形・パーサと往復安定）。
fn escape_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// リンク先の md エスケープ（括弧・空白を含む URL は <> で包む）。
fn escape_link_dest(href: &str) -> String {
    if href.contains(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        format!("<{href}>")
    } else {
        href.to_string()
    }
}
