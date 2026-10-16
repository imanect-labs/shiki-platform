//! llm-gateway の設定（プロバイダ・モデルカタログ・Langfuse）。
//!
//! api 層の `LlmConfig`（backend enum）からここへ写す。プロバイダは OpenAI 互換ファースト
//! （APIキーで動く openai-compat＝vLLM もこれで賄う）で、Anthropic / Gemini / 複数 openai-compat を
//! 後から足せる。エアギャップは LiteLLM 等の外部を積まず vLLM 直結のみ（NFR-2 無傷）。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::Usage;

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
    /// プロンプトキャッシュ読み出しの単価（マイクロ USD / 100万トークン）。省略はプロンプト単価
    /// （割引なしで数える・#581 以前と同じ）。
    #[serde(default)]
    pub cache_read_price_micros_per_mtok: Option<u64>,
    /// プロンプトキャッシュ書き込みの単価（マイクロ USD / 100万トークン）。省略はプロンプト単価。
    #[serde(default)]
    pub cache_write_price_micros_per_mtok: Option<u64>,
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
    /// 思考を `reasoning_content` ではなく本文の先頭に `<think>…</think>` で返すモデルか
    /// （MiniMax M3 等・#584）。true のときだけ Chat Completions アダプタが本文先頭のタグを
    /// 思考へ振り分ける。既定 false（先頭から `<think>` を書かせる正規の出力を、思考と
    /// 取り違えて回答から消さないため）。
    #[serde(default)]
    pub reasoning_in_content: bool,
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

    /// キャッシュ読み出しの実効単価（未設定はプロンプト単価）。
    pub fn cache_read_price(&self) -> u64 {
        self.cache_read_price_micros_per_mtok
            .unwrap_or(self.prompt_price_micros_per_mtok)
    }

    /// キャッシュ書き込みの実効単価（未設定はプロンプト単価）。
    pub fn cache_write_price(&self) -> u64 {
        self.cache_write_price_micros_per_mtok
            .unwrap_or(self.prompt_price_micros_per_mtok)
    }

    /// トークン消費からコスト（マイクロ USD）を算出する（整数・金額クリティカル）。
    ///
    /// 入力はキャッシュの内訳ごとに単価を分ける（定価の入力・読み出し・書き込み・#581）。
    /// 内訳は総数の内側へ収めてから数える（[`Usage::normalized`]）。
    /// 各項の積を合計してから 1 度だけ 100 万で割る（項ごとに切り捨てると小さな呼び出しで
    /// 端数の取りこぼしが積もる）。
    pub fn cost_usd_micros(&self, usage: &Usage) -> i64 {
        let usage = usage.normalized();
        let terms = [
            (
                usage.uncached_prompt_tokens(),
                self.prompt_price_micros_per_mtok,
            ),
            (usage.cache_read_tokens, self.cache_read_price()),
            (usage.cache_write_tokens, self.cache_write_price()),
            (
                usage.completion_tokens,
                self.completion_price_micros_per_mtok,
            ),
        ];
        let total = terms.iter().fold(0u128, |acc, &(tokens, price)| {
            acc.saturating_add(u128::from(tokens) * u128::from(price))
        }) / 1_000_000;
        i64::try_from(total).unwrap_or(i64::MAX)
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

/// LLM 呼び出しのタイムアウト（全プロバイダ共通・#554）。測り方は
/// [`providers`](crate::providers) の各アダプタが共通のヘルパで行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTimeouts {
    /// 送信してから応答ヘッダが返るまで（接続を含む）。
    pub first_response: Duration,
    /// ストリームのチャンクとチャンクの間隔（推論モデルの思考中のハートビート間隔より長く）。
    pub idle: Duration,
    /// 1 回の生成全体の上限（暴走の安全弁）。gateway の再試行は試行ごとに測り直すので、
    /// 利用者から見た待ちの上限は試行回数分になる。
    pub total: Duration,
}

impl LlmTimeouts {
    /// 既定の最初の応答（60 秒）。
    pub const DEFAULT_FIRST_RESPONSE_SECS: u64 = 60;
    /// 既定の無通信（120 秒）。
    pub const DEFAULT_IDLE_SECS: u64 = 120;
    /// 既定の全体（15 分）。
    pub const DEFAULT_TOTAL_SECS: u64 = 15 * 60;

    /// 秒で組む（設定値から写す用）。
    pub const fn from_secs(first_response: u64, idle: u64, total: u64) -> Self {
        LlmTimeouts {
            first_response: Duration::from_secs(first_response),
            idle: Duration::from_secs(idle),
            total: Duration::from_secs(total),
        }
    }
}

