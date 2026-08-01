//! 調査の委譲（`subagent`・#391）: 隔離コンテキストのサブエージェントを 1 体走らせる。
//!
//! 外部調査（Anthropic の orchestrator-worker・NVIDIA AI-Q・Claude Code の `/deep-research`）で
//! 効いているのは「**コンテキストの分離**」であって同時実行数ではない。単一ループの 1 つの履歴に
//! 全ての取得本文が積むと、剪定で古い証拠が畳まれ、視点が混ざる。子を**新しい履歴**で回し、
//! **合成済み findings だけ**を親へ返すことで、親のコンテキストは要約のみで済む。
//!
//! # 1 呼び出し＝1 サブエージェント
//!
//! ファンアウトの機構を新設しない。[`Tool::is_read_only`] を表明することで、モデルが同一ステップに
//! 複数 `subagent` を並べれば **#349 の有界並列がそのまま並列化**し、UI も既存の「並行して N 件」
//! 表示（#386）に乗る。
//!
//! # 境界（守る不変条件）
//!
//! - **権限昇格なし**: 子は親と同一の `AuthContext` で走る（`Tool::call` の `ctx` をそのまま渡す）。
//! - **read-only のみ**: 子のツールは呼び出し側が明示 allowlist で渡す。破壊系が無いので
//!   「入れ子の承認は誰が出すのか」という問題自体が消える（`approver: None`）。
//! - **入れ子の入れ子なし**: 子のツール列に `subagent` を含めない（構築時に渡さないだけで足りる）。
//! - **生の取得本文を返さない**: 親へ渡すのは子の最終本文（findings）のみ。子のイベントは
//!   内部の収集シンクが受け、親の `generation_event` へは流さない。
//! - **予算は親に積む**: 子の消費を [`ToolUsage`] で返し、ループが親の `Spent` へ `add_external`
//!   する。トークンが十数倍になり得る機構なので、これが唯一の安全弁。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use authz::AuthContext;
use llm_gateway::{LlmGateway, Message as LlmMessage, Role as LlmRole};

use crate::agent::{run_agent, RunContext};
use crate::approval::ApprovalPolicy;
use crate::event::{AgentError, AgentEvent};
use crate::profile::AgentOptions;
use crate::tool::{Tool, ToolError, ToolOutcome, ToolUsage};
use crate::vocab::ToolName;

/// サブエージェント 1 体の上限と全体の体数上限（#391）。
#[derive(Debug, Clone, Copy)]
pub struct SubagentLimits {
    /// 1 体のループ上限（親より小さくする）。
    pub max_steps: usize,
    /// 1 体の累積トークン上限。
    pub max_tokens: u64,
    /// 1 体の累積コスト上限（マイクロ USD）。
    pub max_cost_usd_micros: i64,
    /// **1 run で作れる体数の累計上限**（同時実行数は `parallel_read_tools` が決める）。
    pub max_per_run: usize,
    /// 子の中での冪等 read 並列度（親×子で同時取得が積算するため小さくする）。
    pub parallel_read_tools: usize,
    /// **1 応答**の出力上限（累積上限の `max_tokens` とは別概念）。
    ///
    /// 子の成果物は findings（数千字）そのもので、reasoning 系はその前に思考でトークンを使う。
    /// プロファイル既定（4096）だと**本文を書き出す前に枠が尽きて空で終わる**（実測: 検証ロールが
    /// 2 ステップで本文ゼロ・親には「Completed」とだけ届いた）。親が既に同じ理由で 8192 へ
    /// 上げているので、子も揃える。
    pub max_response_tokens: u32,
}

impl Default for SubagentLimits {
    /// **調査の本体を委譲で回す**ことを前提にした既定（#391・#404）。
    ///
    /// 親は計画・分割・統合だけを持ち、取得は子が行う。1 本の調査で 100 件規模の情報源に
    /// 当たるには、子 1 体が 8〜10 件を取り、それを 12〜16 体並べる必要がある。
    ///
    /// 上限は **`Spent::fresh_tokens`（新規ぶん）** で判定する（#404）。履歴の再送を
    /// 仕事量として数えないので、ここの値はそのまま「1 体が読み書きできる分量」を表す。
    /// 8 ステップで 8〜10 件の本文を取り込み、findings を書いて返すのに 12 万あれば足りる
    /// （課金累計はこの数倍になるが、それは `max_cost_usd_micros` が見る）。
    /// 体数は 16（親のコンテキストには findings しか戻らないので、増やしても親は太らない）。
    fn default() -> Self {
        SubagentLimits {
            max_steps: 8,
            max_tokens: 120_000,
            max_cost_usd_micros: 600_000,
            max_per_run: 16,
            parallel_read_tools: 4,
            max_response_tokens: 8192,
        }
    }
}

