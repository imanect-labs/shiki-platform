//! 1 ステップ分のツール実行フェーズ（issue #349）。
//!
//! **冪等 read（[`Tool::is_read_only`]）は有界並列、それ以外は逐次**に実行する。
//! deep research は専用エンジンではなく agent-core のループに乗る（design §4.4）ため、
//! `web_search` → 複数 `web_fetch` のファンアウトが直列だと `N 件 × レイテンシ` を積む。
//!
//! 守る性質:
//! - **観測順の決定性**: 並列に走らせても、`ToolResult` ブロックとイベント発火は**常に
//!   呼び出し順**（再現性・監査・ループ検出の積算がステップ間で揺れない）。
//! - **承認ゲートの不変**: 承認要（`requires_confirmation` / 自律版の egress）は逐次のまま
//!   [`Approver`] でブロックする。read はその承認待ちと**並行**して進む。
//! - **同一ホストへの礼儀**: 同じホストへの `web_fetch` は互いに直列化する（1 ステップから
//!   同一サイトへ同時多発しない）。異なるホスト同士だけが並列に走る。

use std::collections::HashMap;
use std::sync::Arc;

use authz::AuthContext;
use futures::stream::StreamExt;
use llm_gateway::Block;

use crate::agent::{PendingCall, PLAN_TOOL};
use crate::agent_gate::{authorize, emit_tool_events, execute_tool, is_gated, Authz};
use crate::approval::Approver;
use crate::event::{AgentError, AgentEvent, EventSink, RecoveryAction};
use crate::loop_detect::LoopDetector;
use crate::plan::{self, Plan};
use crate::profile::AgentOptions;
use crate::tool::{Tool, ToolOutcome};

/// ツール実行フェーズの不変な入力（run 中ずっと同じもの）。
pub(crate) struct ToolPhase<'a> {
    pub(crate) tool_map: &'a HashMap<&'a str, &'a Arc<dyn Tool>>,
    pub(crate) ctx: &'a AuthContext,
    pub(crate) trace_id: Option<&'a str>,
    pub(crate) opts: &'a AgentOptions,
    pub(crate) approver: Option<&'a dyn Approver>,
}

/// ツール実行フェーズの結果。
pub(crate) enum ToolPhaseOutcome {
    /// 承認待ち中にキャンセルされた（run を停止する）。
    Cancelled,
    /// 全呼び出しを処理した（観測ブロックは呼び出し順）。
    Executed {
        blocks: Vec<Block>,
        /// 失敗ループを検出した（自律版のみ・5.5）。
        looping: bool,
        /// ツールの内側で起きた LLM 消費の合計（`subagent`・#391）。
        /// 呼び出し側が親の `Spent` へ `add_external` で積む（子の消費で親の予算が止まる）。
        external: crate::tool::ToolUsage,
    },
}

/// 1 呼び出しの扱い（ループ検出/リカバリイベントの出し分けに使う）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// ツールを実際に実行した。
    Executed,
    /// 未許可/却下で実行しなかった。
    Rejected,
    /// `plan` メタツール（ループが横取りした）。
    Plan,
}

/// 1 呼び出しの処理結果（呼び出し順の添字つき）。
struct Processed {
    index: usize,
    outcome: ToolOutcome,
    disposition: Disposition,
}

