//! レイアウト/親子チャンク化（Task 2.2・決定的な純関数）。
//!
//! - **レイアウト尊重**: 見出し境界でセクション（parent）を切り、段落境界で leaf を詰める。
//!   表は 1 ブロック＝1 チャンク（[`ChunkKind::Table`]）として**絶対に分割しない**。
//! - **親子（small-to-big）**: 検索対象は leaf/table。親（セクション全文）は文脈として
//!   LLM/UI に渡す。leaf → parent は `parent_id` で引ける。
//! - **決定的 ID**: `uuid5(node_id, "{version}/{ordinal}")`。同一版の再チャンクは同じ ID 群に
//!   なり、Qdrant/DB の upsert が冪等になる。
//!
//! サイズは文字数（`char_count`）基準。日本語はトークン数と文字数の乖離が小さく、
//! バイト数基準だと ASCII 文書と 3 倍ずれるため。

use uuid::Uuid;

use crate::anchor::{utf16_len, Anchor};
use crate::types::{BlockType, Chunk, ChunkKind, ParsedBlock};

/// チャンクサイズの調整点。既定は日本語ビジネス文書想定。
#[derive(Debug, Clone)]
pub struct ChunkParams {
    /// leaf の目標最大文字数（超えたら段落境界で分割）。
    pub max_leaf_chars: usize,
    /// parent（セクション本文）の最大文字数（超過分は打ち切り。文脈提示用のため）。
    pub max_parent_chars: usize,
}

impl Default for ChunkParams {
    fn default() -> Self {
        ChunkParams {
            max_leaf_chars: 600,
            max_parent_chars: 4000,
        }
    }
}

/// ブロック列を親子チャンクへ落とす。
///
/// `blocks` の添字がブロック番号（ordinal）。leaf / table にはその範囲のアンカー
/// （ブロック番号＋ブロック内の UTF-16 オフセット）と前後の文脈を付ける（#508）。
pub fn chunk_document(
    node_id: Uuid,
    version: i64,
    blocks: &[ParsedBlock],
    params: &ChunkParams,
) -> Vec<Chunk> {
    let mut builder = ChunkBuilder::new(node_id, version, blocks, params.clone());
    // 見出しスタック（(level, text)）。heading_path はここから導出する。
    let mut headings: Vec<(u32, String)> = Vec::new();

    for (index, block) in blocks.iter().enumerate() {
        let ordinal = i32::try_from(index).unwrap_or(i32::MAX);
        match block.block_type {
            BlockType::Heading => {
                // セクション境界: 進行中のセクションを確定してから見出しスタックを更新する。
                builder.flush_section(&heading_path(&headings));
                let level = block.level.unwrap_or(1).max(1);
                while headings.last().is_some_and(|(l, _)| *l >= level) {
                    headings.pop();
                }
                headings.push((level, block.text.trim().to_string()));
            }
            BlockType::Table => {
                builder.push_table(ordinal, block, &heading_path(&headings));
            }
            BlockType::Paragraph | BlockType::Caption | BlockType::ListItem => {
                builder.push_paragraph(ordinal, block);
            }
        }
    }
    builder.flush_section(&heading_path(&headings));
    builder.finish()
}

fn heading_path(headings: &[(u32, String)]) -> Vec<String> {
    headings.iter().map(|(_, t)| t.clone()).collect()
}

/// ブロック上の位置（ブロック番号・ブロック内のバイト位置と UTF-16 位置）。
#[derive(Debug, Clone, Copy)]
struct Pos {
    block: i32,
    byte: usize,
    u16: i32,
}

/// 進行中セクションの段落 1 個。
struct Para {
    block: i32,
    /// 前後の空白を除いた本文（ブロック本文の `lead` バイト目から）。
    text: String,
    lead: usize,
    page: Option<i32>,
}

/// 前後の文脈として残す文字数。
const CONTEXT_CHARS: usize = crate::anchor::QUOTE_CONTEXT_CHARS;

/// セクション（見出し境界）単位で parent + leaves を組み立てる内部状態。
struct ChunkBuilder<'a> {
    node_id: Uuid,
    version: i64,
    blocks: &'a [ParsedBlock],
    params: ChunkParams,
    chunks: Vec<Chunk>,
    ordinal: i32,
    /// 進行中セクションの段落。表は即確定するためここには入らない。
    pending: Vec<Para>,
    /// 進行中セクションの表チャンク（parent 確定時に parent_id を埋める）。
    pending_tables: Vec<Chunk>,
}