use super::subagent_input::{optional, required, Role};
use super::subagent_prompts::DEFAULT_SYSTEM;
use super::subagent_sink::CollectingSink;

/// 委譲ツール。1 run につき 1 インスタンス（体数カウンタを共有する）。
pub struct SubagentTool {
    gateway: LlmGateway,
    /// 子に渡すツール（**read-only の明示 allowlist**・`subagent` を含めないこと）。
    tools: Vec<Arc<dyn Tool>>,
    limits: SubagentLimits,
    /// 子の system プロンプト（既定は [`DEFAULT_SYSTEM`]）。
    system: String,
    /// 論理モデル名（親と同じものを渡す想定）。
    model: Option<String>,
    /// 冪等キーの接頭辞（親の `<run_id>:<fencing>`）。子は `:sub<n>` を足す。
    idempotency_prefix: String,
    /// 生成済みの体数（`max_per_run` の判定と冪等キーの採番を兼ねる）。
    spawned: AtomicUsize,
    /// 親と共有するキャンセルフラグ（run 停止で子も止める）。
    cancel: Arc<std::sync::atomic::AtomicBool>,
    /// **子のツール実行を UI へ中継する**送り口（#391 の続き）。
    ///
    /// 調査の本体を委譲すると、親の画面には `subagent` の行しか出ない。100 件の情報源に
    /// 当たっていても「委譲しました」が数行流れるだけで、何が起きているか見えない。
    /// ツール実行イベントだけを親の**イベント経路**（generation_event）へ中継する。
    ///
    /// **LLM コンテキストへは入れない**（隔離の不変条件はそのまま）。`Citation` を UI へ
    /// 伝播させているのと同じ扱いで、親が読むのは合成済み findings だけ。
    tool_events: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
}

impl SubagentTool {
    /// 委譲ツールを作る。`tools` は**子に渡す read-only ツール**（`subagent` を含めない）。
    #[must_use]
    pub fn new(
        gateway: LlmGateway,
        tools: Vec<Arc<dyn Tool>>,
        limits: SubagentLimits,
        idempotency_prefix: String,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        SubagentTool {
            gateway,
            tools,
            limits,
            system: DEFAULT_SYSTEM.to_string(),
            model: None,
            idempotency_prefix,
            spawned: AtomicUsize::new(0),
            cancel,
            tool_events: None,
        }
    }

    /// 子のツール実行イベントを中継する送り口を設定する（UI 表示専用）。
    #[must_use]
    pub fn with_tool_events(mut self, tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>) -> Self {
        self.tool_events = Some(tx);
        self
    }

    /// 論理モデル名を指定する（未指定は gateway 既定）。
    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// 子の実行オプション（ロールごとに system と予算を変える）。
    fn child_options(&self, role: Role) -> AgentOptions {
        child_options(&self.limits, role, &self.system, self.model.as_deref())
    }
}

/// 検証ロールのコンテキスト猶予。
///
/// 検証の入力は**レポートと証拠台帳そのもの**（合わせて 2 万字規模）で、既定の 24k では
/// 読み込んだ端から剪定で畳まれる ——「材料を保てない子に突き合わせをさせる」形になる。
/// 調査の子と違い、畳まれた分を web から取り直すこともできない。
///
/// ステップ数は**広げない**。上限に当たっていたのは着地指示を `VERIFY_SYSTEM` に
/// 入れ忘れていた間の話で、入れた後の run は 8 ステップ中 7 で自分から畳んで指摘を返した。
const VERIFY_CONTEXT_TOKENS: usize = 64_000;

