//! `FulltextIndex` トレイト（Task 2.5）。
//!
//! keyword（BM25）検索の差し替え点。既定は Tantivy＋Lindera
//! （[`TantivyFulltext`](crate::fulltext_tantivy::TantivyFulltext)）。
//!
//! # テナント分離の不変条件（docs/design.md §4.3）
//!
//! 既定は **index-per-tenant**。テナント境界は「どの index を開くか」で強制され、
//! authz_tags と独立の防壁になる（PIT-8 の shadow 切替とも相性が良い）。
//! メソッドは同期（Tantivy が同期 API）。非同期文脈からは `spawn_blocking` で呼ぶ。

use authz::AuthContext;
use uuid::Uuid;

use crate::error::RagError;
use crate::vector_store::{PreFilter, ScoredChunk};

/// 全文索引へ入れる 1 チャンク分のドキュメント。
pub struct FulltextDoc<'a> {
    pub chunk_id: Uuid,
    pub node_id: Uuid,
    pub version: i64,
    /// 検索対象テキスト（`Chunk::searchable_text()`）。
    pub text: &'a str,
    /// 名前空間化形式のまま（dense 側と同じ権限境界を keyword 側にも張る）。
    pub authz_tags: &'a [String],
}

pub trait FulltextIndex: Send + Sync {
    /// ノードのチャンク群を差し替える（既存の同 node 文書を全削除 → 追加 → commit）。
    /// 決定的 chunk_id と合わせて再実行も冪等。move 時のタグ再評価もこれで行う
    /// （本文は rag_chunk が正本なので再投入するだけ）。
    fn replace_node(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        docs: &[FulltextDoc<'_>],
    ) -> Result<(), RagError>;

    /// ノードの全文書を索引から削除する。
    fn delete_node(&self, ctx: &AuthContext, node_id: Uuid) -> Result<(), RagError>;

    /// BM25 検索。tenant 境界は index 選択で強制（authz_tags と独立）。
    ///
    /// `scope_tags` は知識スコープ（skill・Task 6.8）の絞り込み（空 = 絞らない）。
    /// 権限境界（prefilter）と独立の AND 句で、常に狭める方向にのみ働く。
    fn search(
        &self,
        ctx: &AuthContext,
        query_text: &str,
        limit: usize,
        prefilter: &PreFilter,
        scope_tags: &[String],
        exclude: &[Uuid],
    ) -> Result<Vec<ScoredChunk>, RagError>;

    /// テナント消去（SAAS.2）: 当該テナントの index を破棄する。
    fn purge_tenant(&self, tenant_id: &str) -> Result<(), RagError>;
}
