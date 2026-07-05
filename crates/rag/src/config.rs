//! RAG の設定（`SHIKI__RAG__*`）。api 側 AppConfig に埋め込まれる。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagConfig {
    /// false ならインジェスト・パイプラインを起動せず、`POST /search` は 503 を返す。
    #[serde(default)]
    pub enabled: bool,

    /// ingestion-worker のベース URL（/parse /embed /rerank）。
    #[serde(default = "default_worker_base_url")]
    pub worker_base_url: String,

    /// Qdrant REST のベース URL。
    #[serde(default = "default_qdrant_url")]
    pub qdrant_url: String,

    /// 埋め込みモデル版。worker の応答と突合し、不一致はインジェスト拒否（PIT-8）。
    /// Qdrant collection 名にも織り込まれ、変更＝shadow index 再構築＋alias 切替。
    #[serde(default = "default_embedding_model_version")]
    pub embedding_model_version: String,

    /// Tantivy インデックス（index-per-tenant）の永続化ディレクトリ。
    #[serde(default = "default_index_data_dir")]
    pub index_data_dir: String,

    /// パース対象 blob の上限バイト数（worker 側 max_download_bytes と対）。
    #[serde(default = "default_max_parse_bytes")]
    pub max_parse_bytes: i64,

    /// pre-filter に使う可読タグ集合の上限（PIT-1）。超過時は tenant-only へ縮退し
    /// post-filter 全依存＋over-fetch 引き上げで正しさを維持する。OpenFGA ListObjects の
    /// 応答上限（既定 1000）未満に設定し「切り詰められた不完全集合を正として使う」事故を防ぐ。
    #[serde(default = "default_readable_tags_max")]
    pub readable_tags_max: usize,

    /// over-fetch 係数（タグ pre-filter が効いている通常時）。
    #[serde(default = "default_over_fetch_tags")]
    pub over_fetch_tags: usize,

    /// over-fetch 係数（tenant-only 縮退時。post-filter で大きく削られる前提）。
    #[serde(default = "default_over_fetch_tenant_only")]
    pub over_fetch_tenant_only: usize,

    /// `POST /search` の top_k 既定値と上限。
    #[serde(default = "default_top_k")]
    pub default_top_k: usize,
    #[serde(default = "default_max_top_k")]
    pub max_top_k: usize,

    /// reranker へ渡す候補プール数（認可済み候補の上位のみ）。
    #[serde(default = "default_rerank_pool")]
    pub rerank_pool: usize,

    /// インジェスト consumer の並列ジョブ数。
    #[serde(default = "default_consumer_concurrency")]
    pub consumer_concurrency: usize,

    /// outbox → job_queue relay のポーリング間隔（ms）。
    #[serde(default = "default_relay_poll_ms")]
    pub relay_poll_ms: u64,

    /// ジョブの visibility timeout（秒）。1 ジョブの処理上限時間の目安。
    #[serde(default = "default_job_vt_secs")]
    pub job_vt_secs: u64,

    /// ジョブの配信試行上限（超過で DLQ）。
    #[serde(default = "default_job_max_attempts")]
    pub job_max_attempts: i32,
}

fn default_worker_base_url() -> String {
    "http://localhost:8090".into()
}
fn default_qdrant_url() -> String {
    "http://localhost:6333".into()
}
fn default_embedding_model_version() -> String {
    "cl-nagoya/ruri-v3-30m".into()
}
fn default_index_data_dir() -> String {
    "./data/index".into()
}
fn default_max_parse_bytes() -> i64 {
    50 * 1024 * 1024
}
fn default_readable_tags_max() -> usize {
    500
}
fn default_over_fetch_tags() -> usize {
    3
}
fn default_over_fetch_tenant_only() -> usize {
    8
}
fn default_top_k() -> usize {
    8
}
fn default_max_top_k() -> usize {
    50
}
fn default_rerank_pool() -> usize {
    32
}
fn default_consumer_concurrency() -> usize {
    2
}
fn default_relay_poll_ms() -> u64 {
    500
}
fn default_job_vt_secs() -> u64 {
    300
}
fn default_job_max_attempts() -> i32 {
    5
}

impl Default for RagConfig {
    fn default() -> Self {
        RagConfig {
            enabled: false,
            worker_base_url: default_worker_base_url(),
            qdrant_url: default_qdrant_url(),
            embedding_model_version: default_embedding_model_version(),
            index_data_dir: default_index_data_dir(),
            max_parse_bytes: default_max_parse_bytes(),
            readable_tags_max: default_readable_tags_max(),
            over_fetch_tags: default_over_fetch_tags(),
            over_fetch_tenant_only: default_over_fetch_tenant_only(),
            default_top_k: default_top_k(),
            max_top_k: default_max_top_k(),
            rerank_pool: default_rerank_pool(),
            consumer_concurrency: default_consumer_concurrency(),
            relay_poll_ms: default_relay_poll_ms(),
            job_vt_secs: default_job_vt_secs(),
            job_max_attempts: default_job_max_attempts(),
        }
    }
}
