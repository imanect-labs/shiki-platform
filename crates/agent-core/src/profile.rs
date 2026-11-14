//! エージェントプロファイル（Task 5.1）と実行オプション。
//!
//! **同一コア（[`run_agent`](crate::agent::run_agent)）を設定だけで切り替える**: チャット制約版は
//! 短ホライズン・安全ツール、自律版は長ホライズン・フルツール・予算・計画・剪定。コアロジックは
//! 二重化しない（phase-5.md §5.1）。

use std::borrow::Cow;
use std::time::Instant;

use llm_gateway::Effort;

use crate::approval::ApprovalPolicy;
use crate::budget::{Budget, BudgetExtension};
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
#[derive(Clone)]
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
    /// 会話（スレッド）ごとに安定した ID。プロバイダのルーティング・キャッシュの手がかりとして
    /// llm-gateway へそのまま渡す（`GenerateRequest::session_id`）。
    pub session_id: Option<String>,
    /// tool search（遅延ツールを検索して読み込む）の設定。
    pub tool_search: ToolSearchOptions,
    /// 会計の利用元の上書き（`None` はプロファイルとミニアプリの有無から決める）。ワークフローの
    /// agent.invoke は自律版のループでも `Workflow` として数える（BUG-6）。
    pub usage_source: Option<llm_gateway::UsageSource>,
    /// 作成系の道具を初めて呼んだときに 1 回だけ広げる予算（D-53 の HD-4 (c)・D-204）。
    ///
    /// `None`（[`Self::chat`]・[`Self::autonomous`] の既定）は広げない。チャットのワーカーが通常の会話にだけ
    /// 入れる（サブエージェント・ワークフローの agent.invoke・ミニアプリの AI には効かせない）。広げたかどうかは
    /// 保存せず、毎ステップ履歴から導く（[`BudgetExtension`]）。
    pub budget_extension: Option<BudgetExtension>,
}

/// このステップに当てる予算と 1 応答の出力上限（作成系の道具を呼んでいれば広げた値・D-204）。
pub(crate) struct StepBudget<'a> {
    /// 判定に使う予算。
    pub(crate) budget: Cow<'a, Budget>,
    /// 生成に渡す 1 応答の出力上限。
    pub(crate) max_tokens: Option<u32>,
    /// 広げた値か。
    pub(crate) extended: bool,
}

/// 広げた 1 応答の出力上限をモデルの窓に収めるための材料（D-204）。
///
/// 広げた上限（既定 32768）をそのまま送ると、窓の小さいモデル（例: 窓 32k の vLLM）では
/// 「prompt＋max_tokens＞窓」で 400 になり、作成の直後の生成が失敗する。広げる前の上限で動いていた
/// 構成を悪くしないよう、元の上限は下回らない。
#[derive(Debug, Clone, Copy)]
pub(crate) struct WindowFit {
    /// モデルの窓（カタログの `context_window_tokens`・知らなければ `None`＝抑えない）。
    pub(crate) window_tokens: Option<u32>,
    /// 履歴の見積りに足す固定分（system・ツール定義・トークナイザの差・[`crate::context::ContextGauge`]）。
    pub(crate) overhead_tokens: usize,
}

/// 窓から prompt の見積りを引いた後に、さらに空けておく余白（窓の千分率）。
/// 見積りは概算（[`crate::context::estimate_prompt_tokens`]）なので、外れても溢れない側に倒す。
const WINDOW_MARGIN_PERMILLE: u64 = 50;

/// 広げた 1 応答の上限 `extended` を、窓 `window` から prompt の見積りと余白を引いた残りに収める。
/// 元の上限 `base` は下回らない（元の値で動いていた構成を悪くしない）。`base` が無く残りも無ければ
/// 広げる前と同じ `None`（プロバイダ既定）に戻す。
fn fit_response_tokens(
    extended: u32,
    base: Option<u32>,
    window: u32,
    prompt_tokens: u64,
) -> Option<u32> {
    let margin = u64::from(window) * WINDOW_MARGIN_PERMILLE / 1000;
    let room = u64::from(window)
        .saturating_sub(prompt_tokens)
        .saturating_sub(margin);
    let fitted = u32::try_from(room).unwrap_or(u32::MAX).min(extended);
    match base {
        Some(b) => Some(fitted.max(b)),
        None => (fitted > 0).then_some(fitted),
    }
}

