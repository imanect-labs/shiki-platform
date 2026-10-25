//! 検索 API の DTO（単一定義・Task 2.10）。
//!
//! api 層はこの型をそのまま utoipa → OpenAPI → TS へ流す（手書きミラー禁止・codegen が正）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// 検索モード。`hybrid` が既定。`keyword` は型番・固有名詞など表記どおりの一致を引くために
/// モデルにも開いている（doc_search・#423）。`dense` 単独はデバッグ比較用。
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

/// 検索の知識スコープ（skill・Task 6.8）。
///
/// **絞り込み専用**であり権限を広げない: 実効範囲 = スコープ ∩ ユーザー可読集合
/// （pre-filter）で、最終可読性は常に post-filter（OpenFGA file check）が再検証する。
/// folder は配下全体を含む（チャンクは祖先フォルダの構造タグを全て持つ）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchScope {
    /// 参照を許すフォルダ（配下の全ファイルを含む）。
    pub folders: Vec<Uuid>,
    /// 参照を許す個別ファイル。
    pub files: Vec<Uuid>,
}

impl SearchScope {
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.files.is_empty()
    }
}

/// 検索結果 1 件の関連度の目安（reranker の生スコアを設定の閾値で段階に落としたもの・#424）。
///
/// 生スコアは cross-encoder の実装に依存し、数値そのものに意味は無い。モデルへはこの段階だけを
/// 見せる（数値を見せると過信する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Relevance {
    High,
    Medium,
    Low,
}

impl Relevance {
    /// reranker の生スコアを閾値で段階に落とす（`high` 以上 = 高・`low` 未満 = 低・間 = 中）。
    ///
    /// 有限でないスコアは「低」（根拠として扱わない側へ倒す）。`low > high` の設定は
    /// `low = high` として扱う（中が空になるだけで、高と低の意味は崩さない）。
    #[must_use]
    pub fn from_score(score: f32, high: f32, low: f32) -> Self {
        if !score.is_finite() {
            return Relevance::Low;
        }
        if score >= high {
            Relevance::High
        } else if score < low.min(high) {
            Relevance::Low
        } else {
            Relevance::Medium
        }
    }
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
    /// reranker の生スコア（大きいほど関連）。モデル依存でスケールに意味は無い。閾値の較正用。
    pub rerank_score: f32,
    /// `rerank_score` を設定の閾値で段階に落としたもの。
    pub relevance: Relevance,
    pub version: i64,
    /// `doc_block` 上の範囲（位置情報を持つ索引のみ・#508）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<crate::anchor::Anchor>,
    /// 元エディタで探すための一節（前後の文脈つき）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote: Option<crate::anchor::TextQuote>,
    /// PDF のページ上の枠（PDF 以外は空）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub boxes: Vec<crate::anchor::PageBox>,
}

/// 構造化データのレコードを出所とする引用 1 件（Task 15.8 スパイク・docs/data/rag-integration.md）。
///
/// 本文は検索時に post-filter が**現在の値から作り直した**投影（索引のチャンク本文ではない）。
/// 投影列はフィールドマスク対象にできない（data のスキーマ検証）ので、閲覧者ごとの再マスクは要らない。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecordSearchResult {
    pub chunk_id: Uuid,
    pub table_id: Uuid,
    pub record_id: Uuid,
    /// 投影を作ったレコードの版。
    pub rev: i64,
    /// タイトル列の値。
    pub title: String,
    /// 本文列の「表示名: 値」の連結。
    pub content: String,
    pub score: f32,
}

/// 多型の検索結果（file 前提を崩す・Task 15.8）。`kind` で出所を判別する。
///
/// 既存の [`SearchResult`] は `file` の中身としてそのまま使う（ファイル検索の応答形は変えない）。
/// API への公開（`SearchResponse.results` の置き換え）はポストアルファの別タスク。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchHit {
    File(SearchResult),
    Record(RecordSearchResult),
}

/// 版のブロック列の 1 ブロック（出典パネルの描画用・#508）。
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct DocBlock {
    /// 版の中のブロック番号（アンカーの `block_start` / `block_end` と同じ基準）。
    pub ordinal: i32,
    #[sqlx(rename = "type", try_from = "String")]
    #[serde(rename = "type")]
    pub block_type: crate::types::BlockType,
    /// heading のみ: 見出しレベル（1 が最上位）。
    pub level: Option<i32>,
    pub text: String,
    /// list_item のみ: 見た目の番号・記号。
    pub list_marker: Option<String>,
    pub page: Option<i32>,
    #[sqlx(json)]
    pub prov: Vec<crate::anchor::PageBox>,
}

/// `GET /files/{id}/versions/{version}/blocks` のレスポンス（ordinal 順の窓）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DocBlocksPage {
    pub blocks: Vec<DocBlock>,
    /// 窓の後ろに続きがあれば、次に `from` へ渡す ordinal。
    pub next_from: Option<i32>,
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
    /// post-filter は通ったが hydrate で落ちた chunk 数（他 org＝PIT-45 / 索引より先に削除済み）。
    /// バックフィルが埋め直すので結果は欠けないが、恒常的に大きいなら索引が実体とずれている合図。
    pub hydrate_dropped: u32,
    /// reranker に渡した候補数。
    pub reranked: u32,
    /// reranker の生スコアが設定の下限（`min_rerank_score`）未満で落としたヒット数（#100）。
    pub below_min_score: u32,
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

#[cfg(test)]
mod tests {
    use super::Relevance;

    #[test]
    fn relevance_bands_include_lower_bound_of_each_band() {
        let band = |s| Relevance::from_score(s, 0.55, 0.45);
        assert_eq!(band(0.55), Relevance::High);
        assert_eq!(band(0.549), Relevance::Medium);
        assert_eq!(band(0.45), Relevance::Medium);
        assert_eq!(band(0.449), Relevance::Low);
        assert_eq!(band(f32::NAN), Relevance::Low);
        assert_eq!(band(f32::INFINITY), Relevance::Low);
    }

    #[test]
    fn inverted_thresholds_keep_high_and_low_meaning() {
        // low > high の誤設定: 中が空になる。
        let band = |s| Relevance::from_score(s, 0.4, 0.45);
        assert_eq!(band(0.42), Relevance::High);
        assert_eq!(band(0.39), Relevance::Low);
    }
}
