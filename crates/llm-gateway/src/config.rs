//! llm-gateway の設定（プロバイダ・モデルカタログ・Langfuse）。
//!
//! api 層の `LlmConfig`（backend enum）からここへ写す。プロバイダは OpenAI 互換ファースト
//! （APIキーで動く openai-compat＝vLLM もこれで賄う）で、Anthropic / Gemini / 複数 openai-compat を
//! 後から足せる。エアギャップは LiteLLM 等の外部を積まず vLLM 直結のみ（NFR-2 無傷）。

use serde::{Deserialize, Serialize};

/// プロバイダ種別（アダプタ選択）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// OpenAI 互換 `/v1/chat/completions`（vLLM・OpenAI・LiteLLM Proxy 等）。第一級。
    Openai,
    /// Anthropic Messages API 直結。
    Anthropic,
    /// OpenAI Responses API（`/v1/responses`）。Chat Completions を話さないモデル用
    /// （OpenCode Go の GPT 系など）。
    OpenaiResponses,
    /// テスト/CI 用の決定的スタブ（外部依存なし）。
    #[default]
    Stub,
}

impl ProviderKind {
    /// 設定・会計・管理画面で使う文字列表現。
    pub const fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Openai => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenaiResponses => "openai_responses",
            ProviderKind::Stub => "stub",
        }
    }

    /// 思考強度（[`Effort`](crate::model::Effort)）を既定で受け付けるか。
    ///
    /// Chat Completions（vLLM 等の OpenAI 互換）は `reasoning_effort` を知らないモデルが多い
    /// ので既定は送らない。送れるモデルはカタログの `supports_effort` で個別に明示する。
    pub const fn effort_by_default(self) -> bool {
        matches!(
            self,
            ProviderKind::Anthropic | ProviderKind::OpenaiResponses
        )
    }
}

/// 既定（単一 backend 設定から作る）プロバイダの ID。`provider` 未指定のモデルはここへ振る。
pub const DEFAULT_PROVIDER_ID: &str = "default";

/// プロバイダ接続設定。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// プロバイダ ID（モデルの `provider` から引く鍵・管理画面の表示）。省略は [`DEFAULT_PROVIDER_ID`]。
    #[serde(default = "default_provider_id")]
    pub id: String,
    /// 管理画面の表示名（省略は `id`）。
    #[serde(default)]
    pub display_name: Option<String>,
    pub kind: ProviderKind,
    /// ベース URL（openai-compat: `http://vllm:8000/v1`・Anthropic: `https://api.anthropic.com`）。
    #[serde(default)]
    pub base_url: Option<String>,
    /// API キー（環境変数経由で注入。stub/エアギャップ vLLM は不要）。
    #[serde(default)]
    pub api_key: Option<String>,
    /// リクエストタイムアウト秒（既定 120）。
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// 送出する User-Agent（未指定は HTTP クライアントの既定）。呼び出し元の名乗りを
    /// 求めるプロバイダ（OpenCode Go 等）向け。
    #[serde(default)]
    pub user_agent: Option<String>,
    /// 会話ごとの ID（[`GenerateRequest::session_id`]）を載せるヘッダ名
    /// （例 `x-opencode-session`）。未指定なら送らない。
    ///
    /// [`GenerateRequest::session_id`]: crate::model::GenerateRequest::session_id
    #[serde(default)]
    pub session_header: Option<String>,
}

fn default_timeout_secs() -> u64 {
    120
}

fn default_provider_id() -> String {
    DEFAULT_PROVIDER_ID.to_string()
}

impl ProviderConfig {
    /// ID（空なら既定 ID）。`Default` で作った設定は ID が空になるため、ここで補う。
    pub fn effective_id(&self) -> &str {
        if self.id.is_empty() {
            DEFAULT_PROVIDER_ID
        } else {
            &self.id
        }
    }

    /// 管理画面の表示名。
    pub fn label(&self) -> &str {
        self.display_name
            .as_deref()
            .unwrap_or_else(|| self.effective_id())
    }
}

/// モデルカタログの 1 エントリ（許可モデル＋単価）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelEntry {
    /// 論理モデル名（UI/リクエストで使う）。
    pub id: String,
    /// プロバイダ実 ID（アダプタが送出する）。省略時は `id` をそのまま使う。
    #[serde(default)]
    pub real_id: Option<String>,
    /// プロンプト単価（マイクロ USD / 100万トークン）。
    #[serde(default)]
    pub prompt_price_micros_per_mtok: u64,
    /// 補完単価（マイクロ USD / 100万トークン）。
    #[serde(default)]
    pub completion_price_micros_per_mtok: u64,
    /// コンテキスト窓（入力＋出力の合計トークン上限）。未設定なら窓を知らない扱い。
    ///
    /// agent-core が剪定閾値の天井に使う（#529）。窓の小さいモデルでの溢れ失敗を防ぐ。
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
    /// 振り先プロバイダの ID（[`ProviderConfig::id`]）。省略は既定プロバイダ。
    #[serde(default)]
    pub provider: Option<String>,
    /// ユーザー・管理画面に出す名前（省略は `id`）。
    #[serde(default)]
    pub display_name: Option<String>,
    /// モデル選択メニューの 1 行説明（用途の目安）。
    #[serde(default)]
    pub description: Option<String>,
    /// 日本国外で処理されるか（外部 API）。UI に「国外処理」バッジを出す（NFR-10）。
    #[serde(default)]
    pub processed_abroad: bool,
    /// 思考強度を送るか。省略はプロバイダ種別の既定（[`ProviderKind::effort_by_default`]）。
    #[serde(default)]
    pub supports_effort: Option<bool>,
    /// 組織の管理者が明示的に有効にするまで使わせない（AI 設定を一度も保存していない組織でも）。
    ///
    /// 組織の許可リストは管理者が保存した時点で確定し、それ以降にカタログへ足したモデルは
    /// 管理者が有効にするまで使えない。未保存の組織はカタログの既定で動くので、国外処理の
    /// モデルなど同意を取ってから使わせたいものはここを `true` にする。
    #[serde(default)]
    pub requires_opt_in: bool,
}

