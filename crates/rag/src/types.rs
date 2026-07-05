//! RAG のドメイン型（パース中間表現・チャンク）。

use serde::Deserialize;
use uuid::Uuid;

/// worker `/parse` が返す構造化ブロックの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockType {
    Heading,
    Paragraph,
    Table,
    Caption,
}

/// 文書の読み順に並んだ構造化ブロック（パース中間表現）。
#[derive(Debug, Clone, Deserialize)]
pub struct ParsedBlock {
    #[serde(rename = "type")]
    pub block_type: BlockType,
    /// heading のみ: 見出しレベル（1 が最上位）。
    pub level: Option<u32>,
    pub text: String,
    pub page: Option<i32>,
}

/// パース結果（DocumentParser の出力）。
#[derive(Debug, Clone, Deserialize)]
pub struct ParsedDocument {
    pub blocks: Vec<ParsedBlock>,
    #[serde(default)]
    pub used_ocr: bool,
}

/// チャンク種別。検索対象は leaf / table、parent は small-to-big の文脈提示用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Parent,
    Leaf,
    Table,
}

impl ChunkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChunkKind::Parent => "parent",
            ChunkKind::Leaf => "leaf",
            ChunkKind::Table => "table",
        }
    }
}

/// チャンク化の出力（rag_chunk 行と 1:1）。
///
/// `id` は `uuid5(node_id, version, ordinal)` の決定的生成で、同一版の再インジェストは
/// 同じ ID 群になる（Qdrant/DB とも上書き＝冪等）。
#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: Uuid,
    /// small-to-big の親チャンク（parent 行自身は None）。
    pub parent_id: Option<Uuid>,
    pub kind: ChunkKind,
    /// 文書内の出現順（node_id, version 内で一意）。
    pub ordinal: i32,
    pub page: Option<i32>,
    pub heading_path: Vec<String>,
    pub content: String,
}

impl Chunk {
    /// 埋め込み・全文索引に使う検索用テキスト（見出し文脈を前置して精度を上げる）。
    pub fn searchable_text(&self) -> String {
        if self.heading_path.is_empty() {
            self.content.clone()
        } else {
            format!("{}\n{}", self.heading_path.join(" > "), self.content)
        }
    }
}
