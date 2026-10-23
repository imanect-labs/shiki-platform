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
    /// S3 互換の接続設定（`backend=s3` のとき必須。起動時に main で検証）。
    #[serde(default)]
    pub s3: Option<storage::S3Config>,
    /// 1 ファイルの最大アップロードサイズ（バイト）。既定 5 GiB。declare の宣言サイズが
    /// これを超えたら拒否し、容量枯渇（認証ユーザーによる無制限アップロード）を防ぐ。
    #[serde(default = "default_max_upload_size_bytes")]
    pub max_upload_size_bytes: i64,
    /// 中断アップロード（finalize されなかった declare）の回収設定（#468）。
    #[serde(default)]
    pub upload_gc: UploadGcConfig,
}

fn default_max_upload_size_bytes() -> i64 {
    5 * 1024 * 1024 * 1024 // 5 GiB
}

/// 中断アップロードの回収（#468）。
///
/// クライアント切断で finalize が中断されると `pending_upload` 行と staging/incoming
/// オブジェクトが残り、放置するとテナントごとに単調増加する。既定で有効。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UploadGcConfig {
    /// 回収を行うか。既定 true（無効化はストレージが増え続けることを承知した上での運用判断）。
    #[serde(default = "default_upload_gc_enabled")]
    pub enabled: bool,
    /// finalize されないまま経過したら回収するまでの秒数。既定 24 時間。
    ///
    /// presigned PUT の TTL より十分長くする。短くすると、巨大ファイルを低速回線で
    /// アップロード中の declare を回収してしまう。
    #[serde(default = "default_upload_gc_ttl_secs")]
    pub ttl_secs: u64,
    /// sweep の実行間隔（秒）。既定 1 時間。
    #[serde(default = "default_upload_gc_interval_secs")]
    pub interval_secs: u64,
}

fn default_upload_gc_enabled() -> bool {
    true
}

/// 既定オン。評価（Gemma・Luna）で、当時の emit_ui より正確で軽いことを確かめた（#559・emit_ui は #604 で撤去）。
fn default_yui() -> bool {
    true
}

fn default_upload_gc_ttl_secs() -> u64 {
    24 * 60 * 60
}

fn default_upload_gc_interval_secs() -> u64 {
    60 * 60
}

