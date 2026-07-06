//! agent-core の制約版ループ（Task 3.3 / 3.9）。
//!
//! LLM↔ツールのループ（計画→ツール呼出→観測→継続→終了）。ツールセット非依存で [`Tool`] を差す。
//! Phase 3 は短ホライズン・安全ツールのみ。ツール呼出/結果/引用/トークンは [`EventSink`] で逐次
//! 外部化し、chat 側で SSE イベント＋content block へ写す。最大ステップ/デッドラインで安全停止する。
//!
//! ツール自動選択ポリシ（Task 3.9）: **利用可能ツールを全提示し、モデルが自動選択**する。
//! `requires_confirmation()` なツール（破壊/権限/高コスト系）は事前許可（`allow_confirmed_tools`）が
//! 無ければ実行せず、モデルに「確認が必要」と観測させる（破壊系を確認なしに実行しない）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use authz::AuthContext;
use futures::stream::StreamExt;
use llm_gateway::{
    Block, Effort, GenerateRequest, GenerationRecord, LlmGateway, Message as LlmMessage,
    Role as LlmRole, StopReason, StreamDelta, ToolDef, Usage,
};

use crate::event::{AgentError, AgentEvent, EventSink};
use crate::tool::Tool;

/// ループの停止理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStop {
    /// モデルが自然終了した。
    Completed,
    /// 最大ステップに達した。
    MaxSteps,
    /// キャンセル要求で停止した。
    Cancelled,
}

/// ループのオプション。
pub struct AgentOptions {
    /// 最大ステップ（LLM 呼び出し回数の上限・安全停止）。
    pub max_steps: usize,
    /// トップレベル system プロンプト。
    pub system: Option<String>,
    /// 論理モデル名（未指定は gateway 既定）。
    pub model: Option<String>,
    /// 思考強度。
    pub effort: Option<Effort>,
    /// 1 応答の max_tokens。
    pub max_tokens: Option<u32>,
    /// requires_confirmation なツールを実行してよいか（事前許可）。
    pub allow_confirmed_tools: bool,
    /// 全体デッドライン（超えたらステップ境界で停止）。
    pub deadline: Option<Instant>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        AgentOptions {
            max_steps: 8,
            system: None,
            model: None,
            effort: None,
            max_tokens: Some(2048),
            allow_confirmed_tools: false,
            deadline: None,
        }
    }
}

/// 1 run の会計/計装コンテキスト。
pub struct RunContext<'a> {
    pub ctx: &'a AuthContext,
    /// 冪等キーの接頭辞（`<run_id>:<attempt>`）。ステップごとに `:<step>` を足す。
    pub idempotency_prefix: String,
    pub trace_id: Option<String>,
    /// Langfuse 入力プレビュー（発話）。
    pub input_preview: String,
}

/// 累積中のツール呼び出し（1 ステップ分）。
struct PendingCall {
    id: String,
    name: String,
    input: serde_json::Value,
}

