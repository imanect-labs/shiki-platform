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
use crate::event::{AgentError, AgentEvent, EventSink};
use crate::profile::AgentOptions;
use crate::tool::{Citation, Tool, ToolError, ToolOutcome, ToolUsage};
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
}

impl Default for SubagentLimits {
    /// 実 LLM ＋ 実 web で 1 体が**完走できる**ことを基準にした既定（#391）。
    ///
    /// `Spent.tokens` は各ステップの prompt＋completion の**累積**で、prompt には伸び続ける履歴が
    /// 毎回含まれる（≒二次で増える）。取得本文が入る調査エージェントでは、6 ステップでも
    /// 累積 10 万トークン級になる。当初 60k で切っていたところ、実検証で**3 体すべてが
    /// `Budget(Tokens)` で findings を返せず**、親がレポートを書かずに終わった。
    /// ステップを絞って（8→6）トークン枠を現実的な値へ上げ、体数上限は代わりに下げている。
    fn default() -> Self {
        SubagentLimits {
            max_steps: 6,
            max_tokens: 120_000,
            max_cost_usd_micros: 400_000,
            max_per_run: 8,
            parallel_read_tools: 2,
        }
    }
}

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
}

/// 計画ロールの system プロンプト（#402）。
///
/// **この子には手順書（skill の instructions）もワークスペースも渡さない。** 親が計画を書くと
/// 「brief を書く」「証拠台帳を作る」「節ごとに執筆する」といった**自分のこれからの作業**が
/// step に混入する（実 LLM で 3 回とも起きた）。知らない文脈で立てさせれば、原理的に入らない。
const PLAN_SYSTEM: &str = r#"あなたは調査の計画だけを立てる担当です。依頼だけを見て、
**その問いに答えるために何を確かめる必要があるか**を 5〜7 個に分けてください。

守ること:
- 1 項目 = 1 つの問い。「〜か」「〜はどこから来るか」「〜はどの条件で成り立つか」の形で書く。
- 各項目に「何を根拠に決着させるか」（見る指標・当たる情報源の種類・比較の軸）を 1〜2 文添える。
- 項目どうしが重複しないように割る（時期・地域・主体・観点のどれかで切る）。
- 賛否が割れる依頼なら、割れている理由そのもの（測り方・対象・条件の違い）を項目にする。
- **調べない**。いま手元にある知識だけで、確かめるべきことを列挙する。

出力は次の JSON だけ（前後に説明を書かない）:
{"title": "調査タイトル", "intro": "何にどう答えるか。対象外も書く", "steps": [{"title": "問い", "description": "何を根拠に決着させるか"}]}"#;

/// 子の既定 system プロンプト。**findings のみを返す**ことを強く縛る。
const DEFAULT_SYSTEM: &str = "あなたは調査専門のサブエージェントです。与えられた objective を\
 boundary の範囲内だけ調べ、**合成済みの findings** を返します。\n\
 \n\
 守ること:\n\
 - boundary の外は調べない（他のサブエージェントが担当している）。\n\
 - 取得した本文をそのまま貼らない。事実・数値・出典 URL・日付に要約する。\n\
 - 主張には必ず出典 URL（または社内文書名）を添える。裏が取れないものは「未確認」と書く。\n\
 - 見つからなかったことは「見つからなかった」と明示する（推測で埋めない）。\n\
 - 矛盾する情報があれば両方を、日付と出所つきで残す。\n\
 - 最後の応答が成果物です。前置き・謝辞・次の提案は書かない。";

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
        }
    }

    /// 論理モデル名を指定する（未指定は gateway 既定）。
    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// 子の実行オプション（`plan_role` は計画専用の system と小さな予算）。
    fn child_options(&self, plan_role: bool) -> AgentOptions {
        let system = if plan_role { PLAN_SYSTEM } else { &self.system };
        let mut opts = child_options(&self.limits, system, self.model.as_deref());
        if plan_role {
            // 計画は 1 ターンで返る（ツールが無いのでループしない）。
            opts.budget = crate::budget::Budget::autonomous(2, None, 20_000, 50_000);
        }
        opts
    }
}

/// 子の実行オプション（read-only の事前許可 ＋ `plan` 非提示 ＋ 小さめの予算）。
///
/// 自由関数にしてあるのは、隔離の条件（承認の狭さ・plan 非提示・並列度）を gateway 無しで
/// 単体検証できるようにするため。
fn child_options(limits: &SubagentLimits, system: &str, model: Option<&str>) -> AgentOptions {
    let mut opts = AgentOptions::autonomous(
        limits.max_steps,
        None,
        limits.max_tokens,
        limits.max_cost_usd_micros,
    );
    opts.system = Some(system.to_string());
    opts.model = model.map(str::to_string);
    opts.offer_plan_tool = false;
    opts.parallel_read_tools = limits.parallel_read_tools;
    // 自律版は egress（web_search / web_fetch）を承認ゲート対象にする。子に approver は
    // 居ないため、この 2 つだけを事前許可する。**それ以外に確認が要るツールが混ざっていたら
    // Reject される**（allowlist の設計ミスが黙って通らない・fail-closed）。
    opts.approval = ApprovalPolicy::auto([
        ToolName::WebSearch.as_str().to_string(),
        ToolName::WebFetch.as_str().to_string(),
    ]);
    opts
}

/// 入力の必須文字列を取り出す（空白のみは欠落として扱う）。
fn required(input: &serde_json::Value, key: &str) -> Result<String, ToolError> {
    let raw = input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    if raw.is_empty() {
        return Err(ToolError::Invalid(format!("missing '{key}'")));
    }
    Ok(raw.to_string())
}

