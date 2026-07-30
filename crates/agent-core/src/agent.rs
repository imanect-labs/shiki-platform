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

use crate::approval::Approver;
use crate::budget::BudgetCheck;
use crate::checkpoint::Checkpoint;
use crate::event::{AgentError, AgentEvent, EventSink, RecoveryAction};
use crate::loop_detect::LoopDetector;
use crate::plan;
use crate::profile::{AgentOptions, AgentOutcome};
use crate::tool::Tool;

/// 計画メタツールの名前（自律版のみ提示・ループが横取りしてツールへは dispatch しない）。
pub(crate) const PLAN_TOOL: &str = "plan";

/// ループの停止理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStop {
    /// モデルが自然終了した。
    Completed,
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
    let tool_defs = build_tool_defs(tools, opts);

    // 再開 or 新規開始の状態。ループ検出器はチェックポイントから復元する（resume で失敗履歴を失わない）。
    let mut state = resume.unwrap_or_else(|| Checkpoint::start(messages));
    let mut detector = state.loop_detector.clone();
    let mut warned: std::collections::HashSet<crate::budget::BudgetKind> =
        std::collections::HashSet::new();

    let stop = loop {
        if sink.is_cancelled() {
            break AgentStop::Cancelled;
        }
        // --- 予算ガード（ステップに入る前に判定・Task 5.7）。 ---
        match opts.budget.check(&state.spent, Instant::now()) {
            BudgetCheck::Exceeded(kind) => break AgentStop::Budget(kind),
            BudgetCheck::Warn(kind, used, limit) => {
                if warned.insert(kind) {
                    sink.emit(AgentEvent::BudgetWarning { kind, used, limit })
                        .await?;
                }
            }
            BudgetCheck::Ok => {}
        }

        // --- コンテキスト剪定（自律版のみ・古い大きなツール出力を畳む・Task 5.3）。 ---
        if opts.profile.is_autonomous() && opts.context_soft_limit_tokens > 0 {
            crate::context::prune_history(
                &mut state.messages,
                opts.context_soft_limit_tokens,
                opts.context_keep_recent,
            );
        }

        let step_outcome = run_step(
            gateway,
            &tool_defs,
            &tool_map,
            run,
            opts,
            approver,
            &mut state,
            sink,
            &mut detector,
        )
        .await?;
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

/// 1 ステップ（1 LLM 生成＋そのツール実行）を回し、状態を進める。
#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // ストリーム分岐＋ツール実行で伸びる。
async fn run_step(
    gateway: &LlmGateway,
    tool_defs: &[ToolDef],
    tool_map: &HashMap<&str, &Arc<dyn Tool>>,
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
    };
    let mut stream = gateway
        .stream(req)
        .await
        .map_err(|e| AgentError::Llm(e.to_string()))?;

    let mut text_acc = String::new();
    let mut pending_names: HashMap<String, String> = HashMap::new();
    let mut calls: Vec<PendingCall> = Vec::new();
    let mut usage = Usage::default();
    let mut final_stop = StopReason::EndTurn;

    while let Some(delta) = stream.next().await {
        if sink.is_cancelled() {
            return Ok(StepOutcome::Stop(AgentStop::Cancelled));
        }
        match delta.map_err(|e| AgentError::Llm(e.to_string()))? {
            StreamDelta::TextDelta { text } => {
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
            },
        )
        .await;
    state
        .spent
        .add_step(usage.prompt_tokens + usage.completion_tokens, cost);
    // チェックポイントの step を消費ステップ数に追従させる（再開時の起点・監査の整合）。
    state.step = state.spent.steps;

    // assistant メッセージを履歴へ。
    let mut assistant_blocks: Vec<Block> = Vec::new();
    if !text_acc.is_empty() {
        assistant_blocks.push(Block::Text { text: text_acc });
    }
    for c in &calls {
        assistant_blocks.push(Block::ToolUse {
            id: c.id.clone(),
            name: c.name.clone(),
            input: c.input.clone(),
        });
    }
    state.messages.push(LlmMessage {
        role: LlmRole::Assistant,
        content: assistant_blocks,
    });

    // 終了判定: ツール呼び出しが無ければ完了。
    if final_stop != StopReason::ToolUse || calls.is_empty() {
        return Ok(StepOutcome::Stop(AgentStop::Completed));
    }

    // ツール実行 → 観測を履歴へ。冪等 read は有界並列・それ以外は逐次（#349・agent_tools）。
    let phase = crate::agent_tools::ToolPhase {
        tool_map,
        ctx: run.ctx,
        trace_id: run.trace_id.as_deref(),
        opts,
        approver,
    };
    let (result_blocks, looping, external) =
        match crate::agent_tools::run_tool_calls(&phase, calls, &mut state.plan, sink, detector)
            .await?
        {
            crate::agent_tools::ToolPhaseOutcome::Cancelled { external } => {
                // キャンセルでも走った分は計上してから止める（「止めれば無料」を作らない）。
                state
                    .spent
                    .add_external(external.tokens, external.cost_usd_micros);
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
        state
            .spent
            .add_external(external.tokens, external.cost_usd_micros);
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
        .map(|t| ToolDef {
            name: t.name().to_string(),
            description: t.description().to_string(),
            input_schema: t.input_schema(),
        })
        .collect();
    if opts.profile.is_autonomous() && opts.offer_plan_tool {
        defs.push(ToolDef {
            name: PLAN_TOOL.to_string(),
            description:
                "現在の計画（サブタスク列）を提示/改訂する。目標を数個のサブタスクに分解し、\
                進捗に応じて全置換で更新する（各要素に status: todo/doing/done/blocked）。\
                計画は UI に表示され進捗が追跡される。"
                    .to_string(),
            input_schema: plan::plan_tool_schema(),
        });
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