/// 組み立て中の leaf。
#[derive(Default)]
struct LeafAcc {
    text: String,
    /// `text.chars().count()` を毎回やり直さないための累積。断片が短い文書
    /// （字幕・チャットログ）ほど数え直しの倍率が上がる。
    chars: usize,
    page: Option<i32>,
    start: Option<Pos>,
    end: Option<Pos>,
}

impl<'a> ChunkBuilder<'a> {
    fn new(node_id: Uuid, version: i64, blocks: &'a [ParsedBlock], params: ChunkParams) -> Self {
        ChunkBuilder {
            node_id,
            version,
            blocks,
            params,
            chunks: Vec::new(),
            ordinal: 0,
            pending: Vec::new(),
            pending_tables: Vec::new(),
        }
    }

    /// 決定的チャンク ID: uuid5(node_id を名前空間に, "{version}/{ordinal}")。
    fn next_id(&mut self) -> (Uuid, i32) {
        let ordinal = self.ordinal;
        self.ordinal += 1;
        let id = Uuid::new_v5(
            &self.node_id,
            format!("{}/{}", self.version, ordinal).as_bytes(),
        );
        (id, ordinal)
    }

    fn block_text(&self, block: i32) -> &'a str {
        usize::try_from(block)
            .ok()
            .and_then(|i| self.blocks.get(i))
            .map_or("", |b| b.text.as_str())
    }

    fn push_paragraph(&mut self, ordinal: i32, block: &ParsedBlock) {
        let text = block.text.trim();
        if !text.is_empty() {
            self.pending.push(Para {
                block: ordinal,
                text: text.to_string(),
                lead: block.text.len() - block.text.trim_start().len(),
                page: block.page,
            });
        }
    }

    /// 表は表単位で 1 チャンク（分割禁止）。parent_id はセクション確定時に埋める。
    fn push_table(&mut self, ordinal: i32, block: &ParsedBlock, path: &[String]) {
        let text = block.text.trim();
        if text.is_empty() {
            return;
        }
        let lead = block.text.len() - block.text.trim_start().len();
        let start = Pos {
            block: ordinal,
            byte: lead,
            u16: utf16_len(&block.text[..lead]),
        };
        let end = Pos {
            block: ordinal,
            byte: lead + text.len(),
            u16: start.u16 + utf16_len(text),
        };
        let (id, chunk_ordinal) = self.next_id();
        let mut chunk = Chunk {
            id,
            parent_id: None, // flush_section で設定
            kind: ChunkKind::Table,
            ordinal: chunk_ordinal,
            page: block.page,
            heading_path: path.to_vec(),
            content: text.to_string(),
            anchor: None,
            quote_prefix: String::new(),
            quote_suffix: String::new(),
            boxes: Vec::new(),
        };
        self.locate(&mut chunk, start, end);
        self.pending_tables.push(chunk);
    }

    /// 進行中セクションを確定する: parent 1 個＋段落 leaf 群＋表チャンク群。
    fn flush_section(&mut self, path: &[String]) {
        if self.pending.is_empty() && self.pending_tables.is_empty() {
            return;
        }

        // parent 本文 = セクション内の段落＋表を読み順で連結（上限で打ち切り）。
        let tables = std::mem::take(&mut self.pending_tables);
        let paragraphs = std::mem::take(&mut self.pending);
        let mut parent_content = String::new();
        for text in paragraphs
            .iter()
            .map(|p| p.text.as_str())
            .chain(tables.iter().map(|t| t.content.as_str()))
        {
            if !parent_content.is_empty() {
                parent_content.push_str("\n\n");
            }
            parent_content.push_str(text);
            if parent_content.chars().count() >= self.params.max_parent_chars {
                parent_content = parent_content
                    .chars()
                    .take(self.params.max_parent_chars)
                    .collect();
                break;
            }
        }

        let (parent_uuid, parent_ordinal) = self.next_id();
        let first_page = paragraphs
            .iter()
            .map(|p| p.page)
            .chain(tables.iter().map(|t| t.page))
            .find(Option::is_some)
            .flatten();
        self.chunks.push(Chunk {
            id: parent_uuid,
            parent_id: None,
            kind: ChunkKind::Parent,
            ordinal: parent_ordinal,
            page: first_page,
            heading_path: path.to_vec(),
            content: parent_content,
            anchor: None,
            quote_prefix: String::new(),
            quote_suffix: String::new(),
            boxes: Vec::new(),
        });

        // 段落を max_leaf_chars まで詰めて leaf 化。段落境界で切るのが基本だが、
        // 1 段落だけで上限を超える場合は段落内でも割る（下記 split_oversized）。
        let mut leaf = LeafAcc::default();
        for para in &paragraphs {
            let lead_u16 = utf16_len(&self.block_text(para.block)[..para.lead]);
            // 断片は順に並ぶので、UTF-16 位置は前の断片の続きから数える（全体で線形）。
            let (mut at_byte, mut at_u16) = (0usize, 0i32);
            for (offset, piece) in split_oversized_at(&para.text, self.params.max_leaf_chars) {
                at_u16 += utf16_len(&para.text[at_byte..offset]);
                let start = Pos {
                    block: para.block,
                    byte: para.lead + offset,
                    u16: lead_u16 + at_u16,
                };
                at_byte = offset + piece.len();
                at_u16 += utf16_len(piece);
                let end = Pos {
                    block: para.block,
                    byte: para.lead + at_byte,
                    u16: lead_u16 + at_u16,
                };

                let piece_chars = piece.chars().count();
                // 区切りの "\n\n" も leaf の文字数に乗るので判定に含める。含めないと
                // ちょうど上限で収まる組み合わせのときだけ max+2 文字の leaf ができる。
                let sep = if leaf.text.is_empty() { 0 } else { 2 };
                if !leaf.text.is_empty()
                    && leaf.chars + sep + piece_chars > self.params.max_leaf_chars
                {
                    self.emit_leaf(&mut leaf, parent_uuid, path);
                }
                if leaf.text.is_empty() {
                    leaf.page = para.page;
                    leaf.start = Some(start);
                } else {
                    leaf.text.push_str("\n\n");
                    leaf.chars += 2;
                }
                leaf.text.push_str(piece);
                leaf.chars += piece_chars;
                leaf.end = Some(end);
            }
        }
        self.emit_leaf(&mut leaf, parent_uuid, path);

        // 表チャンクへ parent を結線して確定する。
        for mut table in tables {
            table.parent_id = Some(parent_uuid);
            self.chunks.push(table);
        }
    }

    fn emit_leaf(&mut self, leaf: &mut LeafAcc, parent_id: Uuid, path: &[String]) {
        let acc = std::mem::take(leaf);
        if acc.text.is_empty() {
            return;
        }
        let (id, ordinal) = self.next_id();
        let mut chunk = Chunk {
            id,
            parent_id: Some(parent_id),
            kind: ChunkKind::Leaf,
            ordinal,
            page: acc.page,
            heading_path: path.to_vec(),
            content: acc.text,
            anchor: None,
            quote_prefix: String::new(),
            quote_suffix: String::new(),
            boxes: Vec::new(),
        };
        if let (Some(start), Some(end)) = (acc.start, acc.end) {
            self.locate(&mut chunk, start, end);
        }
        self.chunks.push(chunk);
    }

    /// チャンクに範囲のアンカー・前後の文脈・原本上の枠を付ける。
    fn locate(&self, chunk: &mut Chunk, start: Pos, end: Pos) {
        chunk.anchor = Some(Anchor {
            block_start: start.block,
            off_start: start.u16,
            block_end: end.block,
            off_end: end.u16,
        });
        chunk.quote_prefix = self.context_before(start);
        chunk.quote_suffix = self.context_after(end);
        for i in start.block..=end.block {
            let Some(block) = usize::try_from(i).ok().and_then(|i| self.blocks.get(i)) else {
                continue;
            };
            for b in &block.prov {
                if !chunk.boxes.contains(b) {
                    chunk.boxes.push(b.clone());
                }
            }
        }
    }

    /// 範囲の直前の文脈（同じブロックで足りなければ、前のブロックの末尾を改行でつなぐ）。
    fn context_before(&self, at: Pos) -> String {
        let head = &self.block_text(at.block)[..at.byte];
        let mut out: Vec<char> = head.chars().rev().take(CONTEXT_CHARS).collect();
        if out.len() < CONTEXT_CHARS && at.block > 0 {
            out.push('\n');
            let prev = self.block_text(at.block - 1).trim_end();
            out.extend(prev.chars().rev().take(CONTEXT_CHARS - out.len()));
        }
        out.into_iter()
            .rev()
            .collect::<String>()
            .trim_start()
            .to_string()
    }

    /// 範囲の直後の文脈（同じブロックで足りなければ、次のブロックの先頭を改行でつなぐ）。
    fn context_after(&self, at: Pos) -> String {
        let tail = &self.block_text(at.block)[at.byte..];
        let mut out: String = tail.chars().take(CONTEXT_CHARS).collect();
        let taken = out.chars().count();
        let next = usize::try_from(at.block + 1)
            .ok()
            .and_then(|i| self.blocks.get(i));
        if taken < CONTEXT_CHARS {
            if let Some(next) = next {
                out.push('\n');
                out.extend(
                    next.text
                        .trim_start()
                        .chars()
                        .take(CONTEXT_CHARS - taken - 1),
                );
            }
        }
        out.trim_end().to_string()
    }

    fn finish(mut self) -> Vec<Chunk> {
        // 出現順（ordinal）で安定させる。表は next_id 先取りのため並べ直しが要る。
        self.chunks.sort_by_key(|c| c.ordinal);
        self.chunks
    }
}

