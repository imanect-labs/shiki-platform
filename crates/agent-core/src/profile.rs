//! エージェントプロファイル（Task 5.1）と実行オプション。
//!
//! **同一コア（[`run_agent`](crate::agent::run_agent)）を設定だけで切り替える**: チャット制約版は
//! 短ホライズン・安全ツール、自律版は長ホライズン・フルツール・予算・計画・剪定。コアロジックは
//! 二重化しない（phase-5.md §5.1）。

use std::time::Instant;

use llm_gateway::Effort;

use crate::approval::ApprovalPolicy;
use crate::budget::Budget;
use crate::checkpoint::Checkpoint;
use crate::plan::Plan;

/// 自律度・ホライズン・環境のプロファイル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProfile {
    /// チャット制約版（短ホライズン・安全ツール・現行 Phase 3/4 挙動）。
    /// 計画メタツール・コンテキスト剪定・ループ検出は無効（Phase 3 と同一の振る舞い）。
    Chat,
    /// 自律版（長ホライズン・フルツール・予算/計画/剪定/ループ検出を有効化）。
    Autonomous,
}

impl AgentProfile {
    /// 自律機能（計画・剪定・ループ検出・予算警告）を使うプロファイルか。
    #[must_use]
    pub fn is_autonomous(self) -> bool {
        matches!(self, AgentProfile::Autonomous)
    }
}

/// ループのオプション。予算（step/time/token/cost）は [`Budget`] に集約する。
pub struct AgentOptions {
    /// 自律度プロファイル。
    pub profile: AgentProfile,
    /// トップレベル system プロンプト。
    pub system: Option<String>,
    /// 論理モデル名（未指定は gateway 既定）。
    pub model: Option<String>,
    /// 思考強度。
    pub effort: Option<Effort>,
    /// 1 応答の max_tokens（**1 生成あたり**・累積上限は [`Budget`]）。
    pub max_tokens: Option<u32>,
    /// サンプリング温度（skill のモデル既定・Task 6.9。None は provider 既定）。
    pub temperature: Option<f32>,
    /// 承認ポリシ（どの破壊系ツールを事前許可するか・Task 3.9/5.6）。
    pub approval: ApprovalPolicy,
    /// 予算ガード（安全停止の要）。
    pub budget: Budget,
    /// コンテキスト剪定のソフト上限トークン（自律版のみ・0 で無効）。
    pub context_soft_limit_tokens: usize,
    /// 剪定時に無傷で残す直近メッセージ数（自律版のみ）。
    pub context_keep_recent: usize,
    /// `plan` メタツールを提示するか（自律版の既定 true・#391）。
    ///
    /// サブエージェントは 1 つの `objective` を調べて findings を返すだけなので計画は要らない。
    /// 提示すると限られたステップ予算を計画更新に使ってしまうため、委譲側で false にする。
    pub offer_plan_tool: bool,
    /// 同一ステップ内で並列実行する冪等 read ツールの上限（issue #349・0/1 は逐次と等価）。
    ///
    /// deep research の `web_search`→複数 `web_fetch` ファンアウトを直列にしないための有界並列度。
    /// 同一ホストへの `web_fetch` は本値によらず互いに直列化される（礼儀）。
    pub parallel_read_tools: usize,
    /// tool search（遅延ツールを検索して読み込む）の設定。
    pub tool_search: ToolSearchOptions,
}

/// tool search の設定（[`crate::tool_search`]）。
///
/// 遅延にできるツール（[`crate::ToolName::loading`] が `Deferred`）の定義が十分大きいときだけ
/// 有効になる。小さいなら全部載せる方が安い（検索の 1 手と、その往復のトークンを払わない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSearchOptions {
    /// 有効にするか（false なら常に全ツールを載せる）。
    pub enabled: bool,
    /// 遅延にできる定義の推定トークン合計がこれ未満なら使わない。
    pub min_deferred_tokens: usize,
    /// 語彙の既定によらず**常に載せる**ツール名（skill の `allowed_tools` 等・run 単位の上書き）。
    pub always_load: Vec<String>,
}

/// tool search を有効にする遅延定義の既定下限（推定トークン）。
///
/// Anthropic の目安（定義が 10k トークン超・ツール 10 個以上で効く）より低めに置く:
/// 選択精度の劣化はトークン量より**候補数**で効くため、中規模でも絞る価値がある。
pub const DEFAULT_TOOL_SEARCH_MIN_DEFERRED_TOKENS: usize = 2_000;

impl Default for ToolSearchOptions {
    fn default() -> Self {
        ToolSearchOptions {
            enabled: true,
            min_deferred_tokens: DEFAULT_TOOL_SEARCH_MIN_DEFERRED_TOKENS,
            always_load: Vec::new(),
        }
    }
}

/// 冪等 read ツールの既定並列度（#349）。検索 API/取得先への負荷と体感速度の折衷。
pub const DEFAULT_PARALLEL_READ_TOOLS: usize = 4;

impl AgentOptions {
    /// チャット制約版の既定（現行挙動と互換・max_steps=8・token/cost 無制限・剪定/計画/ループ検出なし）。
    #[must_use]
    pub fn chat(max_steps: usize) -> Self {
        AgentOptions {
            profile: AgentProfile::Chat,
            system: None,
            model: None,
            effort: None,
            max_tokens: Some(2048),
            temperature: None,
            approval: ApprovalPolicy::deny_all(),
            budget: Budget::chat(max_steps),
            context_soft_limit_tokens: 0,
            context_keep_recent: 0,
            offer_plan_tool: true,
            parallel_read_tools: DEFAULT_PARALLEL_READ_TOOLS,
            tool_search: ToolSearchOptions::default(),
        }
    }

    /// 自律版の既定（長ホライズン・予算あり・剪定/計画/ループ検出を有効化）。
    #[must_use]
    pub fn autonomous(
        max_steps: usize,
        deadline: Option<Instant>,
        max_total_tokens: u64,
        max_cost_usd_micros: i64,
    ) -> Self {
        AgentOptions {
            profile: AgentProfile::Autonomous,
            system: None,
            model: None,
            effort: None,
            // 1 生成あたりの出力上限（`Budget.max_tokens`＝セッション累積上限とは別概念）。
            max_tokens: Some(4096),
            temperature: None,
            approval: ApprovalPolicy::deny_all(),
            budget: Budget::autonomous(max_steps, deadline, max_total_tokens, max_cost_usd_micros),
            // 既定: 約 24k トークンで古いツール出力を畳み、直近 6 メッセージは残す。
            context_soft_limit_tokens: 24_000,
            context_keep_recent: 6,
            offer_plan_tool: true,
            parallel_read_tools: DEFAULT_PARALLEL_READ_TOOLS,
            tool_search: ToolSearchOptions::default(),
        }
    }
}

impl Default for AgentOptions {
    fn default() -> Self {
        AgentOptions::chat(8)
    }
}

/// ループの終了結果。停止理由＋再開用チェックポイント（ステップ境界の状態）。
pub struct AgentOutcome {
    pub stop: crate::agent::AgentStop,
    /// 停止時点の状態（計画・消費・履歴・ステップ）。W3/W4 で durable run に永続化して再開に使う。
    pub checkpoint: Checkpoint,
}

impl AgentOutcome {
    /// 最終計画への参照（可視化・監査の補助）。
    #[must_use]
    pub fn plan(&self) -> &Plan {
        &self.checkpoint.plan
    }
}
