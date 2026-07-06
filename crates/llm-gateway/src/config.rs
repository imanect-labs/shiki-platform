//! llm-gateway の設定（プロバイダ・モデルカタログ・Langfuse）。
//!
//! api 層の `LlmConfig`（backend enum）からここへ写す。プロバイダは OpenAI 互換ファースト
//! （APIキーで動く openai-compat＝vLLM もこれで賄う）で、Anthropic / Gemini / 複数 openai-compat を
//! 後から足せる。エアギャップは LiteLLM 等の外部を積まず vLLM 直結のみ（NFR-2 無傷）。

use serde::{Deserialize, Serialize};

/// プロバイダ種別（アダプタ選択）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// OpenAI 互換 `/v1/chat/completions`（vLLM・OpenAI・LiteLLM Proxy 等）。第一級。
    Openai,
    /// Anthropic Messages API 直結。
    Anthropic,
    /// テスト/CI 用の決定的スタブ（外部依存なし）。
    Stub,
}

/// プロバイダ接続設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
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
}

fn default_timeout_secs() -> u64 {
    120
}

/// モデルカタログの 1 エントリ（許可モデル＋単価）。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl ModelEntry {
    /// 送出する実モデル ID。
    pub fn resolved_id(&self) -> &str {
        self.real_id.as_deref().unwrap_or(&self.id)
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// gateway 全体の設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub provider: ProviderConfig,
    pub catalog: ModelCatalog,
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
            }],
        };
        assert_eq!(c.get("a").unwrap().resolved_id(), "real-a");
        assert!(c.get("z").is_none());
        assert_eq!(c.default_entry().unwrap().id, "a");
    }
}