/// 上限を超える 1 段落を、上限以下の断片へ割る（上限以下ならそのまま 1 個返す）。
///
/// パーサが段落を刻めなかった文書（空行を使わない議事録・字幕・ログ、改行を持たない
/// PDF 抽出結果など）が、そのまま数万文字の leaf になるのを防ぐ最後の砦。巨大 leaf は
/// 埋め込みが文書全体の平均に薄まって意味検索から消え、BM25 でも長文ペナルティで沈む。
///
/// 切れ目は「文末（。！？!?）→ 改行 → 空白 → 文字数」の順に優先する。空白を見るのは
/// 英文で語の途中が切れるのを防ぐため。ASCII の `.` は小数や略語にも出るので文末には
/// 含めない（空白へのフォールバックがあれば語中分割は起きない）。
///
/// 見つけた切れ目が手前すぎる場合は使わず次の候補へ送る。窓の先頭近くに句点が 1 個だけ
/// ある文書（「開会します。」＋句読点の無い長い羅列）で、1 文字の leaf が索引に入るため。
///
/// 計算量は入力長に対して線形。**残り文字数を数え直さない**こと（`rest.chars().count()`
/// をループ条件に置くと O(n^2) になり、改行も句点も無い 50MB の text/plain で
/// チャンク化だけに数分かかる。`chunk_document` は `spawn_blocking` の外で呼ばれるため
/// tokio ワーカを占有し、`job_vt_secs` 超過でジョブが再配信されて DLQ に落ちる）。
fn split_oversized_at(text: &str, max_chars: usize) -> Vec<(usize, &str)> {
    let max = max_chars.max(1);
    // 断片の位置は入力 `text` 先頭からのバイト位置で返す（アンカーの基準・#508）。
    let lead = text.len() - text.trim_start().len();
    let text = text.trim();
    let mut pieces = Vec::new();
    let mut rest = text;
    let mut consumed = lead;
    // nth(max) は高々 max 文字ぶんしか進まないので、1 周あたり O(max)。
    // 各周で rest は min_fill 以上縮むため、全体でも入力長に線形。
    while let Some((limit, _)) = rest.char_indices().nth(max) {
        let head = &rest[..limit];
        // 断片が短くなりすぎる切れ目は採らない（ノイズ chunk を作らないための下限）。
        let min_fill = limit / 2;
        let cut = [
            sentence_end(head),
            head.rfind('\n').map(|i| i + 1),
            head.rfind(char::is_whitespace)
                .map(|i| i + head[i..].chars().next().map_or(1, char::len_utf8)),
        ]
        .into_iter()
        .flatten()
        .find(|&c| c >= min_fill)
        // どの候補も無い/手前すぎるなら max 文字で強制的に割る。
        // cut は必ず 1 以上になるので rest は毎周必ず縮む（無限ループしない）。
        .unwrap_or(limit);
        let (piece, tail) = rest.split_at(cut);
        push_trimmed(&mut pieces, consumed, piece);
        consumed += cut;
        rest = tail;
    }
    push_trimmed(&mut pieces, consumed, rest);
    pieces
}