impl Default for UploadGcConfig {
    fn default() -> Self {
        Self {
            enabled: default_upload_gc_enabled(),
            ttl_secs: default_upload_gc_ttl_secs(),
            interval_secs: default_upload_gc_interval_secs(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStoreBackend {
    /// S3 互換（RustFS / MinIO / AWS S3 など）。実装は `storage::S3ObjectStore`。
    ///
    /// **旧名 `minio` も受け付ける。** 実装は最初から S3 互換汎用
    /// （`S3ObjectStore` / `S3Config`）で特定製品に縛られていないが、
    /// enum の名前だけ製品名のままだった。RustFS へ差し替えた時点で実体に
    /// 合わせて改名し、既存の `.env` / TOML を壊さないよう alias を残す。
    #[serde(alias = "minio")]
    S3,
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
    /// 既定プロバイダ（ID `default`）。`models[].provider` を省略したモデルの振り先。
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
    /// 既定モデルのコンテキスト窓トークン数。`models` を書かない構成（Compose の env だけ）で
    /// 合成する単一エントリに載せる（#529）。0 は未設定（env の既定値に空文字を置けないため）。
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
    /// Langfuse（self-host）計装。未設定なら計装は no-op。
    #[serde(default)]
    pub langfuse: Option<LangfuseConfig>,
    /// プロバイダへ送る User-Agent（未指定は HTTP クライアントの既定）。
    #[serde(default)]
    pub user_agent: Option<String>,
    /// 会話（スレッド）ごとの ID を載せるヘッダ名（例 `x-opencode-session`）。未指定なら送らない。
    #[serde(default)]
    pub session_header: Option<String>,
    /// 既定プロバイダの表示名（管理画面・モデル選択の見出し・#583）。未指定は ID の `default`。
    #[serde(default)]
    pub display_name: Option<String>,
    /// 追加プロバイダ（複数同時利用）。`models[].provider` がこの `id` を指す。
    /// 既定プロバイダ（`backend` 等の単一設定）と併用できる。
    #[serde(default)]
    pub providers: Vec<LlmProviderEntry>,
    /// 「Auto」の振り先（既定 `gpt-6-luna`・思考強度 `low`）。カタログに無ければ既定モデルへ落ちる。
    #[serde(default)]
    pub auto: Option<LlmAutoConfig>,
    /// 送信してから応答ヘッダが返るまでの上限秒（既定 60・全プロバイダ共通・#554）。
    #[serde(default = "default_llm_first_response_timeout_secs")]
    pub first_response_timeout_secs: u64,
    /// ストリームのチャンク間の無通信の上限秒（既定 120）。推論モデルの思考中の
    /// ハートビート間隔より長くする。
    #[serde(default = "default_llm_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    /// 1 回の生成全体の上限秒（既定 900 = 15 分・暴走の安全弁）。
    #[serde(default = "default_llm_total_timeout_secs")]
    pub total_timeout_secs: u64,
    /// 利用制限（月次コスト上限・プレビュー環境向け）。既定は無効。
    #[serde(default)]
    pub limits: LlmLimitsConfig,
    /// ChatGPT プラン（Sign in with ChatGPT）での個人利用（プレビュー環境向け・#854）。既定は無効。
    #[serde(default)]
    pub chatgpt: LlmChatGptConfig,
}

/// ChatGPT プランの接続（設定表現・`[llm.chatgpt]`）。既定は無効で、プレビュー環境だけで有効にする。
///
/// 有効にすると、プロバイダ `chatgpt` と `models` のモデル（論理名 `chatgpt:<slug>`）が
/// カタログに加わる。設定画面（接続済みアプリ）から連携した**本人だけ**が使える。
/// 接続はプロセスで 1 件・資格情報は `credential_path` のファイルに置く（単一レプリカ前提）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmChatGptConfig {
    /// 有効にするか（既定 false）。
    #[serde(default)]
    pub enabled: bool,
    /// 資格情報ファイルのパス（0600 で書く。ボリュームが無ければ再デプロイで消える）。
    #[serde(default = "default_chatgpt_credential_path")]
    pub credential_path: String,
    /// カタログに載せるモデル（ChatGPT の `/v1/models` の `slug`）。設定画面に、接続した
    /// アカウントで使えるモデルが出る。環境変数ではカンマ区切りでも書ける。
    #[serde(default, deserialize_with = "slugs")]
    pub models: Vec<String>,
}

fn default_chatgpt_credential_path() -> String {
    "/var/lib/shiki/chatgpt.json".to_string()
}

impl Default for LlmChatGptConfig {
    fn default() -> Self {
        LlmChatGptConfig {
            enabled: false,
            credential_path: default_chatgpt_credential_path(),
            models: Vec::new(),
        }
    }
}

/// 配列か、カンマ区切りの文字列（`SHIKI__LLM__CHATGPT__MODELS=a,b`）を受ける。
fn slugs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Slugs {
        List(Vec<String>),
        Csv(String),
    }
    let list = match Slugs::deserialize(d)? {
        Slugs::List(v) => v,
        Slugs::Csv(s) => s.split(',').map(str::to_string).collect(),
    };
    Ok(list
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// 利用制限（設定表現・`[llm.limits]`）。既定は無効で、プレビュー環境だけで有効にする。
///
/// ユーザー 1 人あたりの月額と、プロバイダ別の環境総枠の二段
/// （[`llm_gateway::UsageLimitsConfig`]）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmLimitsConfig {
    /// 制限を掛けるか（既定 false）。
    #[serde(default)]
    pub enabled: bool,
    /// ユーザー 1 人あたりの月額上限（マイクロ USD）。未設定・0 は無制限。
    #[serde(default)]
    pub per_user_usd_micros: Option<i64>,
    /// プロバイダ別の環境総枠。
    #[serde(default)]
    pub providers: Vec<LlmProviderLimit>,
}

/// 1 つの請求先（API キー）の環境総枠（設定表現）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderLimit {
    /// この枠を共有する `llm.providers[].id`（既定プロバイダは `default`）。
    ///
    /// **枠は API キー単位**。1 つのキーを複数のプロバイダ設定で使っているなら全部列挙する
    /// （OpenCode Go は API 形式ごとにプロバイダを分けるが、キーと請求は 1 つ）。
    pub providers: Vec<String>,
    /// 月額上限（マイクロ USD）。
    pub usd_micros: i64,
}

const fn default_llm_first_response_timeout_secs() -> u64 {
    llm_gateway::LlmTimeouts::DEFAULT_FIRST_RESPONSE_SECS
}

const fn default_llm_idle_timeout_secs() -> u64 {
    llm_gateway::LlmTimeouts::DEFAULT_IDLE_SECS
}

const fn default_llm_total_timeout_secs() -> u64 {
    llm_gateway::LlmTimeouts::DEFAULT_TOTAL_SECS
}

impl Default for LlmConfig {
    fn default() -> Self {
        LlmConfig {
            backend: LlmBackend::default(),
            base_url: None,
            api_key: None,
            default_model: None,
            models: Vec::new(),
            context_window_tokens: None,
            langfuse: None,
            user_agent: None,
            session_header: None,
            display_name: None,
            providers: Vec::new(),
            auto: None,
            first_response_timeout_secs: default_llm_first_response_timeout_secs(),
            idle_timeout_secs: default_llm_idle_timeout_secs(),
            total_timeout_secs: default_llm_total_timeout_secs(),
            limits: LlmLimitsConfig::default(),
            chatgpt: LlmChatGptConfig::default(),
        }
    }
}

/// 追加プロバイダ 1 件（設定表現）。API キーは環境変数で注入する（コード・イメージに置かない）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderEntry {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub backend: LlmBackend,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub session_header: Option<String>,
}

/// Auto の振り先（設定表現）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmAutoConfig {
    pub model: String,
    /// `low` / `medium` / `high`。
    #[serde(default)]
    pub effort: Option<String>,
}

