//! agent-core のエージェントループ（Task 3.3/3.9 制約版 → Task 5.1〜5.7 自律拡張）。
//!
//! LLM↔ツールのループ（計画→ツール呼出→観測→継続→終了）。ツールセット非依存で [`Tool`] を差す。
//! [`AgentProfile`] で挙動を切り替える: **Chat**＝短ホライズン・安全ツール（Phase 3/4 と同一）、
//! **Autonomous**＝長ホライズン・フルツール＋予算ガード（5.7）・計画分解（5.2）・コンテキスト剪定（5.3）・
//! 失敗ループ検出（5.5）。ツール呼出/結果/引用/計画/予算は [`EventSink`] で逐次外部化する。
//!
//! ツール自動選択ポリシ（Task 3.9）: 利用可能ツールを全提示し、モデルが自動選択する。
//! `requires_confirmation()` なツール（破壊/権限/高コスト系）は事前許可が無ければ実行しない。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use authz::AuthContext;
use futures::stream::StreamExt;
use llm_gateway::{
    Block, GenerateRequest, GenerationRecord, LlmGateway, Message as LlmMessage, Role as LlmRole,
    StopReason, StreamDelta, ToolDef, Usage,
};

use crate::agent_observation::{
    append_budget_observation, demand_final_answer, nudge_empty_response,
};
use crate::approval::Approver;
use crate::budget::BudgetCheck;
use crate::checkpoint::Checkpoint;
use crate::event::{AgentError, AgentEvent, EventSink, RecoveryAction};
use crate::loop_detect::LoopDetector;
use crate::plan;
use crate::profile::{AgentOptions, AgentOutcome};
use crate::tool::Tool;

/// 計画メタツールの名前（自律版のみ提示・ループが横取りしてツールへは dispatch しない）。
pub(crate) const PLAN_TOOL: &str = crate::vocab::MetaToolName::Plan.as_str();

/// ループの停止理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStop {
    /// モデルが自然終了した。
    Completed,
    /// **1 応答の出力上限**（`max_tokens`）で切れた。完了ではない。
    ///
    /// `Completed` に潰すと、空の成果物が「正常終了」として報告される。実測（#407 の検証委譲）
    /// では、検証ロールの子が思考で 1 応答の枠を使い切って本文ゼロで終わり、親には
    /// 「停止理由: Completed なのに findings が無い」と届いた（親のログに「which is odd」と
    /// 残っている）。原因が言えないと、やり直すべきか自分でやるべきかも判断できない。
    Truncated,
    /// 最大ステップ/時間/トークン/コストの予算に達した（安全停止・Task 5.7）。
    Budget(crate::budget::BudgetKind),
    /// 同一失敗のループを検出して安全停止した（Task 5.5）。
    LoopDetected,
    /// キャンセル要求で停止した。
    Cancelled,
}

/// 1 run の会計/計装コンテキスト。
pub struct RunContext<'a> {
    pub ctx: &'a AuthContext,
    /// 冪等キーの接頭辞（`<run_id>:<attempt>`）。ステップごとに `:<step>` を足す。
    pub idempotency_prefix: String,
    pub trace_id: Option<String>,
    /// Langfuse 入力プレビュー（発話）。
    pub input_preview: String,
    /// 呼び出し元ミニアプリ（ゲートウェイ agent.invoke・Task 9.9）。chat 等は `None`。
    pub app_id: Option<uuid::Uuid>,
    /// **この run が trace の起点か。**
    ///
    /// サブエージェントは親と同じ `trace_id` を受け継ぐ（tools/subagent.rs）。
    /// step は子でも 0 から始まるので、これが無いと子の目的文が
    /// Langfuse の trace input を上書きし、利用者の最初の質問が消える。
    pub owns_trace: bool,
}

/// 累積中のツール呼び出し（1 ステップ分）。
pub(crate) struct PendingCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) input: serde_json::Value,
}

