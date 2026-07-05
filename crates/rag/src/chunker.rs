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
pub fn chunk_document(
    node_id: Uuid,
    version: i64,
    blocks: &[ParsedBlock],
    params: &ChunkParams,
) -> Vec<Chunk> {
    let mut builder = ChunkBuilder::new(node_id, version, params.clone());
    // 見出しスタック（(level, text)）。heading_path はここから導出する。
    let mut headings: Vec<(u32, String)> = Vec::new();

    for block in blocks {
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
                builder.push_table(block, &heading_path(&headings));
            }
            BlockType::Paragraph | BlockType::Caption => {
                builder.push_paragraph(block);
            }
        }
    }
    builder.flush_section(&heading_path(&headings));
    builder.finish()
}

fn heading_path(headings: &[(u32, String)]) -> Vec<String> {
    headings.iter().map(|(_, t)| t.clone()).collect()
}

/// セクション（見出し境界）単位で parent + leaves を組み立てる内部状態。
struct ChunkBuilder {
    node_id: Uuid,
    version: i64,
    params: ChunkParams,
    chunks: Vec<Chunk>,
    ordinal: i32,
    /// 進行中セクションの段落（text, page）。表は即確定するためここには入らない。
    pending: Vec<(String, Option<i32>)>,
    /// 進行中セクションの表チャンク（parent 確定時に parent_id を埋める）。
    pending_tables: Vec<Chunk>,
}

impl ChunkBuilder {
    fn new(node_id: Uuid, version: i64, params: ChunkParams) -> Self {
        ChunkBuilder {
            node_id,
            version,
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

    fn push_paragraph(&mut self, block: &ParsedBlock) {
        let text = block.text.trim();
        if !text.is_empty() {
            self.pending.push((text.to_string(), block.page));
        }
    }

    /// 表は表単位で 1 チャンク（分割禁止）。parent_id はセクション確定時に埋める。
    fn push_table(&mut self, block: &ParsedBlock, path: &[String]) {
        let text = block.text.trim();
        if text.is_empty() {
            return;
        }
        let (id, ordinal) = self.next_id();
        self.pending_tables.push(Chunk {
            id,
            parent_id: None, // flush_section で設定
            kind: ChunkKind::Table,
            ordinal,
            page: block.page,
            heading_path: path.to_vec(),
            content: text.to_string(),
        });
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
            .map(|(t, _)| t.as_str())
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
            .map(|(_, p)| *p)
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
        });

        // 段落を max_leaf_chars まで詰めて leaf 化（段落境界でのみ分割）。
        let mut leaf_text = String::new();
        let mut leaf_page: Option<i32> = None;
        for (text, page) in &paragraphs {
            let would_be = leaf_text.chars().count() + text.chars().count();
            if !leaf_text.is_empty() && would_be > self.params.max_leaf_chars {
                self.emit_leaf(&mut leaf_text, &mut leaf_page, parent_uuid, path);
            }
            if leaf_text.is_empty() {
                leaf_page = *page;
            } else {
                leaf_text.push_str("\n\n");
            }
            leaf_text.push_str(text);
        }
        self.emit_leaf(&mut leaf_text, &mut leaf_page, parent_uuid, path);

        // 表チャンクへ parent を結線して確定する。
        for mut table in tables {
            table.parent_id = Some(parent_uuid);
            self.chunks.push(table);
        }
    }

    fn emit_leaf(
        &mut self,
        text: &mut String,
        page: &mut Option<i32>,
        parent_id: Uuid,
        path: &[String],
    ) {
        if text.is_empty() {
            return;
        }
        let (id, ordinal) = self.next_id();
        self.chunks.push(Chunk {
            id,
            parent_id: Some(parent_id),
            kind: ChunkKind::Leaf,
            ordinal,
            page: page.take(),
            heading_path: path.to_vec(),
            content: std::mem::take(text),
        });
    }

    fn finish(mut self) -> Vec<Chunk> {
        // 出現順（ordinal）で安定させる。表は next_id 先取りのため並べ直しが要る。
        self.chunks.sort_by_key(|c| c.ordinal);
        self.chunks
    }
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
        }
    }

    fn para(text: &str) -> ParsedBlock {
        ParsedBlock {
            block_type: BlockType::Paragraph,
            level: None,
            text: text.into(),
            page: Some(1),
        }
    }

    fn table(text: &str) -> ParsedBlock {
        ParsedBlock {
            block_type: BlockType::Table,
            level: None,
            text: text.into(),
            page: Some(2),
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
}