impl ModelEntry {
    /// 送出する実モデル ID。
    pub fn resolved_id(&self) -> &str {
        self.real_id.as_deref().unwrap_or(&self.id)
    }

    /// 振り先プロバイダの ID。
    pub fn provider_id(&self) -> &str {
        self.provider
            .as_deref()
            .filter(|p| !p.is_empty())
            .unwrap_or(DEFAULT_PROVIDER_ID)
    }

    /// 表示名。
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }

    /// トークン消費からコスト（マイクロ USD）を算出する（整数・金額クリティカル）。
    pub fn cost_usd_micros(&self, prompt_tokens: u64, completion_tokens: u64) -> i64 {
        let prompt = prompt_tokens.saturating_mul(self.prompt_price_micros_per_mtok) / 1_000_000;
        let completion =
            completion_tokens.saturating_mul(self.completion_price_micros_per_mtok) / 1_000_000;
        i64::try_from(prompt.saturating_add(completion)).unwrap_or(i64::MAX)
    }
}

/// モデルカタログ（テナント許可モデル＋既定＋単価）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCatalog {
    /// 既定モデル（リクエストが model 未指定のとき）。
    pub default_model: String,
    pub models: Vec<ModelEntry>,
}

impl ModelCatalog {
    /// 論理モデル名からエントリを引く（未知は既定へフォールバックしない・None）。
    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// 既定モデルのエントリ。
    pub fn default_entry(&self) -> Option<&ModelEntry> {
        self.get(&self.default_model)
    }
}

/// Langfuse（self-host）計装の設定。未設定なら計装は no-op。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangfuseConfig {
    /// 例: `http://langfuse:3000`。
    pub base_url: String,
    pub public_key: String,
    pub secret_key: String,
}

/// 「Auto」（ユーザーがモデルを選ばない既定）の振り先。
///
/// 将来は問い合わせ内容でモデルを選ぶ（Auto Routing）が、当面は固定の 1 モデル＋思考強度に
/// 写す。カタログに無い／テナントで無効なモデルを指していたら既定モデルへ落とす
/// （Auto が「使えない」状態を作らない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoConfig {
    #[serde(default = "default_auto_model")]
    pub model: String,
    #[serde(default = "default_auto_effort")]
    pub effort: crate::model::Effort,
}

fn default_auto_model() -> String {
    "gpt-6-luna".to_string()
}

const fn default_auto_effort() -> crate::model::Effort {
    crate::model::Effort::Low
}

impl Default for AutoConfig {
    fn default() -> Self {
        AutoConfig {
            model: default_auto_model(),
            effort: default_auto_effort(),
        }
    }
}

/// gateway 全体の設定。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// 既定プロバイダ（ID 省略時は [`DEFAULT_PROVIDER_ID`]）。`provider` 未指定のモデルの振り先。
    pub provider: ProviderConfig,
    /// 追加プロバイダ（複数同時利用）。モデルの `provider` で振り分ける。
    #[serde(default)]
    pub extra_providers: Vec<ProviderConfig>,
    pub catalog: ModelCatalog,
    /// Auto の振り先。
    #[serde(default)]
    pub auto: AutoConfig,
    #[serde(default)]
    pub langfuse: Option<LangfuseConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_is_integer_micros() {
        let m = ModelEntry {
            id: "m".into(),
            real_id: None,
            prompt_price_micros_per_mtok: 5_000_000, // $5 / Mtok
            completion_price_micros_per_mtok: 25_000_000, // $25 / Mtok
            context_window_tokens: None,
            ..Default::default()
        };
        // 1000 prompt + 200 completion → 5000 + 5000 = 10000 micros = $0.01
        assert_eq!(m.cost_usd_micros(1000, 200), 10_000);
        assert_eq!(m.resolved_id(), "m");
    }

    #[test]
    fn catalog_lookup() {
        let c = ModelCatalog {
            default_model: "a".into(),
            models: vec![ModelEntry {
                id: "a".into(),
                real_id: Some("real-a".into()),
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
                context_window_tokens: None,
                ..Default::default()
            }],
        };
        assert_eq!(c.get("a").unwrap().resolved_id(), "real-a");
        assert!(c.get("z").is_none());
        assert_eq!(c.default_entry().unwrap().id, "a");
    }
}