/// エージェントループを回す。ツールは全提示し、モデルが自動選択する。
///
/// `resume` を渡すと当該チェックポイント（計画・消費・履歴）から再開する（ステップ境界復元・5.5）。
/// 新規開始なら `None`（`messages` を起点に開始）。戻り値は停止理由＋再開用チェックポイント。
#[allow(clippy::too_many_arguments)] // gateway/tools/履歴/run/opts/resume/approver/sink の 8 点は本質的。
pub async fn run_agent(
    gateway: &LlmGateway,
    tools: &[Arc<dyn Tool>],
    messages: Vec<LlmMessage>,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    resume: Option<Checkpoint>,
    approver: Option<&dyn Approver>,
    sink: &mut dyn EventSink,
) -> Result<AgentOutcome, AgentError> {
    let tool_map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();
    // 出番の限られるツールは定義を遅延にし、`tool_search` で読み込ませる（候補は提示済みの
    // ツールだけ＝検索で認可は広がらない・tool_search.rs）。
    let (tool_defs, tool_search) =
        crate::tool_search::prepare(build_tool_defs(tools, opts), &opts.tool_search);
    // 文書の埋め込みを先に温めておく（モデルが最初に検索するまでに間に合えば融合で返せる）。
    if let Some(search) = &tool_search {
        search.prewarm(run.ctx);
    }

    // 再開 or 新規開始の状態。ループ検出器はチェックポイントから復元する（resume で失敗履歴を失わない）。
    let mut state = resume.unwrap_or_else(|| Checkpoint::start(messages));
    let mut detector = state.loop_detector.clone();
    let mut warned: std::collections::HashSet<crate::budget::BudgetKind> =
        std::collections::HashSet::new();
    // 剪定閾値はモデルの窓を天井にする（#529）。見積りは各ステップの実測 prompt で補正する。
    let limits = crate::context::ContextLimits::resolve(
        opts.context_soft_limit_tokens,
        opts.context_keep_recent,
        gateway.context_window_tokens(opts.model.as_deref()),
        opts.max_tokens,
    );
    // 実測の補正がまだ無い（新規開始・旧チェックポイント）なら、固定分（system＋ツール定義）を
    // 概算で入れておく。最初の 1 回で溢れて拒否されると、補正に必要な実測すら得られない。
    // 実測済みなら（0 でも）その値を使う。
    // 遅延ロードで未読込のツール定義は文脈に載らないので数えない。
    let context_tool_tokens = |messages: &[llm_gateway::Message]| {
        crate::context::estimate_static_tokens(
            None,
            llm_gateway::tool_loading::context_tools(&tool_defs, messages),
        )
    };
    let tool_tokens = context_tool_tokens(&state.messages);
    let initial_overhead = state.context_overhead_tokens.unwrap_or_else(|| {
        crate::context::estimate_static_tokens(opts.system.as_deref(), [])
            .saturating_add(tool_tokens)
    });
    let mut gauge =
        crate::context::ContextGauge::with_overhead(initial_overhead).with_tool_tokens(tool_tokens);

    let stop = loop {
        if sink.is_cancelled() {
            break AgentStop::Cancelled;
        }
        // --- 予算ガード（ステップに入る前に判定・Task 5.7）。 ---
        match opts.budget.check(&state.spent, Instant::now()) {
            // 上限で畳む前に、**ツールを外した 1 ターンだけ**回して成果物を書かせる。
            BudgetCheck::Exceeded(kind) => {
                land_on_budget(gateway, run, opts, &mut state, sink).await?;
                break AgentStop::Budget(kind);
            }
            BudgetCheck::Warn(kind, used, limit) => {
                if warned.insert(kind) {
                    sink.emit(AgentEvent::BudgetWarning { kind, used, limit })
                        .await?;
                }
            }
            BudgetCheck::Ok => {}
        }

        // --- コンテキスト剪定（自律版のみ・古い大きなツール出力を畳む・Task 5.3）。 ---
        if opts.profile.is_autonomous() {
            // tool_search で読み込んだ定義の増分を見積りに足す（次の実測で置き換わる）。
            gauge.observe_tools(context_tool_tokens(&state.messages));
            limits.apply(&mut state.messages, &gauge);
        }

        // 補正の基準（送る履歴の見積り）。剪定するのは自律版だけなので Chat では測らない。
        let sent_estimate = opts
            .profile
            .is_autonomous()
            .then(|| crate::context::estimate_prompt_tokens(&state.messages));
        let steps_before = state.spent.steps;
        let step_outcome = run_step(
            gateway,
            &tool_defs,
            &tool_map,
            tool_search.as_ref(),
            run,
            opts,
            approver,
            &mut state,
            sink,
            &mut detector,
        )
        .await?;
        // 生成まで進んだステップだけ実測で補正する（途中キャンセルは報告が無い）。
        if let Some(sent) = sent_estimate.filter(|_| state.spent.steps > steps_before) {
            gauge.calibrate(sent, state.spent.last_prompt_tokens());
            // このステップで読み込んだ定義は実測に入っていない。保存前に足しておく（再開時の
            // 基準は履歴から求めるので、ここで足さないと再開直後に見落とす）。
            gauge.observe_tools(context_tool_tokens(&state.messages));
            state.context_overhead_tokens = Some(gauge.overhead_tokens());
        }
        // ステップ境界でループ検出器の状態をチェックポイントへ畳み込む（中断/再開に耐える）。
        state.loop_detector = detector.clone();
        match step_outcome {
            // **継続する**ステップ境界のみ durable run へ永続化する（クラッシュ/takeover 時は
            // ここから再開する・#351）。終端ステップ（自然終了・予算・ループ検出）では保存しない:
            // finalize 前にクラッシュすると、最終応答を含むチェックポイントから再開して LLM を
            // もう一度呼び、二重応答/余分なツール実行が起きるため（takeover は一つ前の境界から
            // 終端ステップを再生成して収束する）。非永続シンクは no-op。
            StepOutcome::Continue => sink.save_checkpoint(&state).await?,
            StepOutcome::Stop(stop) => break stop,
        }
    };

    Ok(AgentOutcome {
        stop,
        checkpoint: state,
    })
}