impl LlmConfig {
    /// llm-gateway のタイムアウトへ写す（`validate` で 1 秒以上を保証済み）。
    #[must_use]
    pub const fn timeouts(&self) -> llm_gateway::LlmTimeouts {
        llm_gateway::LlmTimeouts::from_secs(
            self.first_response_timeout_secs,
            self.idle_timeout_secs,
            self.total_timeout_secs,
        )
    }

    /// llm-gateway の利用制限へ写す（`validate` で非負を保証済み）。
    ///
    /// 0 は「無制限」として扱う（env で空文字を置けないため、0 で無効化できるようにする）。
    #[must_use]
    pub fn usage_limits(&self) -> llm_gateway::UsageLimitsConfig {
        llm_gateway::UsageLimitsConfig {
            enabled: self.limits.enabled,
            per_user_usd_micros: self.limits.per_user_usd_micros.filter(|&v| v > 0),
            providers: self
                .limits
                .providers
                .iter()
                .filter(|p| p.usd_micros > 0 && !p.providers.is_empty())
                .map(|p| llm_gateway::ProviderLimit {
                    providers: p.providers.clone(),
                    usd_micros: p.usd_micros,
                })
                .collect(),
        }
    }

    /// llm-gateway のモデルカタログへ写す。既定モデルは `default_model` → カタログ先頭 →
    /// `"default"` の順。`models` が空ならモデル名を素通しする単一エントリを合成し、
    /// `context_window_tokens`（0 は未設定）をそこに載せる。
    #[must_use]
    pub fn model_catalog(&self) -> llm_gateway::ModelCatalog {
        let default_model = self
            .default_model
            .clone()
            .or_else(|| self.models.first().map(|m| m.id.clone()))
            .unwrap_or_else(|| "default".to_string());
        let models = if self.models.is_empty() {
            vec![llm_gateway::ModelEntry {
                id: default_model.clone(),
                real_id: None,
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
                context_window_tokens: self.context_window_tokens.filter(|&w| w > 0),
                ..Default::default()
            }]
        } else {
            self.models
                .iter()
                .map(|m| llm_gateway::ModelEntry {
                    id: m.id.clone(),
                    real_id: m.real_id.clone(),
                    prompt_price_micros_per_mtok: m.prompt_price_micros_per_mtok,
                    completion_price_micros_per_mtok: m.completion_price_micros_per_mtok,
                    cache_read_price_micros_per_mtok: m.cache_read_price_micros_per_mtok,
                    cache_write_price_micros_per_mtok: m.cache_write_price_micros_per_mtok,
                    context_window_tokens: m.context_window_tokens,
                    provider: m.provider.clone(),
                    display_name: m.display_name.clone(),
                    description: m.description.clone(),
                    processed_abroad: m.processed_abroad,
                    supports_effort: m.supports_effort,
                    requires_opt_in: m.requires_opt_in,
                    reasoning_in_content: m.reasoning_in_content,
                })
                .collect()
        };
        let mut models = models;
        models.extend(self.chatgpt_models());
        llm_gateway::ModelCatalog {
            default_model,
            models,
        }
    }

