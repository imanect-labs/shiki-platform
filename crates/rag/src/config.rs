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

    /// ingestion-worker に送る共有トークン（`Authorization: Bearer`・#812）。worker 側の
    /// `WORKER_AUTH_TOKEN` と同じ値にする。未設定（空）は無認証で送り、起動時に警告する。
    #[serde(default, deserialize_with = "empty_str_as_none", skip_serializing)]
    pub worker_auth_token: Option<String>,

    /// Qdrant REST のベース URL。
    #[serde(default = "default_qdrant_url")]
    pub qdrant_url: String,

    /// Qdrant の API キー（`api-key` ヘッダ・#812）。Qdrant 側の `QDRANT__SERVICE__API_KEY` と
    /// 同じ値にする。未設定（空）は無認証で送り、起動時に警告する。
    #[serde(default, deserialize_with = "empty_str_as_none", skip_serializing)]
    pub qdrant_api_key: Option<String>,

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
    ///
    /// reranker（CPU の cross-encoder）の時間は候補数に比例する。既定 16 は #561 の実測で
    /// 決めた: 32 と比べて再ランキングがほぼ半分（2.8 → 1.6 秒）で、上位 8 件の質の差は
    /// nDCG@8 で 0.832 → 0.809、直接答える一致の数は同じ。品質を優先するなら 32 に上げる。
    #[serde(default = "default_rerank_pool")]
    pub rerank_pool: usize,

    /// 関連度ラベルの閾値（reranker の生スコア・#424）。`relevance_high` 以上を「高」、
    /// `relevance_low` 未満を「低」、その間を「中」とする。**reranker のモデルに依存する**ので、
    /// ingestion-worker の `RERANK_MODEL` を替えたら実クエリで測り直す（既定値は
    /// `hotchpotch/japanese-reranker-cross-encoder-xsmall-v1` での実測・#424）。
    #[serde(default = "default_relevance_high")]
    pub relevance_high: f32,
    #[serde(default = "default_relevance_low")]
    pub relevance_low: f32,
    /// reranker の生スコアがこれ未満のヒットを結果から落とす（#100）。既定 None（足切りしない）。
    ///
    /// 足切りで top_k を割るのは正しい挙動（関連の薄いヒットで無理に埋めない）。既定を無効に
    /// しているのは、#424 の較正で無関係なヒットにも 0.57 程度のスコアが付いていたため
    /// （閾値を上げると関連あるヒットも落ちる）。モデルとセットで実データから決めること。
    #[serde(default, deserialize_with = "empty_as_none")]
    pub min_rerank_score: Option<f32>,
    /// 上の閾値を測った reranker のモデル名。worker が返す `model_version` と違えば警告する
    /// （閾値はモデルごとのスコアのスケールに依存し、食い違うと全件「低」等に化ける）。
    #[serde(default = "default_relevance_model")]
    pub relevance_model: String,

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

/// compose の `ingestion-worker` はホスト側も 8000（コンテナと同じ）に publish する。
/// 8090 は `SHIKI__GATEWAY__PORT` の既定値＝自分自身の第2リスナなので、ここを 8090 に
/// すると native 起動時に server が自分へ /parse を投げることになる（#453）。
fn default_worker_base_url() -> String {
    "http://localhost:8000".into()
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
    16
}
fn default_relevance_high() -> f32 {
    0.55
}
fn default_relevance_low() -> f32 {
    0.45
}
/// 数値か数値の文字列を受け、空文字は None にする（compose は未設定の env を空文字で渡す）。
fn empty_as_none<'de, D>(d: D) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Num(f32),
        Str(String),
    }
    match Option::<Raw>::deserialize(d)? {
        None => Ok(None),
        Some(Raw::Num(n)) => Ok(Some(n)),
        Some(Raw::Str(s)) if s.trim().is_empty() => Ok(None),
        Some(Raw::Str(s)) => s
            .trim()
            .parse()
            .map(Some)
            .map_err(|e| serde::de::Error::custom(format!("min_rerank_score: {e}"))),
    }
}

/// 空文字（compose が未設定の env を渡す形）を None にする。秘密値の「未設定」判定用。
fn empty_str_as_none<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(d)?.filter(|s| !s.trim().is_empty()))
}

fn default_relevance_model() -> String {
    "hotchpotch/japanese-reranker-cross-encoder-xsmall-v1".to_string()
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
            worker_auth_token: None,
            qdrant_url: default_qdrant_url(),
            qdrant_api_key: None,
            embedding_model_version: default_embedding_model_version(),
            index_data_dir: default_index_data_dir(),
            max_parse_bytes: default_max_parse_bytes(),
            readable_tags_max: default_readable_tags_max(),
            over_fetch_tags: default_over_fetch_tags(),
            over_fetch_tenant_only: default_over_fetch_tenant_only(),
            default_top_k: default_top_k(),
            max_top_k: default_max_top_k(),
            rerank_pool: default_rerank_pool(),
            relevance_high: default_relevance_high(),
            relevance_low: default_relevance_low(),
            min_rerank_score: None,
            relevance_model: default_relevance_model(),
            consumer_concurrency: default_consumer_concurrency(),
            relay_poll_ms: default_relay_poll_ms(),
            job_vt_secs: default_job_vt_secs(),
            job_max_attempts: default_job_max_attempts(),
        }
    }
}