/// このステップのツール呼び出しを実行し、観測ブロックを**呼び出し順**で返す。
pub(crate) async fn run_tool_calls(
    phase: &ToolPhase<'_>,
    calls: Vec<PendingCall>,
    plan_state: &mut Plan,
    sink: &mut dyn EventSink,
    detector: &mut LoopDetector,
) -> Result<ToolPhaseOutcome, AgentError> {
    // --- 1. 分類（await なし）: 並列に回せる冪等 read か、逐次経路か。 ---
    let parallel: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, c)| is_parallel_read(phase, c))
        .map(|(i, _)| i)
        .collect();

    // --- 2. 並列 read と逐次経路を**同時に**走らせる。 ---
    // read は承認待ち（逐次側）にブロックされない。逐次側だけが sink/plan を可変で借りる。
    let reads = run_reads(phase, &calls, &parallel);
    let sequential = run_sequential(phase, &calls, &parallel, plan_state, sink);
    let (read_results, seq) = futures::future::join(reads, sequential).await;
    let seq = seq?;

    // --- 3. 合流して呼び出し順に並べ直す（観測順の決定性）。 ---
    let mut slots: Vec<Option<Processed>> = (0..calls.len()).map(|_| None).collect();
    for p in read_results.into_iter().chain(seq.results) {
        let at = p.index;
        slots[at] = Some(p);
    }
    // --- 4. 呼び出し順にイベント発火・ループ検出・観測ブロック積み。 ---
    //
    // キャンセル時も**実際に走った分の観測は外部化する**（read は承認待ちと並行して
    // 完了し得るため、黙って捨てると「実行したのに UI/監査に何も残らない」穴になる）。
    let mut blocks: Vec<Block> = Vec::with_capacity(calls.len());
    let mut looping = false;
    let mut external = crate::tool::ToolUsage::default();
    for (call, slot) in calls.into_iter().zip(slots) {
        // キャンセルで未処理のまま残った呼び出しは飛ばす。
        let Some(p) = slot else { continue };
        if let Some(u) = p.outcome.usage {
            external.tokens = external.tokens.saturating_add(u.tokens);
            external.cost_usd_micros = external.cost_usd_micros.saturating_add(u.cost_usd_micros);
        }
        emit_tool_events(sink, &call, &p.outcome).await?;
        if phase.opts.profile.is_autonomous() && p.disposition != Disposition::Plan {
            if p.outcome.is_error && p.disposition == Disposition::Executed {
                sink.emit(AgentEvent::FailureRecovery {
                    detail: format!("tool '{}' failed; retrying with observation", call.name),
                    action: RecoveryAction::Retry,
                })
                .await?;
            }
            // 却下も失敗としてループ検出へ流す（同じ却下操作の反復を安全停止する）。
            if detector.observe(&call.name, &call.input, p.outcome.is_error) {
                looping = true;
            }
        }
        blocks.push(Block::ToolResult {
            tool_use_id: call.id,
            content: p.outcome.content,
            is_error: p.outcome.is_error,
        });
    }
    if seq.cancelled {
        return Ok(ToolPhaseOutcome::Cancelled);
    }
    Ok(ToolPhaseOutcome::Executed {
        blocks,
        looping,
        external,
    })
}

/// 同一ステップ内で並列に回してよい呼び出しか。
///
/// 「確認不要 ＝ 並列にしてよい」ではない。副作用の無さを**ツール自身が表明**していること
/// （[`Tool::is_read_only`]）と、承認ゲートの**対象ですらない**ことの両方を要求する。
/// 事前許可で通るだけの呼び出しは並列にしない（ポリシは実行中に変わり得るため・#350）。
fn is_parallel_read(phase: &ToolPhase<'_>, call: &PendingCall) -> bool {
    if phase.opts.profile.is_autonomous() && call.name == PLAN_TOOL {
        return false;
    }
    phase
        .tool_map
        .get(call.name.as_str())
        .is_some_and(|t| t.is_read_only())
        && !is_gated(phase.tool_map, call, phase.opts)
}

