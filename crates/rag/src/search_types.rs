//! 検索 API の DTO（単一定義・Task 2.10）。
//!
//! api 層はこの型をそのまま utoipa → OpenAPI → TS へ流す（手書きミラー禁止・codegen が正）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// 検索モード。`hybrid` が既定（dense/keyword 単独はデバッグ比較用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Hybrid,
    Dense,
    Keyword,
}

/// `POST /search` のリクエスト。
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SearchRequest {
    /// 検索クエリ（自然文/キーワード）。
    pub query: String,
    /// 返す引用チャンク数（既定 8・上限は設定 `max_top_k`）。
    pub top_k: Option<u32>,
    /// 検索モード（既定 hybrid）。
    pub mode: Option<SearchMode>,
    /// デバッグ情報（各段の件数・所要時間）を含めるか。
    #[serde(default)]
    pub debug: bool,
}

/// 引用チャンク 1 件。`file_name`/`folder_id` は検索時点の現在値（node JOIN）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchResult {
    pub chunk_id: Uuid,
    pub file_id: Uuid,
    pub file_name: String,
    /// 引用元へのジャンプ用（Drive の親フォルダ）。ルート直下は null。
    pub folder_id: Option<Uuid>,
    pub page: Option<i32>,
    pub heading_path: Vec<String>,
    pub content: String,
    /// small-to-big の親チャンク本文（文脈提示用）。
    pub parent_content: Option<String>,
    pub score: f32,
    pub version: i64,
}

/// 各検索段の所要時間（ms）。
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct StageTimings {
    pub readable_set_ms: u64,
    pub embed_ms: u64,
    pub retrieve_ms: u64,
    pub post_filter_ms: u64,
    pub rerank_ms: u64,
    pub hydrate_ms: u64,
}

/// デバッグ情報: どの段で何件に絞られたか（Task 2.10 受入条件「権限で絞られた件数」）。
///
/// 注: `authz_denied_files` は「読めない一致文書の存在」を示すオラクルになり得るため、
/// デバッグ表示は社内基盤前提。公開 API 化時は管理者ロール限定にする（docs/design.md §4.3）。
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct SearchDebug {
    /// `tags`（authz_tags pre-filter）か `tenant_only`（可読集合の上限超過で縮退）か。
    pub prefilter_mode: String,
    pub readable_tags: u32,
    pub dense_hits: u32,
    pub keyword_hits: u32,
    /// RRF 融合・重複排除後の候補数。
    pub fused: u32,
    /// post-filter（OpenFGA file check）で落とされた chunk / file 数。
    pub authz_denied_chunks: u32,
    pub authz_denied_files: u32,
    /// バックフィル（over-fetch 再取得）の回数。
    pub backfill_rounds: u32,
    /// reranker に渡した候補数。
    pub reranked: u32,
    pub stage_ms: StageTimings,
}

/// `POST /search` のレスポンス。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    /// `debug: true` のリクエスト時のみ。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<SearchDebug>,
}