/// 1 ステップの結果（継続 or 停止）。
enum StepOutcome {
    Continue,
    Stop(AgentStop),
}

/// LLM エラーをループのエラーへ写す。
///
/// **利用枠超過だけは種別を保つ**。文字列に潰すと「LLM が壊れた」と「枠を使い切った」の
/// 区別が消え、ユーザーにはプロバイダの生 JSON が出る（実測でその状態だった）。
fn map_llm_error(e: llm_gateway::LlmError) -> AgentError {
    match e {
        llm_gateway::LlmError::RateLimited(msg) => AgentError::RateLimited(msg),
        other => AgentError::Llm(other.to_string()),
    }
}

/// 予算超過でループを畳む直前に、**ツールを外した 1 ターン**だけ回して成果物を書かせる。
///
/// 残量を観測に載せる（`[予算]`）だけでは取りこぼす。実測では、着地指示を読んでいるはずの
/// 調査サブエージェントの 1/3 が最後までツールを呼び続け、**findings ゼロのまま上限で切られた**
/// ——トークンを使って何も返らない、純粋な損失になる。ここで機械的に保証する。
///
/// - **ツールを渡さない**ので、追加の取得は起こり得ない（上限を超えて調べ続けない）。
/// - コストは 1 生成ぶん。「それまでの仕事が丸ごと消える」方がはるかに高くつく。
///   トークン/コスト上限で切れた場合も同じ理由で書かせる（超過は 1 生成ぶんに留まる）。
/// - **キャンセル時は書かせない**（ユーザーが止めたのだから、そこで止める）。
/// - 添える先（ツール結果）が無い＝1 手も実行していないので、書かせる材料も無い。
async fn land_on_budget(
    gateway: &LlmGateway,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    state: &mut Checkpoint,
    sink: &mut dyn EventSink,
) -> Result<(), AgentError> {
    if sink.is_cancelled() || !demand_final_answer(&mut state.messages) {
        return Ok(());
    }
    let step = state.spent.steps;
    // Langfuse の所要時間に使う（未指定だと 0 秒と表示される）。
    let started_at = chrono::Utc::now();

    let mut stream = gateway
        .stream(
            run.ctx,
            GenerateRequest {
                model: opts.model.clone(),
                system: opts.system.clone(),
                messages: state.messages.clone(),
                tools: Vec::new(), // ← 追加の取得を構造的に不可能にする
                effort: opts.effort,
                max_tokens: opts.max_tokens,
                temperature: opts.temperature,
                session_id: opts.session_id.clone(),
            },
        )
        .await
        .map_err(map_llm_error)?;

    let mut text_acc = String::new();
    let mut usage = Usage::default();
    while let Some(delta) = stream.next().await {
        match delta.map_err(map_llm_error)? {
            StreamDelta::TextDelta { text } => {
                text_acc.push_str(&text);
                sink.emit(AgentEvent::Text(text)).await?;
            }
            StreamDelta::ThinkingDelta { text } => sink.emit(AgentEvent::Thinking(text)).await?,
            StreamDelta::Done { usage: u, .. } => usage = u,
            _ => {}
        }
    }

    // 会計は通常ステップと同じ扱い（実際に 1 回呼んでいるので、隠さず積む）。
    let cost = gateway.estimate_cost_usd_micros(
        opts.model
            .as_deref()
            .unwrap_or_else(|| gateway.default_model()),
        usage,
    );
    gateway
        .record_generation(
            run.ctx,
            &GenerationRecord {
                idempotency_key: format!("{}:{step}:land", run.idempotency_prefix),
                model: opts
                    .model
                    .clone()
                    .unwrap_or_else(|| gateway.default_model().to_string()),
                usage,
                trace_id: run.trace_id.clone(),
                input_preview: run.input_preview.clone(),
                output_preview: preview(&text_acc),
                app_id: run.app_id,
                source: usage_source(opts, run.app_id),
                // step 0 がそのリクエストの最初の生成。
                first_in_trace: run.owns_trace && step == 0,
                started_at: Some(started_at),
            },
        )
        .await;
    // **ステップは進めない**（`add_external`）。`spent.steps` はループの回数であり予算の軸で、
    // 着地は予算を使い切った**外側**で起こる。ここで +1 すると `steps > max_steps` になり、
    // チェックポイントから再開したときに「最初から予算超過」の状態が復元される。
    // トークンとコストは実際に使ったぶんを隠さず積む。
    state.spent.add_external(
        usage.prompt_tokens.saturating_add(usage.completion_tokens),
        usage.completion_tokens,
        cost,
    );
    if !text_acc.is_empty() {
        state.messages.push(LlmMessage {
            role: LlmRole::Assistant,
            content: vec![Block::Text { text: text_acc }],
        });
    }
    Ok(())
}