impl Default for LlmTimeouts {
    fn default() -> Self {
        LlmTimeouts::from_secs(
            Self::DEFAULT_FIRST_RESPONSE_SECS,
            Self::DEFAULT_IDLE_SECS,
            Self::DEFAULT_TOTAL_SECS,
        )
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
    /// LLM 呼び出しのタイムアウト（最初の応答・無通信・全体）。
    #[serde(default)]
    pub timeouts: LlmTimeouts,
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
        assert_eq!(m.cost_usd_micros(&usage(1000, 200, 0, 0)), 10_000);
        assert_eq!(m.resolved_id(), "m");
    }

    fn usage(prompt: u64, completion: u64, read: u64, write: u64) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_tokens: read,
            cache_write_tokens: write,
        }
    }

    /// OpenRouter・openai/gpt-5.6-luna の実測（#581）: 入力 $0.20・読み出し $0.02・
    /// 書き込み $0.25・出力 $1.20 per Mtok。
    fn luna() -> ModelEntry {
        ModelEntry {
            id: "luna".into(),
            prompt_price_micros_per_mtok: 200_000,
            completion_price_micros_per_mtok: 1_200_000,
            cache_read_price_micros_per_mtok: Some(20_000),
            cache_write_price_micros_per_mtok: Some(250_000),
            ..Default::default()
        }
    }

    #[test]
    fn cache_tokens_use_their_own_prices() {
        let m = luna();
        // 1 回目: 2907 入力のうち 2904 を書き込み（3×0.2 + 2904×0.25 = 726.6 → 726）。
        // 出力 0 で比べる（実請求 $0.000733 は出力込み）。
        assert_eq!(m.cost_usd_micros(&usage(2907, 0, 0, 2904)), 726);
        // 2 回目: 2893 を読み出し・11 を書き込み・残り 3 が定価
        // （3×0.2 + 2893×0.02 + 11×0.25 = 0.6 + 57.86 + 2.75 = 61.21 → 61）。
        assert_eq!(m.cost_usd_micros(&usage(2907, 0, 2893, 11)), 61);
    }

    #[test]
    fn cache_prices_default_to_prompt_price() {
        let m = ModelEntry {
            prompt_price_micros_per_mtok: 1_000_000,
            completion_price_micros_per_mtok: 2_000_000,
            ..Default::default()
        };
        // 単価未設定ならキャッシュの内訳があっても #581 以前と同じ（入力総数 × 入力単価）。
        assert_eq!(
            m.cost_usd_micros(&usage(1000, 100, 600, 300)),
            m.cost_usd_micros(&usage(1000, 100, 0, 0))
        );
        assert_eq!(m.cost_usd_micros(&usage(1000, 100, 600, 300)), 1_200);
    }

    #[test]
    fn inconsistent_cache_breakdown_is_clamped_to_the_prompt_total() {
        let m = luna();
        assert_eq!(usage(10, 0, 8, 8).uncached_prompt_tokens(), 0);
        // 内訳が総数を超える報告: 読み出し → 書き込みの順に総数で頭打ちにする。
        assert_eq!(usage(10, 0, 8, 8).normalized(), usage(10, 0, 8, 2));
        assert_eq!(usage(10, 0, 1_000_000, 5).normalized(), usage(10, 0, 10, 0));
        // 整合した報告は変わらない。
        assert_eq!(usage(10, 0, 3, 4).normalized(), usage(10, 0, 3, 4));
        // 金額も総数の範囲で数える（1M トークンを読み出し単価で = 20_000）。
        assert_eq!(
            m.cost_usd_micros(&usage(1_000_000, 0, 5_000_000, 0)),
            20_000
        );
    }

    #[test]
    fn cost_rounds_once_over_the_sum() {
        let m = ModelEntry {
            prompt_price_micros_per_mtok: 500_000,
            completion_price_micros_per_mtok: 500_000,
            ..Default::default()
        };
        // 1 トークンずつ 0.5 micros。項ごとに切り捨てると 0 になるが、合計してから割ると 1。
        assert_eq!(m.cost_usd_micros(&usage(1, 1, 0, 0)), 1);
    }

    #[test]
    fn cost_saturates_instead_of_overflowing() {
        let m = ModelEntry {
            prompt_price_micros_per_mtok: u64::MAX,
            completion_price_micros_per_mtok: u64::MAX,
            ..Default::default()
        };
        assert_eq!(
            m.cost_usd_micros(&usage(u64::MAX, u64::MAX, 0, 0)),
            i64::MAX
        );
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