/// 冪等 read を有界並列で実行する（同一ホストへの `web_fetch` は互いに直列）。
async fn run_reads(
    phase: &ToolPhase<'_>,
    calls: &[PendingCall],
    parallel: &[usize],
) -> Vec<Processed> {
    // ホスト単位のレーンへ振り分ける。同じレーンの中は逐次、レーン同士が並列。
    let mut lanes: Vec<Vec<usize>> = Vec::new();
    let mut lane_of: HashMap<String, usize> = HashMap::new();
    for &i in parallel {
        match politeness_key(&calls[i]) {
            Some(key) => {
                let lane = *lane_of.entry(key).or_insert_with(|| {
                    lanes.push(Vec::new());
                    lanes.len() - 1
                });
                lanes[lane].push(i);
            }
            None => lanes.push(vec![i]),
        }
    }
    let limit = phase.opts.parallel_read_tools.max(1);
    futures::stream::iter(lanes.into_iter().map(|lane| async move {
        let mut done = Vec::with_capacity(lane.len());
        for i in lane {
            done.push(Processed {
                index: i,
                outcome: execute_tool(phase.tool_map, phase.ctx, &calls[i], phase.trace_id).await,
                disposition: Disposition::Executed,
            });
        }
        done
    }))
    .buffer_unordered(limit)
    .flat_map(futures::stream::iter)
    .collect()
    .await
}

/// 直列化のキー（同一ホストへの `web_fetch` を束ねる）。`None` は他と束ねない＝単独レーン。
fn politeness_key(call: &PendingCall) -> Option<String> {
    if crate::vocab::ToolName::parse(&call.name) != Some(crate::vocab::ToolName::WebFetch) {
        return None;
    }
    let url = call.input.get("url").and_then(serde_json::Value::as_str)?;
    let host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    Some(host)
}

/// 逐次経路の結果（キャンセルなら以降は処理しない）。
struct SequentialRun {
    results: Vec<Processed>,
    cancelled: bool,
}

/// 並列対象**以外**を呼び出し順に処理する（承認ゲート・plan メタツール・破壊系）。
async fn run_sequential(
    phase: &ToolPhase<'_>,
    calls: &[PendingCall],
    parallel: &[usize],
    plan_state: &mut Plan,
    sink: &mut dyn EventSink,
) -> Result<SequentialRun, AgentError> {
    let mut results = Vec::new();
    for (i, call) in calls.iter().enumerate() {
        if parallel.contains(&i) {
            continue;
        }
        if phase.opts.profile.is_autonomous() && call.name == PLAN_TOOL {
            results.push(Processed {
                index: i,
                outcome: ToolOutcome::ok(handle_plan_tool(call, plan_state, sink).await?),
                disposition: Disposition::Plan,
            });
            continue;
        }
        // 承認ゲート（Task 5.6）: 破壊系は事前許可 or ユーザー承認まで実行しない。
        let (outcome, disposition) =
            match authorize(phase.tool_map, call, phase.opts, phase.approver, sink).await? {
                Authz::Cancel => {
                    return Ok(SequentialRun {
                        results,
                        cancelled: true,
                    })
                }
                Authz::Reject(msg) => (ToolOutcome::error(msg), Disposition::Rejected),
                Authz::Proceed => (
                    execute_tool(phase.tool_map, phase.ctx, call, phase.trace_id).await,
                    Disposition::Executed,
                ),
            };
        results.push(Processed {
            index: i,
            outcome,
            disposition,
        });
    }
    Ok(SequentialRun {
        results,
        cancelled: false,
    })
}

/// `plan` メタツールを処理する（計画を改訂し、変化を [`AgentEvent::PlanUpdated`] で外部化）。
///
/// ツール結果イベント自体は合流後に呼び出し順で発火するため、ここでは出さない。
async fn handle_plan_tool(
    call: &PendingCall,
    current: &mut Plan,
    sink: &mut dyn EventSink,
) -> Result<String, AgentError> {
    let inputs = plan::parse_plan_input(&call.input);
    // 空入力（不正 JSON・subtasks 欠落）で既存の計画を消さない（誤消去防止）。空なら現状維持。
    if inputs.is_empty() && !current.subtasks.is_empty() {
        return Ok("計画の更新入力が空だったため、現在の計画を維持しました。".to_string());
    }
    if current.revise(inputs) {
        sink.emit(AgentEvent::PlanUpdated(current.clone())).await?;
    }
    let (done, total) = current.progress();
    Ok(format!("計画を更新しました（{done}/{total} 完了）。"))
}

#[cfg(test)]
mod tests;