    /// ChatGPT プランのモデル（無効なら空）。単価 0（利用者のプラン枠で払う）・国外処理。
    fn chatgpt_models(&self) -> Vec<llm_gateway::ModelEntry> {
        if !self.chatgpt.enabled {
            return Vec::new();
        }
        self.chatgpt
            .models
            .iter()
            .map(|slug| llm_gateway::ModelEntry {
                id: chatgpt_model_id(slug),
                real_id: Some(slug.clone()),
                provider: Some(llm_gateway::CHATGPT_PROVIDER_ID.to_string()),
                display_name: Some(format!("{slug}（ChatGPT）")),
                description: Some("あなたの ChatGPT プランで実行します".to_string()),
                processed_abroad: true,
                ..Default::default()
            })
            .collect()
    }
}

/// ChatGPT プランのモデルの論理名（他プロバイダの同名モデルと衝突させない）。
pub(crate) fn chatgpt_model_id(slug: &str) -> String {
    format!("chatgpt:{slug}")
}

/// モデルカタログの 1 エントリ（設定表現）。単価はマイクロ USD / 100万トークン。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmModelEntry {
    pub id: String,
    #[serde(default)]
    pub real_id: Option<String>,
    #[serde(default)]
    pub prompt_price_micros_per_mtok: u64,
    #[serde(default)]
    pub completion_price_micros_per_mtok: u64,
    /// キャッシュ読み出しの単価（省略はプロンプト単価・#581）。
    #[serde(default)]
    pub cache_read_price_micros_per_mtok: Option<u64>,
    /// キャッシュ書き込みの単価（省略はプロンプト単価・#581）。
    #[serde(default)]
    pub cache_write_price_micros_per_mtok: Option<u64>,
    /// コンテキスト窓トークン数（未設定ならエージェントの剪定は固定閾値のまま・#529）。
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
    /// 振り先プロバイダの ID（`providers[].id`）。省略は既定プロバイダ。
    #[serde(default)]
    pub provider: Option<String>,
    /// ユーザー・管理画面に出す名前。
    #[serde(default)]
    pub display_name: Option<String>,
    /// モデル選択メニューの 1 行説明。
    #[serde(default)]
    pub description: Option<String>,
    /// 日本国外で処理される（外部 API）。「国外処理」バッジを出す。
    #[serde(default)]
    pub processed_abroad: bool,
    /// 思考強度を送るか（省略はプロバイダ種別の既定）。
    #[serde(default)]
    pub supports_effort: Option<bool>,
    /// 組織の管理者が有効にするまで使わせない（[`llm_gateway::ModelEntry::requires_opt_in`]）。
    #[serde(default)]
    pub requires_opt_in: bool,
    /// 本文の先頭に `<think>…</think>`（`<thought>…</thought>`）で思考を返すモデル
    /// （[`llm_gateway::ModelEntry::reasoning_in_content`]）。
    #[serde(default)]
    pub reasoning_in_content: bool,
}