/// tool search の設定（[`crate::tool_search`]）。
///
/// 遅延にできるツール（[`crate::ToolName::loading`] が `Deferred`）の定義が十分大きいときだけ
/// 有効になる。小さいなら全部載せる方が安い（検索の 1 手と、その往復のトークンを払わない）。
#[derive(Clone)]
pub struct ToolSearchOptions {
    /// 有効にするか（false なら常に全ツールを載せる）。
    pub enabled: bool,
    /// 遅延にできる定義の推定トークン合計がこれ未満なら使わない。
    pub min_deferred_tokens: usize,
    /// 語彙の既定によらず**常に載せる**ツール名（skill の `allowed_tools` 等・run 単位の上書き）。
    pub always_load: Vec<String>,
    /// 埋め込み（配線されていれば自然文の検索を BM25 と RRF で融合する・#517）。
    /// RAG と同じ `EmbeddingProvider`（ingestion-worker）。`None` なら BM25 のみ。
    pub embedder: Option<std::sync::Arc<dyn rag::EmbeddingProvider>>,
}

impl std::fmt::Debug for ToolSearchOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSearchOptions")
            .field("enabled", &self.enabled)
            .field("min_deferred_tokens", &self.min_deferred_tokens)
            .field("always_load", &self.always_load)
            .field(
                "embedder",
                &self
                    .embedder
                    .as_ref()
                    .map(|e| e.model_version().to_string()),
            )
            .finish()
    }
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
            embedder: None,
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
            session_id: None,
            tool_search: ToolSearchOptions::default(),
            usage_source: None,
            budget_extension: None,
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
            session_id: None,
            tool_search: ToolSearchOptions::default(),
            usage_source: None,
            budget_extension: None,
        }
    }

    /// 履歴 `messages` から、このステップに当てる予算と 1 応答の出力上限を導く（D-204）。
    ///
    /// 作成系の道具の呼び出しが履歴にあれば広げた値、無ければ設定どおりの値。広げたことは保存しないので、
    /// チェックポイントから再開しても同じ履歴から同じ値になる。
    pub(crate) fn step_budget(&self, messages: &[llm_gateway::Message]) -> StepBudget<'_> {
        match self
            .budget_extension
            .as_ref()
            .filter(|ext| ext.called(messages))
        {
            Some(ext) => StepBudget {
                budget: Cow::Owned(ext.extend(&self.budget)),
                max_tokens: ext.extend_response_tokens(self.max_tokens),
                extended: true,
            },
            None => StepBudget {
                budget: Cow::Borrowed(&self.budget),
                max_tokens: self.max_tokens,
                extended: false,
            },
        }
    }

    /// [`Self::step_budget`] に加えて、広げた 1 応答の上限を窓に収める（[`WindowFit`]・D-204）。
    ///
    /// 次に送る prompt は、直前の実測（`last_prompt_tokens`）と、今の履歴の見積り＋固定分の大きい方で見積もる
    /// （履歴は直前の実測の後にも伸びているので、実測だけでは小さく出る）。広げていないステップは何もしない。
    pub(crate) fn step_budget_within(
        &self,
        messages: &[llm_gateway::Message],
        fit: WindowFit,
        last_prompt_tokens: u64,
    ) -> StepBudget<'_> {
        let mut step = self.step_budget(messages);
        if let (true, Some(extended), Some(window)) =
            (step.extended, step.max_tokens, fit.window_tokens)
        {
            let estimate = crate::context::estimate_prompt_tokens(messages)
                .saturating_add(fit.overhead_tokens);
            let prompt = last_prompt_tokens.max(u64::try_from(estimate).unwrap_or(u64::MAX));
            step.max_tokens = fit_response_tokens(extended, self.max_tokens, window, prompt);
        }
        step
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::ToolName;
    use llm_gateway::{Block, Message, Role};

    fn extension() -> BudgetExtension {
        BudgetExtension {
            on_tools: vec![ToolName::EmitWorkflow],
            max_steps: 24,
            max_fresh_tokens: 600_000,
            max_cost_usd_micros: 2_000_000,
            max_response_tokens: 32_768,
        }
    }

    fn called_emit() -> Vec<Message> {
        vec![
            Message::text(Role::User, "作って"),
            Message {
                role: Role::Assistant,
                content: vec![Block::ToolUse {
                    id: "c1".into(),
                    name: "emit_workflow".into(),
                    input: serde_json::json!({}),
                }],
            },
        ]
    }

    /// 既定（チャット・自律）は広げない。サブエージェント・ワークフローの agent.invoke・ミニアプリの AI は
    /// これらの既定から組むので、作成系の道具を呼んでも予算は変わらない（D-204）。
    #[test]
    fn profiles_do_not_extend_by_default() {
        for opts in [
            AgentOptions::chat(6),
            AgentOptions::autonomous(50, None, 1_000, 1_000),
            AgentOptions::default(),
        ] {
            assert!(opts.budget_extension.is_none());
            let step = opts.step_budget(&called_emit());
            assert!(!step.extended);
            assert_eq!(step.budget.max_steps, opts.budget.max_steps);
            assert_eq!(step.max_tokens, opts.max_tokens);
        }
    }

    /// 呼び出しが履歴にあれば広げた値、無ければ設定どおりの値（毎ステップ履歴から導く）。
    #[test]
    fn step_budget_follows_the_history() {
        let mut opts = AgentOptions::chat(6);
        opts.max_tokens = Some(8192);
        opts.budget.max_tokens = Some(300_000);
        opts.budget.max_cost_usd_micros = Some(1_000_000);
        opts.budget_extension = Some(extension());
        let before = opts.step_budget(&called_emit()[..1]);
        assert!(!before.extended);
        assert!(matches!(before.budget, Cow::Borrowed(_)));
        assert_eq!(
            (before.budget.max_steps, before.max_tokens),
            (6, Some(8192))
        );
        let after = opts.step_budget(&called_emit());
        assert!(after.extended);
        assert_eq!(after.budget.max_steps, 24);
        assert_eq!(after.budget.max_tokens, Some(600_000));
        assert_eq!(after.budget.max_cost_usd_micros, Some(2_000_000));
        assert_eq!(after.max_tokens, Some(32_768));
        // 設定そのものは書き換えない（保存しない）。
        assert_eq!(opts.budget.max_steps, 6);
        assert_eq!(opts.max_tokens, Some(8192));
    }

    /// 広げた上限は窓から prompt と余白を引いた残りに収め、元の上限は下回らない（D-204）。
    #[test]
    fn extended_response_tokens_fit_the_window() {
        // 窓 32768・prompt 10000 → 余白 1638 を引いて 21130。
        assert_eq!(
            fit_response_tokens(32_768, Some(8192), 32_768, 10_000),
            Some(21_130)
        );
        // 窓に余裕があれば広げた値のまま。
        assert_eq!(
            fit_response_tokens(32_768, Some(8192), 200_000, 10_000),
            Some(32_768)
        );
        // 残りが元の上限より小さくても、元の上限は下回らない（広げる前と同じ要求になる）。
        assert_eq!(
            fit_response_tokens(32_768, Some(8192), 32_768, 30_000),
            Some(8192)
        );
        // 元の上限が無く残りも無ければ、広げる前と同じプロバイダ既定に戻す。
        assert_eq!(fit_response_tokens(32_768, None, 32_768, 40_000), None);
        assert_eq!(
            fit_response_tokens(32_768, None, 32_768, 10_000),
            Some(21_130)
        );
    }

    /// 窓を知っていれば、広げたステップだけ上限を抑える。広げていないステップ・窓を知らないモデルは変えない。
    #[test]
    fn step_budget_within_caps_only_extended_steps_with_a_known_window() {
        let mut opts = AgentOptions::chat(6);
        opts.max_tokens = Some(8192);
        opts.budget_extension = Some(extension());
        let fit = WindowFit {
            window_tokens: Some(32_768),
            overhead_tokens: 0,
        };
        // 直前の実測が見積りより大きければ実測を使う。
        let step = opts.step_budget_within(&called_emit(), fit, 10_000);
        assert!(step.extended);
        assert_eq!(step.max_tokens, Some(21_130));
        // 広げていないステップは元の値のまま。
        let before = opts.step_budget_within(&called_emit()[..1], fit, 30_000);
        assert_eq!(before.max_tokens, Some(8192));
        // 窓を知らなければ広げた値のまま。
        let unknown = WindowFit {
            window_tokens: None,
            overhead_tokens: 0,
        };
        let step = opts.step_budget_within(&called_emit(), unknown, 10_000);
        assert_eq!(step.max_tokens, Some(32_768));
        // 固定分（system・ツール定義）も見積りに入れる。
        let heavy = WindowFit {
            window_tokens: Some(32_768),
            overhead_tokens: 20_000,
        };
        let step = opts.step_budget_within(&called_emit(), heavy, 0);
        let max = step.max_tokens.unwrap();
        assert!(max < 32_768 - 20_000, "{max}");
    }
}