/// 任意の文字列（空はなし扱い）。
fn optional(input: &serde_json::Value, key: &str) -> Option<String> {
    let raw = input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    (!raw.is_empty()).then(|| raw.to_string())
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
         執筆・編集は委譲できない（調査専用）。"
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
                    "description": "担当範囲。時期・地域・観点で他の委譲と重複しないよう明示する"
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
                    "enum": ["research", "plan"],
                    "description": "research=調べて findings を返す（既定）。plan=調べずに\
                                    「何を確かめるべきか」の計画 JSON だけを返す（計画フェーズ用）"
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
        // 計画ロールは**依頼だけ**を見る（担当範囲という概念が無い）。
        let plan_role = optional(&input, "role").as_deref() == Some("plan");
        let boundary = if plan_role {
            String::new()
        } else {
            required(&input, "boundary")?
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
        let mut prompt = if plan_role {
            format!("{objective}\n")
        } else {
            format!("{objective}\n\n# 担当範囲（この外は調べない）\n{boundary}\n")
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

        let mut sink = CollectingSink::new(Arc::clone(&self.cancel));
        // 計画ロールにはツールを一切渡さない（調べさせない・#402）。手元の知識だけで
        // 「何を確かめるべきか」を出させる。調べてから計画すると、承認前に調査するのと変わらない。
        let child_tools: &[Arc<dyn Tool>] = if plan_role { &[] } else { &self.tools };
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
            &self.child_options(plan_role),
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
                let mut out = ToolOutcome::error(format!(
                    "サブエージェントの実行に失敗しました（{e}）。委譲をやり直すか自分で調べ、\
                     **レポートは必ず書くこと**。"
                ));
                out.usage = Some(ToolUsage {
                    tokens: sink.spent.tokens,
                    cost_usd_micros: sink.spent.cost_usd_micros,
                });
                return Ok(out);
            }
        };

        let spent = outcome.checkpoint.spent;
        let findings = sink.text.trim().to_string();
        let mut out = if findings.is_empty() {
            ToolOutcome::error(format!(
                "サブエージェントは findings を返しませんでした（停止理由: {:?}）。\
                 委譲をやり直すなら boundary をもっと狭く切ること。やり直さない場合は\
                 自分で web_search / web_fetch を使って調べ、**レポートは必ず書くこと**。",
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
            "role": if plan_role { "plan" } else { "research" },
            "steps": spent.steps,
            "tool_calls": sink.tool_calls,
            "tokens": spent.tokens,
        })];
        out.usage = Some(ToolUsage {
            tokens: spent.tokens,
            cost_usd_micros: spent.cost_usd_micros,
        });
        Ok(out)
    }
}

/// 子のイベントを**親へ流さず**集める内部シンク（#391）。
///
/// 親の `generation_event` に子の生イベントを混ぜると SSE と projection が壊れる。ここで
/// ①最終本文（findings）②`Citation`③ツール名の列（監査用）だけを取り、他は捨てる。
struct CollectingSink {
    text: String,
    citations: Vec<Citation>,
    tool_calls: Vec<String>,
    /// ステップ境界で観測した消費（`save_checkpoint` 経由）。
    ///
    /// `run_agent` が `Err`（LLM 障害等）で抜けると `AgentOutcome` に到達せず、**途中まで
    /// 消費したトークンが親の予算に計上されない**（＝予算の抜け穴・レビュー指摘 Critical）。
    /// ループはステップを完了するたびにチェックポイントを渡してくるので、その `spent` を控えて
    /// エラー経路でも計上する。取りこぼすのは失敗したステップ自身の分だけ。
    spent: crate::budget::Spent,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl CollectingSink {
    fn new(cancel: Arc<std::sync::atomic::AtomicBool>) -> Self {
        CollectingSink {
            text: String::new(),
            citations: Vec::new(),
            tool_calls: Vec::new(),
            spent: crate::budget::Spent::default(),
            cancel,
        }
    }
}

#[async_trait::async_trait]
impl EventSink for CollectingSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        match event {
            AgentEvent::Text(t) => self.text.push_str(&t),
            AgentEvent::Citation(c) => self.citations.push(c),
            AgentEvent::ToolCall { name, .. } => {
                self.tool_calls.push(name);
                // ツール呼び出しの前に出た本文は「これから調べます」の前置き。findings は
                // **ツールを呼ばずに終わった最後のステップ**の本文なので、ここで捨てる。
                self.text.clear();
            }
            // Thinking / ToolResult / 予算警告などは親へ出さない（生の観測を漏らさない）。
            _ => {}
        }
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// ステップ境界の消費を控える（エラー経路でも親へ計上するため）。永続化はしない。
    async fn save_checkpoint(
        &mut self,
        checkpoint: &crate::checkpoint::Checkpoint,
    ) -> Result<(), AgentError> {
        self.spent = checkpoint.spent;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 隔離の条件（承認の狭さ・`plan` 非提示・並列度）を gateway 無しで固定する。
    #[test]
    fn child_options_are_isolated_and_read_only() {
        let limits = SubagentLimits::default();
        let opts = child_options(&limits, "sys", Some("m"));
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
        assert!(
            opts.parallel_read_tools < crate::profile::DEFAULT_PARALLEL_READ_TOOLS,
            "親×子で同時取得が積算しないよう絞る"
        );
        assert_eq!(opts.model.as_deref(), Some("m"));
    }

    #[test]
    fn required_and_optional_treat_blank_as_missing() {
        let input = serde_json::json!({ "objective": " 調べる ", "boundary": "  ", "hint": "" });
        assert_eq!(required(&input, "objective").unwrap(), "調べる");
        assert!(matches!(
            required(&input, "boundary"),
            Err(ToolError::Invalid(_))
        ));
        assert!(optional(&input, "hint").is_none());
        assert!(optional(&input, "missing").is_none());
    }
}