/// Langfuse 計装の設定（設定表現）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangfuseConfig {
    pub base_url: String,
    pub public_key: String,
    pub secret_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackend {
    /// ローカル vLLM（OpenAI 互換・オンプレ/エアギャップ既定）。
    Vllm,
    /// 任意の OpenAI 互換エンドポイント（OpenAI/LiteLLM Proxy 等）。
    Openai,
    /// Anthropic Messages API 直結。
    Anthropic,
    /// OpenAI Responses API（`/v1/responses`）。Chat Completions を話さないモデル用。
    OpenaiResponses,
    /// Google Gemini（枠・未実装）。
    Gemini,
    /// Vertex AI（枠・未実装）。
    Vertex,
    /// テスト/CI 用の決定的スタブ（外部依存なし）。
    #[default]
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
    /// 回答の中に ```ui ブロック（Yui）で図を書かせるか（既定 true・#530）。
    /// true ならシステムプロンプトの末尾に書き方を足す（`system_prompt` を上書きしていても足す）。
    #[serde(default = "default_yui")]
    pub yui: bool,
    /// 生成リースの秒数。
    #[serde(default = "default_lease_secs")]
    pub lease_secs: i64,
    /// エージェントモードの最大ステップ。
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
    /// 通常チャット 1 発話の累積トークン上限（新規ぶん・未指定は WorkerConfig 既定 30 万・#426）。
    #[serde(default)]
    pub budget_max_tokens: Option<u64>,
    /// 通常チャット 1 発話の累積コスト上限（マイクロ USD・未指定は WorkerConfig 既定 1 USD・#426）。
    #[serde(default)]
    pub budget_max_cost_usd_micros: Option<i64>,
    /// 1 応答の最大トークン（未指定は WorkerConfig 既定）。reasoning 系モデルは思考
    /// トークンも消費するため、長い成果物（レポート執筆・大きなツール引数）には
    /// 8192 以上が必要（2048 だとツール引数が途中で切れて空引数になる・実機で確認）。
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 同一ステップ内で並列実行する冪等 read ツールの上限（未指定は WorkerConfig 既定 4・#349）。
    /// 検索プロバイダの rate limit がきつい環境では 1〜2 に絞る（1 で従来どおりの逐次）。
    #[serde(default)]
    pub parallel_read_tools: Option<usize>,
    /// tool search（出番の限られるツールを `tool_search` で読み込ませる）を使うか
    /// （未指定は WorkerConfig 既定 true）。false で全ツールの定義を常に提示する。
    #[serde(default)]
    pub tool_search: Option<bool>,
    /// system プロンプトに入れる「今日の日付」のタイムゾーン（IANA 名・未指定は
    /// WorkerConfig 既定 `Asia/Tokyo`・#546）。不正な名前は起動時にエラーにする。
    #[serde(default)]
    pub timezone: Option<String>,
    /// 通常チャットで旧・無条件 RAG 注入経路を使う後方互換フォールバック（既定 false）。
    /// false ならモデル裁量ループ（issue #102）。明示的なエージェントモード run/自律 run には影響しない。
    #[serde(default)]
    pub classic_rag: bool,
    /// sandbox-orchestrator の gRPC エンドポイント（未指定なら code_interpreter を提示しない）。
    /// 例: `http://127.0.0.1:50000`。compose 網内・非公開ポート。
    #[serde(default)]
    pub sandbox_endpoint: Option<String>,
    /// コード実行系（code_interpreter / shell）の隔離ティア（admin ポリシー・design §4.6）。
    /// `gvisor`（既定・#346）/ `wasm` / `firecracker`。未指定は既定（gVisor）。各ティアは
    /// orchestrator 側で構成済みであることが前提（未構成なら create は Unimplemented で fail する
    /// ＝黙って降格しない）。runsc の動かない環境は `wasm` を明示指定して退避する。
    /// rootfs の numpy/pandas 同梱はビルド（rootfs-requirements.txt・--require-hashes）が保証する。
    #[serde(default)]
    pub sandbox_backend: Option<sandbox_client::SandboxBackend>,
}

/// CSV クエリサービス（tabular・Task 11P.7）の設定。
///
/// DuckDB 実行は非特権別プロセス（`shiki-tabular-runner`）に隔離する（PIT-39）。
/// `runner_path` はそのバイナリの実行パス（compose ではイメージ内の絶対パス、ローカルは
/// `target/debug/shiki-tabular-runner`）。未指定なら PATH 上の同名を使う。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabularConfig {
    /// 隔離ランナーの実行パス（未指定は `shiki-tabular-runner`＝PATH 解決）。
    #[serde(default = "default_runner_path")]
    pub runner_path: String,
    /// 1 クエリの時間上限（秒・クォータ）。
    #[serde(default = "default_tabular_timeout_secs")]
    pub timeout_secs: u64,
    /// メモリ上限（MB・クォータ）。
    #[serde(default = "default_tabular_memory_mb")]
    pub memory_limit_mb: u32,
    /// 結果の最大行数（クォータ）。
    #[serde(default = "default_tabular_max_rows")]
    pub max_rows: u32,
    /// グリッド無限スクロールの 1 ページ行数。
    #[serde(default = "default_tabular_page_size")]
    pub page_size: u32,
}

fn default_runner_path() -> String {
    "shiki-tabular-runner".to_string()
}
fn default_tabular_timeout_secs() -> u64 {
    20
}
fn default_tabular_memory_mb() -> u32 {
    512
}
fn default_tabular_max_rows() -> u32 {
    10_000
}
fn default_tabular_page_size() -> u32 {
    1_000
}

impl Default for TabularConfig {
    fn default() -> Self {
        TabularConfig {
            runner_path: default_runner_path(),
            timeout_secs: default_tabular_timeout_secs(),
            memory_limit_mb: default_tabular_memory_mb(),
            max_rows: default_tabular_max_rows(),
            page_size: default_tabular_page_size(),
        }
    }
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
            yui: default_yui(),
            lease_secs: default_lease_secs(),
            max_steps: default_max_steps(),
            max_tokens: None,
            budget_max_tokens: None,
            budget_max_cost_usd_micros: None,
            parallel_read_tools: None,
            tool_search: None,
            timezone: None,
            classic_rag: false,
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