/// 前後の空白を除いた断片を、その開始位置つきで積む（空なら積まない）。
fn push_trimmed<'t>(pieces: &mut Vec<(usize, &'t str)>, at: usize, piece: &'t str) {
    let trimmed = piece.trim();
    if !trimmed.is_empty() {
        pieces.push((at + piece.len() - piece.trim_start().len(), trimmed));
    }
}

/// [`split_oversized_at`] の断片だけ（テストで切れ目を見る用）。
#[cfg(test)]
fn split_oversized(text: &str, max_chars: usize) -> Vec<&str> {
    split_oversized_at(text, max_chars)
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

/// `head` 内で最後に現れる文末の直後のバイト位置。
fn sentence_end(head: &str) -> Option<usize> {
    head.rfind(['。', '！', '？', '!', '?'])
        .map(|i| i + head[i..].chars().next().map_or(1, char::len_utf8))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn heading(level: u32, text: &str) -> ParsedBlock {
        ParsedBlock {
            block_type: BlockType::Heading,
            level: Some(level),
            text: text.into(),
            page: Some(1),
            prov: Vec::new(),
            list_marker: None,
        }
    }

    fn para(text: &str) -> ParsedBlock {
        ParsedBlock {
            block_type: BlockType::Paragraph,
            level: None,
            text: text.into(),
            page: Some(1),
            prov: Vec::new(),
            list_marker: None,
        }
    }

    fn table(text: &str) -> ParsedBlock {
        ParsedBlock {
            block_type: BlockType::Table,
            level: None,
            text: text.into(),
            page: Some(2),
            prov: Vec::new(),
            list_marker: None,
        }
    }

    fn node() -> Uuid {
        Uuid::from_u128(0x1234)
    }

    #[test]
    fn builds_parent_and_leaves_per_section() {
        let blocks = vec![
            heading(1, "第一章"),
            para("最初の段落。"),
            para("次の段落。"),
            heading(1, "第二章"),
            para("別セクションの段落。"),
        ];
        let chunks = chunk_document(node(), 1, &blocks, &ChunkParams::default());

        let parents: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Parent)
            .collect();
        let leaves: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Leaf)
            .collect();
        assert_eq!(parents.len(), 2);
        assert_eq!(leaves.len(), 2);
        // 小チャンク → 親チャンクの対応が引ける（Task 2.2 受入条件）。
        assert_eq!(leaves[0].parent_id, Some(parents[0].id));
        assert_eq!(leaves[1].parent_id, Some(parents[1].id));
        assert_eq!(leaves[0].heading_path, vec!["第一章"]);
        assert!(parents[0].content.contains("最初の段落。"));
        assert!(parents[0].content.contains("次の段落。"));
    }

    #[test]
    fn heading_path_tracks_nesting() {
        let blocks = vec![
            heading(1, "報告"),
            heading(2, "概要"),
            para("概要本文。"),
            heading(2, "詳細"),
            para("詳細本文。"),
            heading(1, "付録"),
            para("付録本文。"),
        ];
        let chunks = chunk_document(node(), 1, &blocks, &ChunkParams::default());
        let leaves: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Leaf)
            .collect();
        assert_eq!(leaves[0].heading_path, vec!["報告", "概要"]);
        assert_eq!(leaves[1].heading_path, vec!["報告", "詳細"]);
        // 同レベル見出しはスタックから置換され、上位へ戻れる。
        assert_eq!(leaves[2].heading_path, vec!["付録"]);
    }

    #[test]
    fn table_is_never_split_and_links_to_parent() {
        let big_table = format!(
            "| 拠点 | 売上 |\n|---|---|\n{}",
            "| 東京 | 1200 |\n".repeat(200) // max_leaf_chars を大きく超える表
        );
        let blocks = vec![heading(1, "売上"), para("前置き。"), table(&big_table)];
        let chunks = chunk_document(node(), 1, &blocks, &ChunkParams::default());

        let tables: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Table)
            .collect();
        assert_eq!(tables.len(), 1, "表は分割されない");
        assert_eq!(tables[0].content.matches("東京").count(), 200);
        let parent = chunks.iter().find(|c| c.kind == ChunkKind::Parent).unwrap();
        assert_eq!(tables[0].parent_id, Some(parent.id));
        assert_eq!(tables[0].page, Some(2));
    }

    #[test]
    fn long_sections_split_at_paragraph_boundaries() {
        let long_para = "あ".repeat(400);
        let blocks = vec![
            heading(1, "長文"),
            para(&long_para),
            para(&long_para),
            para(&long_para),
        ];
        let params = ChunkParams {
            max_leaf_chars: 600,
            max_parent_chars: 4000,
        };
        let chunks = chunk_document(node(), 1, &blocks, &params);
        let leaves: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Leaf)
            .collect();
        // 400+400 > 600 なので段落境界で分かれ、3 段落 → 3 leaf。
        assert_eq!(leaves.len(), 3);
        assert!(leaves.iter().all(|l| l.content.chars().count() <= 600));
    }

    #[test]
    fn chunk_ids_are_deterministic_across_runs() {
        let blocks = vec![heading(1, "章"), para("本文。")];
        let a = chunk_document(node(), 7, &blocks, &ChunkParams::default());
        let b = chunk_document(node(), 7, &blocks, &ChunkParams::default());
        assert_eq!(
            a.iter().map(|c| c.id).collect::<Vec<_>>(),
            b.iter().map(|c| c.id).collect::<Vec<_>>(),
            "同一 (node, version) の再チャンクは同じ ID 群（冪等 upsert の鍵）"
        );
        // 版が変われば ID も変わる（旧版の残骸と衝突しない）。
        let c = chunk_document(node(), 8, &blocks, &ChunkParams::default());
        assert_ne!(a[0].id, c[0].id);
    }

    #[test]
    fn document_without_headings_gets_root_section() {
        let blocks = vec![para("見出しのないメモ。")];
        let chunks = chunk_document(node(), 1, &blocks, &ChunkParams::default());
        assert_eq!(chunks.len(), 2); // parent + leaf
        assert!(chunks.iter().all(|c| c.heading_path.is_empty()));
    }

    #[test]
    fn empty_and_whitespace_blocks_are_dropped() {
        let blocks = vec![heading(1, "章"), para("  "), para(""), table("  ")];
        let chunks = chunk_document(node(), 1, &blocks, &ChunkParams::default());
        assert!(chunks.is_empty(), "空セクションはチャンクを生まない");
    }

    #[test]
    fn parent_content_is_capped() {
        let blocks = vec![heading(1, "章"), para(&"あ".repeat(10_000))];
        let params = ChunkParams {
            max_leaf_chars: 600,
            max_parent_chars: 1000,
        };
        let chunks = chunk_document(node(), 1, &blocks, &params);
        let parent = chunks.iter().find(|c| c.kind == ChunkKind::Parent).unwrap();
        assert_eq!(parent.content.chars().count(), 1000);
    }

    #[test]
    fn searchable_text_prefixes_heading_path() {
        let blocks = vec![heading(1, "報告"), heading(2, "概要"), para("本文。")];
        let chunks = chunk_document(node(), 1, &blocks, &ChunkParams::default());
        let leaf = chunks.iter().find(|c| c.kind == ChunkKind::Leaf).unwrap();
        assert_eq!(leaf.searchable_text(), "報告 > 概要\n本文。");
    }

    /// 実バグの回帰: 空行を持たない議事録 TXT はパーサが 1 段落として返すため、
    /// 段落境界だけを見る実装では数万文字の leaf が 1 個できていた。
    #[test]
    fn single_oversized_paragraph_is_split_into_bounded_leaves() {
        let body = ("あ".repeat(200) + "。").repeat(50);
        let blocks = vec![heading(1, "会議録"), para(&body)];
        let params = ChunkParams {
            max_leaf_chars: 600,
            max_parent_chars: 4000,
        };
        let chunks = chunk_document(node(), 1, &blocks, &params);
        let leaves: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Leaf)
            .collect();
        assert!(leaves.len() > 10, "1 段落でも分割される: {}", leaves.len());
        for leaf in &leaves {
            assert!(
                leaf.content.chars().count() <= params.max_leaf_chars,
                "leaf が上限超過: {}",
                leaf.content.chars().count()
            );
        }
        // 本文が欠けていないこと。区切り "\n\n" のぶんだけ増えるので下限で見る。
        let total: usize = leaves.iter().map(|c| c.content.chars().count()).sum();
        assert!(
            total >= body.chars().count(),
            "本文が落ちている: {total} < {}",
            body.chars().count()
        );
    }

    #[test]
    fn oversized_paragraph_prefers_sentence_then_newline_boundaries() {
        assert_eq!(
            split_oversized("あいう。えおか。きくけ。", 8),
            vec!["あいう。えおか。", "きくけ。"]
        );
        assert_eq!(
            split_oversized("あいうえお\nかきくけこ\nさし", 7),
            vec!["あいうえお", "かきくけこ", "さし"]
        );
    }

    /// 切れ目が無い（句読点も改行も無い）場合でも必ず上限以下に割れること。
    /// 全角文字でバイト境界を割らないことも同時に見る。
    #[test]
    fn oversized_paragraph_without_boundaries_is_hard_split() {
        let text = "あ".repeat(1000);
        let pieces = split_oversized(&text, 300);
        assert_eq!(pieces.len(), 4);
        assert!(pieces.iter().all(|p| p.chars().count() <= 300));
        assert_eq!(pieces.concat().chars().count(), 1000);
    }

    #[test]
    fn text_within_limit_is_returned_untouched() {
        assert_eq!(split_oversized("短い。", 600), vec!["短い。"]);
    }

    /// 区切り "\n\n" を上限判定に含めていないと leaf が max+2 文字になる回帰。
    #[test]
    fn separator_is_counted_against_the_limit() {
        let half = "あ".repeat(300);
        let blocks = vec![heading(1, "章"), para(&half), para(&half)];
        let params = ChunkParams {
            max_leaf_chars: 600,
            max_parent_chars: 4000,
        };
        let chunks = chunk_document(node(), 1, &blocks, &params);
        for leaf in chunks.iter().filter(|c| c.kind == ChunkKind::Leaf) {
            assert!(
                leaf.content.chars().count() <= params.max_leaf_chars,
                "leaf が上限超過: {}",
                leaf.content.chars().count()
            );
        }
    }

    /// 英文は文末記号（。！？）が無いため、空白に落ちないと語の途中で切れる。
    #[test]
    fn english_text_splits_at_word_boundaries() {
        let sentence = "The quick brown fox jumps over the lazy dog. ";
        let body = sentence.repeat(3);
        let pieces = split_oversized(&body, 100);
        for piece in &pieces {
            assert!(
                !piece.ends_with(|c: char| c.is_alphanumeric()) || piece.chars().count() <= 100,
                "語中で切れている: {piece:?}"
            );
            // 断片の先頭・末尾が語の断片になっていないこと。
            assert!(
                piece.split_whitespace().all(|w| sentence.contains(w)),
                "語が壊れている: {piece:?}"
            );
        }
    }

    /// 窓の先頭付近にだけ文末がある文書で、1 文字の leaf を作らない。
    #[test]
    fn early_sentence_end_does_not_produce_a_tiny_leaf() {
        let body = "。".to_string() + &"い".repeat(1500);
        let blocks = vec![heading(1, "章"), para(&body)];
        let params = ChunkParams {
            max_leaf_chars: 600,
            max_parent_chars: 4000,
        };
        let chunks = chunk_document(node(), 1, &blocks, &params);
        for leaf in chunks.iter().filter(|c| c.kind == ChunkKind::Leaf) {
            assert!(
                leaf.content.chars().count() > 1,
                "1 文字の leaf が生成された: {:?}",
                leaf.content
            );
        }
    }

    /// 文末と改行の両方があるとき、文末が優先されること（優先順位そのものの検証）。
    #[test]
    fn sentence_end_wins_over_newline_when_both_are_available() {
        // 窓内には文末（3 文字目）と改行（6 文字目）の両方があるが、より手前の
        // 文末で切れる＝改行より文末が優先されている。
        assert_eq!(
            split_oversized("あいう。えお\nかきくけこ", 8),
            vec!["あいう。", "えお\nかきくけこ"]
        );
    }

    /// 空・空白のみは断片を生まない（呼び出し側で "\n\n" だけが積まれるのを防ぐ）。
    #[test]
    fn blank_input_yields_no_pieces() {
        assert!(split_oversized("", 600).is_empty());
        assert!(split_oversized("   ", 600).is_empty());
        assert!(split_oversized(&" ".repeat(2000), 600).is_empty());
    }

    /// 切れ目を持たない巨大入力でも線形時間で終わること。
    ///
    /// 残り文字数をループ条件で数え直す実装（O(n^2)）だと、この入力は debug ビルドで
    /// 分単位になる。閾値は環境差を吸収できる大きさに取り、二次オーダだけを検出する。
    #[test]
    fn huge_input_without_boundaries_stays_linear() {
        let body = "あ".repeat(400_000);
        let started = std::time::Instant::now();
        let pieces = split_oversized(&body, 600);
        let elapsed = started.elapsed();
        assert_eq!(pieces.len(), 400_000 / 600 + 1);
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "分割に時間がかかりすぎ（二次オーダの疑い）: {elapsed:?}"
        );
    }

    fn leaves(chunks: &[Chunk]) -> Vec<&Chunk> {
        chunks
            .iter()
            .filter(|c| c.kind != ChunkKind::Parent)
            .collect()
    }

    /// アンカーの範囲を doc_block（＝入力ブロック列）から UTF-16 で切り出す（ブラウザと同じ数え方）。
    #[allow(clippy::cast_sign_loss)] // テストの入力は非負の番号・オフセットだけ。
    fn slice(blocks: &[ParsedBlock], a: &Anchor) -> String {
        let mut out = Vec::new();
        for i in a.block_start..=a.block_end {
            let units: Vec<u16> = blocks[i as usize].text.encode_utf16().collect();
            let from = if i == a.block_start {
                a.off_start as usize
            } else {
                0
            };
            let to = if i == a.block_end {
                a.off_end as usize
            } else {
                units.len()
            };
            out.push(String::from_utf16(&units[from..to]).unwrap());
        }
        out.join("\n\n")
    }

    #[test]
    fn leaf_anchor_points_back_to_its_text_in_the_blocks() {
        let blocks = vec![
            heading(1, "第5章 休業"),
            para("  従業員は育児休業をすることができる。"),
            para("𠮷野家の例: 申出は1か月前まで。"),
            table("| a | b |\n|---|---|"),
        ];
        let chunks = chunk_document(node(), 1, &blocks, &ChunkParams::default());
        let leaf = chunks.iter().find(|c| c.kind == ChunkKind::Leaf).unwrap();
        let table = chunks.iter().find(|c| c.kind == ChunkKind::Table).unwrap();
        // 2 段落を詰めた leaf は、ブロック 1 の先頭（空白を除く）からブロック 2 の末尾まで。
        let a = leaf.anchor.unwrap();
        assert_eq!((a.block_start, a.off_start, a.block_end), (1, 2, 2));
        assert_eq!(slice(&blocks, &a), leaf.content);
        // 表はブロック全体。
        let t = table.anchor.unwrap();
        assert_eq!((t.block_start, t.block_end), (3, 3));
        assert_eq!(slice(&blocks, &t), table.content);
        // parent は位置を持たない。
        assert!(chunks
            .iter()
            .any(|c| c.kind == ChunkKind::Parent && c.anchor.is_none()));
    }

    #[test]
    fn split_paragraph_pieces_have_offsets_inside_the_block() {
        let body = "あいうえお。".repeat(30); // 180 文字
        let blocks = vec![para(&format!(" {body}"))];
        let params = ChunkParams {
            max_leaf_chars: 60,
            ..ChunkParams::default()
        };
        let chunks = chunk_document(node(), 1, &blocks, &params);
        let ls = leaves(&chunks);
        assert!(ls.len() >= 3);
        let mut last_end = 0;
        for leaf in ls {
            let a = leaf.anchor.unwrap();
            assert_eq!((a.block_start, a.block_end), (0, 0));
            assert!(a.off_start >= last_end, "断片は前から順に並ぶ");
            assert_eq!(slice(&blocks, &a), leaf.content);
            last_end = a.off_end;
        }
    }

    #[test]
    fn quote_context_spans_neighbouring_blocks() {
        let blocks = vec![
            para("前の段落の終わり。"),
            heading(2, "提出期限"),
            para("申出書は1か月前までに提出する。"),
            para("期限後も申請できる。"),
        ];
        let params = ChunkParams {
            max_leaf_chars: 18,
            ..ChunkParams::default()
        };
        let chunks = chunk_document(node(), 1, &blocks, &params);
        let leaf = leaves(&chunks)
            .into_iter()
            .find(|c| c.content.starts_with("申出書"))
            .unwrap();
        // 直前は見出し、直後は次の段落の先頭（改行でつなぐ）。
        assert_eq!(leaf.quote_prefix, "提出期限\n");
        assert!(leaf.quote_suffix.starts_with("\n期限後も"));
    }

    #[test]
    fn leaf_collects_pdf_boxes_of_its_blocks() {
        use crate::anchor::{BoxOrigin, PageBox};
        let boxed = |page: i32| PageBox {
            page,
            bbox: [72.0, 700.0, 540.0, 640.0],
            origin: BoxOrigin::BottomLeft,
        };
        let mut a = para("一つ目。");
        a.prov = vec![boxed(1)];
        let mut b = para("二つ目。");
        b.prov = vec![boxed(1), boxed(2)];
        let chunks = chunk_document(node(), 1, &[a, b], &ChunkParams::default());
        let leaf = leaves(&chunks)[0];
        assert_eq!(leaf.boxes, vec![boxed(1), boxed(2)]);
    }

    #[test]
    fn list_items_are_chunked_like_paragraphs() {
        let mut item = para("Workday を開く");
        item.block_type = BlockType::ListItem;
        item.list_marker = Some("1.".into());
        let chunks = chunk_document(node(), 1, &[item], &ChunkParams::default());
        assert_eq!(leaves(&chunks)[0].content, "Workday を開く");
    }
}