/// 1 ステップ（1 LLM 生成＋そのツール実行）を回し、状態を進める。
#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // ストリーム分岐＋ツール実行で伸びる。
async fn run_step(
    gateway: &LlmGateway,
    tool_defs: &[ToolDef],
    tool_map: &HashMap<&str, &Arc<dyn Tool>>,
    tool_search: Option<&crate::tool_search::ToolSearch>,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    approver: Option<&dyn Approver>,
    state: &mut Checkpoint,
    sink: &mut dyn EventSink,
    detector: &mut LoopDetector,
) -> Result<StepOutcome, AgentError> {
    let step = state.spent.steps;
    let req = GenerateRequest {
        model: opts.model.clone(),
        system: opts.system.clone(),
        messages: state.messages.clone(),
        tools: tool_defs.to_vec(),
        effort: opts.effort,
        max_tokens: opts.max_tokens,
        // skill のモデル既定（Task 6.9）。None は provider 既定。
        temperature: opts.temperature,
        session_id: opts.session_id.clone(),
    };
    // Langfuse の所要時間に使う（未指定だと 0 秒と表示される）。
    let started_at = chrono::Utc::now();
    let mut stream = gateway.stream(run.ctx, req).await.map_err(map_llm_error)?;

    let mut text_acc = String::new();
    let mut pending_names: HashMap<String, String> = HashMap::new();
    let mut calls: Vec<PendingCall> = Vec::new();
    // assistant メッセージのブロックを**届いた順**に積む（推論アイテムは直後の本文・ツール呼び出しと
    // 対で送り返す必要があり、並べ替えると Responses が拒否する・#542）。本文は直前のブロックが本文なら
    // そこへ足し、推論アイテム・ツール呼び出しの後なら新しい本文ブロックを始める。
    let mut ordered: Vec<Block> = Vec::new();
    let mut usage = Usage::default();
    let mut final_stop = StopReason::EndTurn;

    while let Some(delta) = stream.next().await {
        if sink.is_cancelled() {
            return Ok(StepOutcome::Stop(AgentStop::Cancelled));
        }
        match delta.map_err(map_llm_error)? {
            StreamDelta::TextDelta { text } => {
                if !text.is_empty() {
                    match ordered.last_mut() {
                        Some(Block::Text { text: last }) => last.push_str(&text),
                        _ => ordered.push(Block::Text { text: text.clone() }),
                    }
                }
                text_acc.push_str(&text);
                sink.emit(AgentEvent::Text(text)).await?;
            }
            StreamDelta::ThinkingDelta { text } => {
                sink.emit(AgentEvent::Thinking(text)).await?;
            }
            StreamDelta::ToolUseStart { id, name } => {
                pending_names.insert(id, name);
            }
            StreamDelta::ToolUseInputDelta { .. } => {}
            StreamDelta::ReasoningItem { provider, item } => {
                ordered.push(Block::Reasoning { provider, item });
            }
            StreamDelta::ToolUseStop { id, input } => {
                let name = pending_names
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                sink.emit(AgentEvent::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    // 同一ステップの呼び出しは同じ番号になる（UI の並行表示のグルーピング根拠）。
                    step: u32::try_from(step).unwrap_or(u32::MAX),
                })
                .await?;
                ordered.push(Block::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                calls.push(PendingCall { id, name, input });
            }
            StreamDelta::Done {
                stop_reason,
                usage: u,
            } => {
                final_stop = stop_reason;
                usage = u;
            }
        }
    }

    // 会計＋Langfuse 計装（attempt/ステップ単位）＋予算の累積更新（Task 5.7）。
    let cost = gateway.estimate_cost_usd_micros(
        opts.model
            .as_deref()
            .unwrap_or_else(|| gateway.default_model()),
        usage,
    );
    gateway
        .record_generation(
            run.ctx,
            &GenerationRecord {
                idempotency_key: format!("{}:{}", run.idempotency_prefix, step),
                model: opts
                    .model
                    .clone()
                    .unwrap_or_else(|| gateway.default_model().to_string()),
                usage,
                trace_id: run.trace_id.clone(),
                input_preview: run.input_preview.clone(),
                output_preview: preview(&text_acc),
                app_id: run.app_id,
                source: usage_source(opts, run.app_id),
                // step 0 がそのリクエストの最初の生成。
                first_in_trace: run.owns_trace && step == 0,
                started_at: Some(started_at),
            },
        )
        .await;
    state
        .spent
        .add_step(usage.prompt_tokens, usage.completion_tokens, cost);
    // チェックポイントの step を消費ステップ数に追従させる（再開時の起点・監査の整合）。
    state.step = state.spent.steps;

    // **本文もツール呼び出しも無い応答は「完了」ではない**。思考にだけ書いて content を空で
    // 返すプロバイダが実在する（実測: 委譲の子が 8 体中 3 体、本文ゼロで「正常終了」した）。
    // そのまま畳むと成果物が消えるので、**1 度だけ**書き直させる。空の assistant メッセージは
    // 積まない（content が空のメッセージを拒否するプロバイダがある）。
    if text_acc.is_empty() && calls.is_empty() && nudge_empty_response(&mut state.messages) {
        return Ok(StepOutcome::Continue);
    }

    // assistant メッセージを履歴へ（届いた順のまま）。
    state.messages.push(LlmMessage {
        role: LlmRole::Assistant,
        content: ordered,
    });

    // 終了判定: ツール呼び出しが無ければ終わり。ただし **max_tokens で切れたのは完了ではない**。
    if final_stop != StopReason::ToolUse || calls.is_empty() {
        return Ok(StepOutcome::Stop(if final_stop == StopReason::MaxTokens {
            AgentStop::Truncated
        } else {
            AgentStop::Completed
        }));
    }

    // ツール実行 → 観測を履歴へ。冪等 read は有界並列・それ以外は逐次（#349・agent_tools）。
    let phase = crate::agent_tools::ToolPhase {
        tool_map,
        tool_search,
        ctx: run.ctx,
        trace_id: run.trace_id.as_deref(),
        opts,
        approver,
    };
    let (mut result_blocks, looping, external) = match crate::agent_tools::run_tool_calls(
        &phase,
        calls,
        &mut state.plan,
        &mut state.citations,
        sink,
        detector,
    )
    .await?
    {
        crate::agent_tools::ToolPhaseOutcome::Cancelled { external } => {
            // キャンセルでも走った分は計上してから止める（「止めれば無料」を作らない）。
            state.spent.add_external(
                external.tokens,
                external.fresh_tokens,
                external.cost_usd_micros,
            );
            return Ok(StepOutcome::Stop(AgentStop::Cancelled));
        }
        crate::agent_tools::ToolPhaseOutcome::Executed {
            blocks,
            looping,
            external,
        } => (blocks, looping, external),
    };
    // ツールの内側で起きた LLM 消費（サブエージェント委譲・#391）を親の会計へ積む。
    // steps は増やさない（親のループ回数の指標を保つ）。次のステップ境界の `Budget::check` が
    // 子の消費込みで判定するため、**トークン/コスト上限で確実に止まる**。
    if external.tokens > 0 || external.cost_usd_micros > 0 {
        state.spent.add_external(
            external.tokens,
            external.fresh_tokens,
            external.cost_usd_micros,
        );
    }
    // 残り予算を**観測として**添える（#407）。`BudgetWarning` は sink 専用でモデルに届かず、
    // エージェントは残量を知らないまま上限で切られていた（実測: 委譲 7 体中 6 体が Budget(Steps)）。
    if opts.profile.is_autonomous() {
        append_budget_observation(&mut result_blocks, &opts.budget, &state.spent);
    }
    state.messages.push(LlmMessage {
        role: LlmRole::Tool,
        content: result_blocks,
    });

    if looping {
        sink.emit(AgentEvent::FailureRecovery {
            detail: "同一ツール呼び出しの失敗が反復したため安全停止しました".to_string(),
            action: RecoveryAction::StopLooping,
        })
        .await?;
        return Ok(StepOutcome::Stop(AgentStop::LoopDetected));
    }
    Ok(StepOutcome::Continue)
}