/// 子の実行オプション（read-only の事前許可 ＋ `plan` 非提示 ＋ 小さめの予算）。
///
/// 自由関数にしてあるのは、隔離の条件（承認の狭さ・plan 非提示・並列度）を gateway 無しで
/// 単体検証できるようにするため。
fn child_options(
    limits: &SubagentLimits,
    role: Role,
    research_system: &str,
    model: Option<&str>,
) -> AgentOptions {
    let mut opts = AgentOptions::autonomous(
        limits.max_steps,
        None,
        limits.max_tokens,
        limits.max_cost_usd_micros,
    );
    opts.system = Some(role.system(research_system).to_string());
    opts.model = model.map(str::to_string);
    // 1 応答の出力上限。既定（4096）だと findings を書き出す前に枠が尽きる（`max_response_tokens`）。
    opts.max_tokens = Some(limits.max_response_tokens);
    opts.offer_plan_tool = false;
    opts.parallel_read_tools = limits.parallel_read_tools;
    // 自律版は egress（web_search / web_fetch）を承認ゲート対象にする。子に approver は
    // 居ないため、この 2 つだけを事前許可する。**それ以外に確認が要るツールが混ざっていたら
    // Reject される**（allowlist の設計ミスが黙って通らない・fail-closed）。
    opts.approval = ApprovalPolicy::auto([
        ToolName::WebSearch.as_str().to_string(),
        ToolName::WebFetch.as_str().to_string(),
    ]);
    match role {
        // 計画は 1 ターンで返る（ツールが無いのでループしない）。
        Role::Plan => opts.budget = crate::budget::Budget::autonomous(2, None, 20_000, 50_000),
        // 検証の入力は**レポートと証拠台帳そのもの**なので、剪定の猶予だけ広げる
        // （畳まれると突き合わせる材料が消え、同じファイルを読み直してステップを溶かす）。
        Role::Verify => opts.context_soft_limit_tokens = VERIFY_CONTEXT_TOKENS,
        Role::Research => {}
    }
    opts
}

