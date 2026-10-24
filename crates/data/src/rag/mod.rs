//! 構造化データの permission-aware 検索（RAG 統合スパイク・Task 15.8・docs/data/rag-integration.md）。
//!
//! テーブル単位のオプトイン（[`crate::TableSchema::rag`]）で、**レコード 1 件 = 1 チャンク**を
//! 投影テンプレート（タイトル列＋本文列）から作る。認可は既存の二段 authz と同じ形に載せる。
//!
//! | 段 | 何で絞るか | OpenFGA のタプル |
//! |---|---|---|
//! | pre-filter | チャンクの構造タグ `data_table:<t>\|<table_id>` ∩ `ListObjects(viewer, data_table)` | 既存（テーブル ReBAC）のみ |
//! | post-filter | 候補 `record_id` を**可視集合に絞るクエリ 1 本**（テーブル viewer ＋ v1 の行述語 ＋ 個別共有） | **1 本も増やさない** |
//!
//! - [`projection`] — 投影テンプレートの検証（マスク列・派生列を拒否）・世代（`generation`）・本文の生成
//! - [`visible`] — post-filter の入口 [`crate::DataStore::rag_visible_records`]
//! - [`indexer`] — 15.6 の outbox に相乗りする増分索引（台帳 consumer [`RAG_INDEX_CONSUMER`]）
//!
//! # フィールドマスクの穴を fail-closed に倒す（data-platform §11.1）
//!
//! チャンクは書込時に静的に作るので、投影にマスク対象列を入れると、行が見えるユーザーには
//! マスク列の値まで回答・引用に流れる。v1 は次の 3 段で塞ぐ。
//!
//! 1. **スキーマ検証**: 投影に `field_policy` の対象列・派生列（lookup / computed / rollup / formula）を
//!    含めると 400（[`projection::validate_projection`]）
//! 2. **世代**: `field_policy` と投影の定義から世代を作り、チャンクに刻む。世代が変わったら索引を破棄して
//!    再構築する（[`indexer`]）。**再構築が終わるまでの古いチャンクは post-filter が世代不一致で落とす**
//! 3. **実行時の再確認**: post-filter は閲覧者のマスク列と投影列の交差を確かめ、交差があれば全件を落とす
//!    （1 が壊れても値を出さない）

pub(crate) mod indexer;
pub(crate) mod projection;
pub(crate) mod visible;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use indexer::{
    RagIndexWorker, RecordChunk, RecordChunkSink, RAG_INDEX_BATCH, RAG_INDEX_CONSUMER,
};
pub use visible::{RagRecordHit, RagVisibleRecords, MAX_RAG_POST_FILTER_IDS};

/// 本文列の上限。
pub const MAX_RAG_BODY_FIELDS: usize = 8;
/// 1 チャンク（タイトル＋本文）の文字数の上限（巨大チャンクで意味検索が希釈されないように・PIT-67）。
pub const MAX_RAG_CHUNK_CHARS: usize = 4_000;

/// 検索（RAG）の投影テンプレート（テーブル単位のオプトイン）。
///
/// レコード 1 件をタイトル 1 列と本文の列群（宣言順）から 1 チャンクへ投影する。投影できるのは
/// 保存値を持つスカラー/選択/参照名の列だけで、**フィールドマスク対象列と派生列は指定できない**
/// （閲覧者ごとに見える値が違う列を、全閲覧者共通のチャンクへ焼き込まない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RagProjection {
    /// タイトル列の参照名（`key`）。
    pub title_field: String,
    /// 本文列の参照名（`key`・0〜[`MAX_RAG_BODY_FIELDS`] 列・宣言順に「表示名: 値」で連結する）。
    #[serde(default)]
    pub body_fields: Vec<String>,
}
