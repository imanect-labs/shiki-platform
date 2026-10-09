//! RAG のドメイン型（パース中間表現・チャンク）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::anchor::{Anchor, PageBox};

/// worker `/parse` が返す構造化ブロックの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockType {
    Heading,
    Paragraph,
    Table,
    Caption,
    /// 箇条書きの 1 項目（`list_marker` に見た目の番号・記号）。
    ListItem,
}

impl TryFrom<String> for BlockType {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        BlockType::parse(&s).ok_or_else(|| format!("unknown block type: {s}"))
    }
}

impl BlockType {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockType::Heading => "heading",
            BlockType::Paragraph => "paragraph",
            BlockType::Table => "table",
            BlockType::Caption => "caption",
            BlockType::ListItem => "list_item",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "heading" => BlockType::Heading,
            "paragraph" => BlockType::Paragraph,
            "table" => BlockType::Table,
            "caption" => BlockType::Caption,
            "list_item" => BlockType::ListItem,
            _ => return None,
        })
    }
}

/// 文書の読み順に並んだ構造化ブロック（パース中間表現）。
///
/// 配列の並び（0 起点）がそのまま版の中のブロック番号（ordinal）になる。`doc_block` に
/// この順で保存し、引用のアンカーはこの番号とブロック内オフセットで指す（#508）。
#[derive(Debug, Clone, Deserialize)]
pub struct ParsedBlock {
    #[serde(rename = "type")]
    pub block_type: BlockType,
    /// heading のみ: 見出しレベル（1 が最上位）。
    pub level: Option<u32>,
    pub text: String,
    pub page: Option<i32>,
    /// PDF など座標を持つ形式のみ: 原本上の枠（worker の prov。charspan は使わない）。
    #[serde(default)]
    pub prov: Vec<PageBox>,
    /// list_item のみ: 見た目の番号・記号。
    #[serde(default)]
    pub list_marker: Option<String>,
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
    /// `doc_block` 上の範囲（parent は持たない・#508）。
    pub anchor: Option<Anchor>,
    /// 引用箇所の前後の文脈（元エディタで一節を探すための TextQuote の prefix / suffix）。
    pub quote_prefix: String,
    pub quote_suffix: String,
    /// PDF の原本上の枠（範囲に含まれるブロックの prov）。
    pub boxes: Vec<PageBox>,
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