#[async_trait::async_trait]
impl Tool for SubagentTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        ToolName::Subagent.as_str()
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "調査を 1 体のサブエージェントへ委譲する。サブエージェントは独立したコンテキストで\
         web/社内文書を調べ、**要約済みの findings（出典つき）だけ**を返す（生の本文は返らない）。\
         同一ステップで複数呼ぶと並列に走る。boundary（担当範囲）は必須で、複数体に委譲するときは\
         時期・地域・観点で重複なく割ること。単純な質問は委譲せず自分で調べる方が速い。\
         **執筆・編集は委譲できない**（書かせない）。書き上がったレポートの裏取りは \
         role=\"verify\" で独立した検証者に回せる（自分で自分の主張を検証すると確証バイアスが\
         そのまま残るため）。返るのは指摘のリストだけで、直すのは自分。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": "調べて答えるべきこと（1〜2 文で具体的に）"
                },
                "boundary": {
                    "type": "string",
                    "description": "担当範囲。時期・地域・観点で他の委譲と重複しないよう明示する\
                                    （role=research では必須。verify では節を割るときだけ指定）"
                },
                "output_format": {
                    "type": "string",
                    "description": "返してほしい形（例: 箇条書き 5 点・数値と出典の表）"
                },
                "sources_hint": {
                    "type": "string",
                    "description": "当たるべき情報源のヒント（例: 官公庁統計・IR 資料・社内規程）"
                },
                "role": {
                    "type": "string",
                    "enum": ["research", "plan", "verify"],
                    "description": "research=調べて findings を返す（既定）。plan=調べずに\
                                    「何を確かめるべきか」の計画 JSON だけを返す（計画フェーズ用）。\
                                    verify=書き上がったレポートと証拠台帳を突き合わせ、証拠に\
                                    紐づかない主張・誤引用の**指摘リストだけ**を返す（書き直さない）。\
                                    objective にレポートと台帳のパスを書くこと"
                }
            },
            "required": ["objective", "boundary"],
            "additionalProperties": false
        })
    }

    /// 子は read-only ツールしか持たず副作用が無いため、同一ステップ内で並列化してよい（#349）。
    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let objective = required(&input, "objective")?;
        let role = Role::parse(optional(&input, "role").as_deref());
        let boundary = if role.needs_boundary() {
            required(&input, "boundary")?
        } else {
            optional(&input, "boundary").unwrap_or_default()
        };

        // 体数上限。超過は**モデルが観測できる失敗**として返す（run は落とさない）。
        let index = self.spawned.fetch_add(1, Ordering::SeqCst);
        if index >= self.limits.max_per_run {
            return Ok(ToolOutcome::error(format!(
                "サブエージェントの上限（1 回の実行で {} 体）に達しました。以降は委譲せず\
                 自分で調べてください。",
                self.limits.max_per_run
            )));
        }

        // 依頼（objective）を**先頭**に置く。見出しを先に書くと、モデルによっては前置きの
        // 体裁を真似して本題が薄くなる（決定的テストでも先頭一致のトリガが効かない）。
        let mut prompt = match (role, boundary.as_str()) {
            (Role::Research, b) => format!("{objective}\n\n# 担当範囲（この外は調べない）\n{b}\n"),
            (_, "") => format!("{objective}\n"),
            (_, b) => format!("{objective}\n\n# 対象範囲\n{b}\n"),
        };
        for (heading, value) in [
            ("返す形", optional(&input, "output_format")),
            ("当たるべき情報源", optional(&input, "sources_hint")),
        ] {
            if let Some(value) = value {
                prompt.push_str("\n# ");
                prompt.push_str(heading);
                prompt.push('\n');
                prompt.push_str(&value);
                prompt.push('\n');
            }
        }

        let mut sink = CollectingSink::new(Arc::clone(&self.cancel), self.tool_events.clone());
        // 計画ロールにはツールを一切渡さない（調べさせない・#402）。手元の知識だけで
        // 「何を確かめるべきか」を出させる。調べてから計画すると、承認前に調査するのと変わらない。
        let child_tools: &[Arc<dyn Tool>] = if role.uses_tools() { &self.tools } else { &[] };
        let run = RunContext {
            ctx,
            // 親と衝突しない冪等キー（会計・Langfuse の相関に使われる）。
            idempotency_prefix: format!("{}:sub{index}", self.idempotency_prefix),
            trace_id: trace_id.map(str::to_string),
            input_preview: objective.clone(),
            app_id: None,
        };
        let outcome = run_agent(
            &self.gateway,
            child_tools,
            vec![LlmMessage::text(LlmRole::User, prompt)],
            &run,
            &self.child_options(role),
            None,
            // 破壊系を渡していないので承認者は不要（居ないこと自体が fail-closed 側）。
            None,
            &mut sink,
        )
        .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            // 失敗しても**そこまでの消費は親へ計上する**（無料で作り直せてしまうと予算が意味を失う）。
            Err(e) => {
                // 利用枠超過は「やり直せば通る」ものではない。実測（#404 の検証 run）では
                // 429 を食った委譲を 6 回リトライして残りの枠を焼き切り、レポートに到達せず
                // run ごと落ちた。**やり直させず、手元の材料で書かせる**。
                let msg = match &e {
                    AgentError::RateLimited(m) => format!(
                        "{m} 委譲はこれ以上やり直さないこと。ここまでに集めた材料で\
                         **レポートを書き切ること**。"
                    ),
                    other => format!(
                        "サブエージェントの実行に失敗しました（{other}）。委譲をやり直すか\
                         自分で調べ、**レポートは必ず書くこと**。"
                    ),
                };
                let mut out = ToolOutcome::error(msg);
                out.usage = Some(ToolUsage {
                    tokens: sink.spent.tokens,
                    fresh_tokens: sink.spent.fresh_tokens,
                    cost_usd_micros: sink.spent.cost_usd_micros,
                });
                return Ok(out);
            }
        };

        let spent = outcome.checkpoint.spent;
        let findings = sink.text.trim().to_string();
        let findings_chars = findings.chars().count();
        let mut out = if findings.is_empty() {
            // 停止理由ごとに**次の手**を言い分ける。「Completed なのに空」とだけ返すと、
            // やり直すべきか自分でやるべきかが判断できない（実測でモデルが迷った）。
            let why = match outcome.stop {
                crate::agent::AgentStop::Truncated => {
                    "本文を書き出す前に 1 応答の出力上限に達しました（思考が長すぎた）。\
                     範囲を半分に切って再依頼するか、自分で確かめてください。"
                }
                crate::agent::AgentStop::Budget(_) => {
                    "上限に達して途中で切られました。boundary をもっと狭く切って再依頼するか、\
                     自分で調べてください。"
                }
                _ => {
                    "何も返しませんでした。委譲をやり直すなら boundary をもっと狭く切ること。\
                     やり直さない場合は自分で調べてください。"
                }
            };
            ToolOutcome::error(format!(
                "サブエージェントは成果物を返しませんでした（停止理由: {:?}）。{why}\
                 どの経路でも**最後の成果物は必ず作ること**。",
                outcome.stop
            ))
        } else {
            ToolOutcome::ok(findings)
        };
        // 社内文書の引用は UI へ伝播させる（**イベント経路**であり親の LLM コンテキストには
        // 入らないので、隔離の不変条件は保たれる）。
        out.citations = sink.citations;
        out.subagent_runs = vec![serde_json::json!({
            "objective": objective,
            "boundary": boundary,
            "role": role.as_str(),
            "steps": spent.steps,
            "tool_calls": sink.tool_calls,
            "tokens": spent.tokens,
            // 上限判定に使う軸（新規ぶん）。`tokens`（課金累計）は履歴の再送を含むため、
            // 「上限が実際に効いたのか」はこちらでしか判断できない（#404）。
            "fresh_tokens": spent.fresh_tokens,
            // findings が空で終わった委譲を数えられるようにする（#407 の収束計測）。
            "findings_chars": findings_chars,
            // 停止理由（完了か・予算/ステップ上限か・キャンセルか）。予算で切れた委譲は
            // 「静かに何も返さない」形で現れるため、監査と UI に必ず残す（#404）。
            //
            // **収束の計測はこの分布で行う**（#407）。「調べ切って終わった（Completed）」と
            // 「上限で切られた（Budget）」の比が、上限値が妥当かどうかの唯一の根拠になる。
            "stop": format!("{:?}", outcome.stop),
        })];
        out.usage = Some(ToolUsage {
            tokens: spent.tokens,
            fresh_tokens: spent.fresh_tokens,
            cost_usd_micros: spent.cost_usd_micros,
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 隔離の条件（承認の狭さ・`plan` 非提示・並列度）を gateway 無しで固定する。
    #[test]
    fn child_options_are_isolated_and_read_only() {
        let limits = SubagentLimits::default();
        let opts = child_options(&limits, Role::Research, "sys", Some("m"));
        assert!(
            !opts.offer_plan_tool,
            "plan は提示しない（限られたステップを使わせない）"
        );
        assert!(opts.profile.is_autonomous(), "剪定とループ検出は効かせる");
        // egress の 2 つだけが事前許可（他に確認が要るツールが混ざれば Reject される）。
        assert!(opts.approval.is_pre_authorized("web_search"));
        assert!(opts.approval.is_pre_authorized("web_fetch"));
        for gated in ["fs_write", "fs_delete", "shell", "office.live_edit"] {
            assert!(
                !opts.approval.is_pre_authorized(gated),
                "{gated} を子が通せてはいけない"
            );
        }
        // 1 応答の出力上限は**プロファイル既定より広げる**。成果物（findings・指摘リスト）は
        // 子の最終応答そのもので、reasoning 系はその前に思考で枠を使う。既定のままだと
        // 本文を書き出す前に切れて空で終わる（#407 の実測）。
        assert!(
            opts.max_tokens.is_some_and(|m| m >= 8192),
            "子の 1 応答上限が狭いと成果物が空で返る"
        );
        // 同時取得は 親の並列度 × 子の並列度 で積算する。委譲を既定にした結果ここは
        // 「絞る」ではなく「積算の上限を意識して決める」値になった（親 6 × 子 4 = 24）。
        // 1 だと子の中が逐次になり、100 件規模の調査が終わらない。
        assert!(
            (2..=crate::profile::DEFAULT_PARALLEL_READ_TOOLS).contains(&opts.parallel_read_tools),
            "子の並列度は 2〜{} の範囲に収める（積算が効くため青天井にしない）",
            crate::profile::DEFAULT_PARALLEL_READ_TOOLS
        );
        assert_eq!(opts.model.as_deref(), Some("m"));
    }

    /// 検証は入力（レポート＋証拠台帳）を保てないと成立しないので、剪定の猶予だけ広げる。
    #[test]
    fn verify_keeps_its_input_from_being_pruned() {
        let limits = SubagentLimits::default();
        let research = child_options(&limits, Role::Research, "sys", None);
        let verify = child_options(&limits, Role::Verify, "sys", None);
        assert!(
            verify.context_soft_limit_tokens > research.context_soft_limit_tokens,
            "レポートと証拠台帳が剪定で畳まれると、突き合わせる材料そのものが消える"
        );
        // ステップは調査と同じ（着地指示が入っていれば上限前に畳める・#407 の実測）。
        assert_eq!(verify.budget.max_steps, research.budget.max_steps);
        // 計画だけは小さい（ツールが無く 1 ターンで返る）。
        let plan = child_options(&limits, Role::Plan, "sys", None);
        assert!(plan.budget.max_steps < research.budget.max_steps);
    }
}
