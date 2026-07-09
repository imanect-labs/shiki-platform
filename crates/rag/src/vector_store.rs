//! `VectorStore` トレイト（Task 2.4）。
//!
//! dense 検索の差し替え点（既定 Qdrant・小規模向け pgvector は将来実装）。
//!
//! # テナント分離の不変条件（docs/design.md §4.3）
//!
//! `tenant_id` フィルタは**実装内部で無条件 AND** する。呼び出し側はフィルタを外せず、
//! authz_tags（テナント内 ReBAC 可読性・PIT-1）が空/バグでも別テナントへ届かない。
//! 第一引数は `&AuthContext`（bare な tenant 文字列を引き回さない）。

use async_trait::async_trait;
use authz::AuthContext;
use uuid::Uuid;

use crate::error::RagError;

/// pre-filter の形（PIT-1 (b) 権限定義オブジェクト方式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreFilter {
    /// 可読タグ集合（`file:<t>|<id>` / `folder:<t>|<祖先>`・名前空間化のまま）で絞る。
    Tags(Vec<String>),
    /// 可読集合が上限超過（overflow）した縮退モード。tenant フィルタのみで検索し、
    /// 正しさは post-filter（OpenFGA file check）＋over-fetch が担う。
    TenantOnly,
}

/// 索引へ upsert する 1 チャンク分のベクタ＋ペイロード。
pub struct ChunkPoint {
    pub chunk_id: Uuid,
    pub node_id: Uuid,
    pub version: i64,
    pub vector: Vec<f32>,
    /// 名前空間化形式のまま格納（local へ剥がさない。design §4.3）。
    pub authz_tags: Vec<String>,
}

/// 検索ヒット（ID とスコアのみ。本文は rag_chunk からハイドレーションする）。
#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk_id: Uuid,
    pub node_id: Uuid,
    pub score: f32,
}

/// dense 検索のパラメータ。
pub struct VectorSearch<'a> {
    pub vector: &'a [f32],
    pub limit: usize,
    pub prefilter: &'a PreFilter,
    /// 知識スコープの絞り込みタグ（skill・Task 6.8）。空 = 絞らない。
    ///
    /// **権限境界（prefilter/post-filter）とは独立の AND 句**であり、常に狭める方向にのみ働く
    /// （TenantOnly 縮退時もスコープ句は維持される）。チャンクは祖先フォルダ全ての構造タグを
    /// 持つため、`folder:<t>|<id>` 1 つで配下全ファイルをカバーする。
    pub scope_tags: &'a [String],
    /// バックフィル時の再取得除外（既に取得済みの chunk_id）。
    pub exclude: &'a [Uuid],
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    /// collection（モデル版単位）と alias・payload index を準備する（冪等）。
    /// 次元は初回の埋め込み応答から確定するため引数で受ける。
    async fn ensure_ready(&self, dimension: usize) -> Result<(), RagError>;

    /// チャンク群を upsert する（決定的 chunk_id により再実行は上書き＝冪等）。
    async fn upsert(&self, ctx: &AuthContext, points: &[ChunkPoint]) -> Result<(), RagError>;

    /// ノードの全ベクタを削除する（ファイル削除・テナント内）。
    async fn delete_node(&self, ctx: &AuthContext, node_id: Uuid) -> Result<(), RagError>;

    /// `keep_version` 以外の版のベクタを削除する（版更新後の残骸掃除）。
    async fn delete_stale_versions(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        keep_version: i64,
    ) -> Result<(), RagError>;

    /// ノードの authz_tags ペイロードを再書込する（move 時の再評価。再埋め込み不要）。
    async fn set_authz_tags(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        tags: &[String],
    ) -> Result<(), RagError>;

    /// フィルタ付き ANN 検索。`tenant_id = ctx.tenant_id` は実装が無条件 AND する。
    async fn search(
        &self,
        ctx: &AuthContext,
        query: &VectorSearch<'_>,
    ) -> Result<Vec<ScoredChunk>, RagError>;

    /// テナント消去（SAAS.2）: 当該テナントの全ベクタを削除する。
    async fn purge_tenant(&self, tenant_id: &str) -> Result<(), RagError>;
}