/// エージェントループを回す。ツールは全提示し、モデルが自動選択する。
#[allow(clippy::too_many_lines)] // ストリーム分岐＋ツール実行で行数が伸びる（分割は流れを損なう）。
pub async fn run_agent(
    gateway: &LlmGateway,
    tools: &[Arc<dyn Tool>],
    mut messages: Vec<LlmMessage>,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    sink: &mut dyn EventSink,
) -> Result<AgentStop, AgentError> {
    let tool_defs: Vec<ToolDef> = tools
        .iter()
        .map(|t| ToolDef {
            name: t.name().to_string(),
            description: t.description().to_string(),
            input_schema: t.input_schema(),
        })
        .collect();
    let tool_map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();

    for step in 0..opts.max_steps {
        if sink.is_cancelled() {
            return Ok(AgentStop::Cancelled);
        }
        if opts.deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(AgentStop::MaxSteps);
        }

        let req = GenerateRequest {
            model: opts.model.clone(),
            system: opts.system.clone(),
            messages: messages.clone(),
            tools: tool_defs.clone(),
            effort: opts.effort,
            max_tokens: opts.max_tokens,
            temperature: None,
        };
        let mut stream = gateway
            .stream(req)
            .await
            .map_err(|e| AgentError::Llm(e.to_string()))?;

        let mut text_acc = String::new();
        let mut pending_names: HashMap<String, String> = HashMap::new();
        let mut calls: Vec<PendingCall> = Vec::new();
        let mut usage = Usage::default();
        let mut stop = StopReason::EndTurn;
        let mut cancelled_mid = false;

        while let Some(delta) = stream.next().await {
            if sink.is_cancelled() {
                cancelled_mid = true;
                break;
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
                // 入力 JSON の逐次差分は使わない（ToolUseStop で完全な入力を受け取る）。
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
                    })
                    .await?;
                    calls.push(PendingCall { id, name, input });
                }
                StreamDelta::Done {
                    stop_reason,
                    usage: u,
                } => {
                    stop = stop_reason;
                    usage = u;
                }
            }
        }

        // このターンの会計＋Langfuse 計装（attempt/ステップ単位で刻む）。
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
                },
            )
            .await;

        if cancelled_mid {
            return Ok(AgentStop::Cancelled);
        }

        // assistant メッセージを履歴へ（次ターンの入力に）。
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
        messages.push(LlmMessage {
            role: LlmRole::Assistant,
            content: assistant_blocks,
        });

        // 終了判定: ツール呼び出しが無ければ完了。
        if stop != StopReason::ToolUse || calls.is_empty() {
            return Ok(AgentStop::Completed);
        }

        // ツール実行 → 観測を履歴へ。
        let mut result_blocks: Vec<Block> = Vec::new();
        for c in calls {
            let outcome = execute_tool(&tool_map, run.ctx, &c, opts, run.trace_id.as_deref()).await;
            sink.emit(AgentEvent::ToolResult {
                tool_call_id: c.id.clone(),
                ok: !outcome.is_error,
                content: outcome.content.clone(),
            })
            .await?;
            for cite in outcome.citations {
                sink.emit(AgentEvent::Citation(cite)).await?;
            }
            // 成果物（保存済みファイル参照）を UI へ流す（chat 側で FileRef へ写す）。
            for artifact in outcome.artifacts {
                sink.emit(AgentEvent::Artifact {
                    tool_call_id: c.id.clone(),
                    artifact,
                })
                .await?;
            }
            result_blocks.push(Block::ToolResult {
                tool_use_id: c.id,
                content: outcome.content,
                is_error: outcome.is_error,
            });
        }
        messages.push(LlmMessage {
            role: LlmRole::Tool,
            content: result_blocks,
        });
    }

    Ok(AgentStop::MaxSteps)
}

/// 1 ツール呼び出しを実行する（未知/確認必須は観測エラーへ）。
async fn execute_tool(
    tool_map: &HashMap<&str, &Arc<dyn Tool>>,
    ctx: &AuthContext,
    call: &PendingCall,
    opts: &AgentOptions,
    trace_id: Option<&str>,
) -> crate::tool::ToolOutcome {
    use crate::tool::ToolOutcome;
    let Some(tool) = tool_map.get(call.name.as_str()) else {
        return ToolOutcome::error(format!("unknown tool: {}", call.name));
    };
    // 破壊/権限/高コスト系は事前許可が無ければ実行しない（Task 3.9）。
    if tool.requires_confirmation() && !opts.allow_confirmed_tools {
        return ToolOutcome::error(format!(
            "tool '{}' requires explicit confirmation and was not executed",
            call.name
        ));
    }
    match tool.call(ctx, call.input.clone(), trace_id).await {
        Ok(o) => o,
        Err(e) => ToolOutcome::error(format!("tool '{}' failed: {e}", call.name)),
    }
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
