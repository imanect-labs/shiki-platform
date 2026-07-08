//! 差し替え点（backend enum）と周辺設定: authz/telemetry/storage/vector/llm。
//!
//! クラウド/オンプレの差は各 `*Backend` enum の値として設定で表現する（docs/design.md §3.1）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthzConfig {
    /// OpenFGA HTTP API ベース URL（必須）。
    pub base_url: String,
    pub store_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// OTLP エクスポート先（例: `http://otel-collector:4317`）。未指定なら OTel 無効。
    pub otlp_endpoint: Option<String>,
    pub service_name: String,
    /// ログ出力形式（`json` or `pretty`）。
    pub log_format: LogFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub backend: ObjectStoreBackend,
    /// MinIO/S3 接続設定（`backend=minio` のとき必須。起動時に main で検証）。
    #[serde(default)]
    pub s3: Option<storage::S3Config>,
    /// 1 ファイルの最大アップロードサイズ（バイト）。既定 5 GiB。declare の宣言サイズが
    /// これを超えたら拒否し、容量枯渇（認証ユーザーによる無制限アップロード）を防ぐ。
    #[serde(default = "default_max_upload_size_bytes")]
    pub max_upload_size_bytes: i64,
}

fn default_max_upload_size_bytes() -> i64 {
    5 * 1024 * 1024 * 1024 // 5 GiB
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStoreBackend {
    Minio,
    Gcs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorConfig {
    pub backend: VectorStoreBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorStoreBackend {
    Qdrant,
    Pgvector,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub backend: LlmBackend,
    /// プロバイダのベース URL（openai-compat/vLLM: `http://vllm:8000/v1`・Anthropic: 省略可）。
    #[serde(default)]
    pub base_url: Option<String>,
    /// API キー（環境変数経由で注入。stub/エアギャップ vLLM は不要）。
    #[serde(default)]
    pub api_key: Option<String>,
    /// 既定モデル（論理名）。未指定はカタログ先頭。
    #[serde(default)]
    pub default_model: Option<String>,
    /// モデルカタログ（許可モデル＋単価）。空ならモデル名を素通しする単一エントリを合成する。
    #[serde(default)]
    pub models: Vec<LlmModelEntry>,
    /// Langfuse（self-host）計装。未設定なら計装は no-op。
    #[serde(default)]
    pub langfuse: Option<LangfuseConfig>,
}

/// モデルカタログの 1 エントリ（設定表現）。単価はマイクロ USD / 100万トークン。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmModelEntry {
    pub id: String,
    #[serde(default)]
    pub real_id: Option<String>,
    #[serde(default)]
    pub prompt_price_micros_per_mtok: u64,
    #[serde(default)]
    pub completion_price_micros_per_mtok: u64,
}

/// Langfuse 計装の設定（設定表現）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangfuseConfig {
    pub base_url: String,
    pub public_key: String,
    pub secret_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackend {
    /// ローカル vLLM（OpenAI 互換・オンプレ/エアギャップ既定）。
    Vllm,
    /// 任意の OpenAI 互換エンドポイント（OpenAI/LiteLLM Proxy 等）。
    Openai,
    /// Anthropic Messages API 直結。
    Anthropic,
    /// Google Gemini（枠・未実装）。
    Gemini,
    /// Vertex AI（枠・未実装）。
    Vertex,
    /// テスト/CI 用の決定的スタブ（外部依存なし）。
    Stub,
}

/// チャット（生成ワーカー・接続非依存生成）の設定。既定は無効。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConfig {
    /// チャット機能を有効化するか（無効なら /threads 系は 503）。
    #[serde(default)]
    pub enabled: bool,
    /// Redis pub/sub の URL（未指定なら session.redis_url を使う）。
    #[serde(default)]
    pub redis_url: Option<String>,
    /// 生成ワーカーの並行数。
    #[serde(default = "default_worker_concurrency")]
    pub worker_concurrency: usize,
    /// システムプロンプト（未指定は既定）。
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// 生成リースの秒数。
    #[serde(default = "default_lease_secs")]
    pub lease_secs: i64,
    /// エージェントモードの最大ステップ。
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
    /// sandbox-orchestrator の gRPC エンドポイント（未指定なら code_interpreter を提示しない）。
    /// 例: `http://127.0.0.1:50000`。compose 網内・非公開ポート。
    #[serde(default)]
    pub sandbox_endpoint: Option<String>,
    /// コード実行系（code_interpreter / shell）の隔離ティア（admin ポリシー・design §4.6）。
    /// `wasm`（既定）/ `gvisor` / `firecracker`。未指定は wasm。gVisor/FC は orchestrator 側で
    /// 当該ティアが構成済みであることが前提（未構成なら create は Unimplemented で fail する）。
    /// web_fetch は egress 限定の短命 sandbox なので常に wasm（この設定の対象外）。
    #[serde(default)]
    pub sandbox_backend: Option<sandbox_client::SandboxBackend>,
}

/// web 検索プロバイダ（web_search / web_fetch ツール・Phase 4）。
///
/// クラウド/オンプレの差は backend の値で吸収する（SaaS=Brave / オンプレ=SearXNG /
/// テスト・エアギャップ=Stub）。既定は `None`＝web ツールを提示しない。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebSearchConfig {
    /// プロバイダ選択（未指定なら web ツール無効）。
    #[serde(default)]
    pub backend: Option<WebSearchBackend>,
    /// Brave Search API キー（`backend=brave` のとき必須。将来 crates/secrets へ移行）。
    #[serde(default)]
    pub brave_api_key: Option<String>,
    /// SearXNG ベース URL（`backend=searxng` のとき必須。例 `http://searxng:8080`）。
    #[serde(default)]
    pub searxng_base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchBackend {
    /// Brave Search API（SaaS）。
    Brave,
    /// 自己ホスト SearXNG（オンプレ）。
    Searxng,
    /// 決定的スタブ（テスト/CI/エアギャップ）。
    Stub,
}

impl Default for ChatConfig {
    fn default() -> Self {
        ChatConfig {
            enabled: false,
            redis_url: None,
            worker_concurrency: default_worker_concurrency(),
            system_prompt: None,
            lease_secs: default_lease_secs(),
            max_steps: default_max_steps(),
            sandbox_endpoint: None,
            sandbox_backend: None,
        }
    }
}

fn default_worker_concurrency() -> usize {
    4
}

fn default_lease_secs() -> i64 {
    30
}

fn default_max_steps() -> usize {
    6
}