/// 提示するツール定義を組み立てる（自律版は `plan` メタツールを足す）。
fn build_tool_defs(tools: &[Arc<dyn Tool>], opts: &AgentOptions) -> Vec<ToolDef> {
    let mut defs: Vec<ToolDef> = tools
        .iter()
        .map(|t| ToolDef::new(t.name(), t.description(), t.input_schema()))
        .collect();
    if opts.profile.is_autonomous() && opts.offer_plan_tool {
        defs.push(ToolDef::new(
            PLAN_TOOL,
            "現在の計画（サブタスク列）を提示/改訂する。目標を数個のサブタスクに分解し、\
            進捗に応じて全置換で更新する（各要素に status: todo/doing/done/blocked）。\
            計画は UI に表示され進捗が追跡される。",
            plan::plan_tool_schema(),
        ));
    }
    defs
}

/// Langfuse 表示用に長文を切り詰める。
fn preview(s: &str) -> String {
    const MAX: usize = 2000;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut end = MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// 会計の利用元（管理画面の利用元別集計・#553）。ミニアプリ経由はミニアプリ、
/// チャット制約版はチャット、自律版はエージェントとして数える。
fn usage_source(opts: &AgentOptions, app_id: Option<uuid::Uuid>) -> llm_gateway::UsageSource {
    if app_id.is_some() {
        llm_gateway::UsageSource::MiniApp
    } else if opts.profile.is_autonomous() {
        llm_gateway::UsageSource::Agent
    } else {
        llm_gateway::UsageSource::Chat
    }
}
