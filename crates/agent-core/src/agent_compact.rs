//! 圧縮（[`crate::compaction`]）をループから実行する: LLM 呼び出し・会計・イベント・失敗時の後退。

use llm_gateway::{GenerationRecord, LlmGateway};

use crate::agent::RunContext;
use crate::checkpoint::Checkpoint;
use crate::event::{AgentError, AgentEvent, EventSink, RecoveryAction};
use crate::profile::AgentOptions;

/// 圧縮を試みる。置き換えたら `true`。要約の失敗（LLM エラー・空・縮まない）はループを止めず
/// `false` を返す（呼び出し側は剪定へ後退する）。
pub(crate) async fn try_compact(
    gateway: &LlmGateway,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    state: &mut Checkpoint,
    sink: &mut dyn EventSink,
    budget: crate::compaction::CompactBudget,
) -> Result<bool, AgentError> {
    if sink.is_cancelled() {
        return Ok(false);
    }
    let started_at = chrono::Utc::now();
    let outcome =
        crate::compaction::compact(gateway, run.ctx, opts, &state.messages, &state.plan, budget)
            .await;
    let mut compacted = match outcome {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(false),
        Err(e) => {
            tracing::warn!(error = %e, "文脈の圧縮に失敗（剪定で続行）");
            return Ok(false);
        }
    };
    // 要約にも 1 生成ぶんを使っている。成否によらず隠さず積む（ステップは進めない）。
    let cost = gateway.estimate_cost_usd_micros(&compacted.model, compacted.usage);
    gateway
        .record_generation(
            run.ctx,
            &GenerationRecord {
                idempotency_key: format!(
                    "{}:{}:compact{}",
                    run.idempotency_prefix, state.spent.steps, state.compactions
                ),
                model: compacted.model.clone(),
                usage: compacted.usage,
                trace_id: run.trace_id.clone(),
                input_preview: run.input_preview.clone(),
                output_preview: String::new(),
                app_id: run.app_id,
                source: crate::agent::usage_source(opts, run.app_id),
                first_in_trace: false,
                started_at: Some(started_at),
            },
        )
        .await;
    state.spent.add_external(
        compacted
            .usage
            .prompt_tokens
            .saturating_add(compacted.usage.completion_tokens),
        compacted.usage.completion_tokens,
        cost,
    );
    state.compactions = state.compactions.saturating_add(1);
    if compacted.messages.is_empty() {
        tracing::warn!("文脈の圧縮で要約が得られなかった（剪定で続行）");
        return Ok(false);
    }
    // 要約に回した区間の原本を保存し、要約に索引を足す（元に戻せる圧縮・[`crate::archive`]）。
    if let Some(archive) = opts.archive.as_deref() {
        let split = compacted.split;
        let middle = state.messages[split.head_end..split.tail_start].to_vec();
        crate::agent_archive::archive_history(
            archive,
            run.ctx,
            state,
            &middle,
            &mut compacted.messages,
        )
        .await;
    }
    state.messages = compacted.messages;
    sink.emit(AgentEvent::FailureRecovery {
        detail: format!(
            "文脈の上限に近づいたため、古いやり取りを要約に置き換えました（推定 {} → {} トークン）",
            compacted.before_tokens, compacted.after_tokens
        ),
        action: RecoveryAction::Compacted,
    })
    .await?;
    Ok(true)
}

/// ステップの前に見る窓の状態（閾値・見積り・ツール定義の見積り）。
#[derive(Clone, Copy)]
pub(crate) struct WindowView<'a> {
    pub limits: &'a crate::context::ContextLimits,
    pub gauge: &'a crate::context::ContextGauge,
    /// 文脈に載るツール定義の見積り（遅延ロード分を含む・このステップ時点）。
    pub tool_est: usize,
}

impl<'a> WindowView<'a> {
    pub(crate) fn new(
        limits: &'a crate::context::ContextLimits,
        gauge: &'a crate::context::ContextGauge,
        tool_est: usize,
    ) -> Self {
        WindowView {
            limits,
            gauge,
            tool_est,
        }
    }
}

/// ループをまたぐ圧縮の状態。
pub(crate) struct CompactionState {
    /// system プロンプトの見積り（run 内で固定・トークナイザ比の分母に足す）。
    system_est: usize,
    /// 直近に圧縮したステップ（直後のステップは、非常閾値を超えない限り繰り返さない）。
    last_step: Option<usize>,
    /// 直近の圧縮が失敗した（soft の剪定を後ろ盾として戻す）。
    failing: bool,
    /// 実トークン ÷ 手元の見積り（[`crate::context::ContextGauge::ratio`]・ステップごとに更新）。
    ratio: f64,
    /// 窓あふれのやり直しを済ませたか（生成まで進んだステップで戻す）。
    pub(crate) overflow_retried: bool,
    /// 引き上げた出力上限が窓を超えて拒否された（このステップは引き上げずにやり直す）。
    pub(crate) boost_refused: bool,
}

impl CompactionState {
    /// `system_est` = system プロンプトの見積り（[`crate::context::estimate_static_tokens`]）。
    pub(crate) fn new(system_est: usize) -> Self {
        CompactionState {
            system_est,
            last_step: None,
            failing: false,
            ratio: 1.0,
            overflow_retried: false,
            boost_refused: false,
        }
    }

    /// このステップに使う設定（出力上限で呼び出しが切れた直後だけ上限を引き上げる・
    /// [`crate::agent_observation::boosted_options`]）。引き上げないなら `None`。
    pub(crate) fn step_options(
        &self,
        opts: &AgentOptions,
        messages: &[llm_gateway::Message],
        view: WindowView<'_>,
    ) -> Option<AgentOptions> {
        if self.boost_refused {
            return None;
        }
        crate::agent_observation::boosted_options(opts, messages, view.limits, view.gauge)
    }

    /// 生成まで進んだステップの後に、やり直しの印を戻す。
    pub(crate) fn step_done(&mut self) {
        self.overflow_retried = false;
        self.boost_refused = false;
    }

    /// 実トークンで決めた量（1 ステップの結果の上限）を、手元の見積りの単位へ直す。
    #[must_use]
    pub(crate) fn to_estimate(&self, real: Option<usize>) -> Option<usize> {
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )] // トークン数の換算（正の小さな値）。
        real.map(|r| (r as f64 / self.ratio) as usize)
    }
}

/// ステップの前の文脈管理（自律版）: 窓を知っていて見積りが soft を超えていれば要約で置き換え、
/// 残りを剪定する。
///
/// 圧縮を使う run では、soft の畳み込みは圧縮が受け持つ（非常閾値だけ畳む）。直近の圧縮が
/// 失敗していれば、従来どおり soft でも畳む。圧縮の直後のステップは、非常閾値を超えない限り
/// 繰り返さない（尻尾そのものが大きいときに毎ステップ要約しない・要約の費用を抑える）。
pub(crate) async fn before_step(
    gateway: &LlmGateway,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    state: &mut Checkpoint,
    sink: &mut dyn EventSink,
    view: WindowView<'_>,
    cs: &mut CompactionState,
) -> Result<(), AgentError> {
    let (limits, gauge) = (view.limits, view.gauge);
    cs.ratio = gauge.ratio(&state.messages, cs.system_est + view.tool_est);
    if let Some(w) = limits.window.filter(|_| opts.compaction.enabled) {
        let cooled = cs.last_step.is_none_or(|s| state.spent.steps >= s + 2);
        let tokens = gauge.tokens(&state.messages);
        // 冷却中でも非常閾値を超えるなら要約する。待つと、この後の剪定が未要約の結果を畳む
        // （要約が大きい・1 ステップの結果が多いと、圧縮の直後のステップで超える）。
        if tokens > w.soft && (cooled || tokens > w.hard) {
            let steps = state.spent.steps;
            let budget = crate::compaction::CompactBudget {
                effective_window: w.effective,
                keep_permille: opts.compaction.keep_recent_permille,
                scale: cs.ratio,
            };
            let ok = try_compact(gateway, run, opts, state, sink, budget).await?;
            if ok {
                cs.last_step = Some(steps);
            }
            cs.failing = !ok;
        }
    }
    limits.apply_with(
        &mut state.messages,
        gauge,
        !opts.compaction.enabled || cs.failing,
    );
    Ok(())
}

/// 窓あふれで拒否された直後の立て直し: 圧縮（できなければ非常の剪定）。やり直せる形にできたか。
/// やり直しは 1 度だけ（`cs.overflow_retried` を立てる。生成まで進んだら [`CompactionState::step_done`] で戻す）。
///
/// 拒否は応答の前に起きるので、履歴にはそのステップの痕跡が無い（やり直して安全）。窓を
/// 知らないモデルでは、今の見積りの 3/4 を窓とみなして要約する。
pub(crate) async fn recover_overflow(
    gateway: &LlmGateway,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    state: &mut Checkpoint,
    sink: &mut dyn EventSink,
    view: WindowView<'_>,
    cs: &mut CompactionState,
) -> Result<bool, AgentError> {
    cs.overflow_retried = true;
    let (limits, gauge) = (view.limits, view.gauge);
    let scale = gauge.ratio(&state.messages, cs.system_est + view.tool_est);
    let effective = limits
        .window
        .map_or_else(|| gauge.tokens(&state.messages) * 3 / 4, |w| w.effective);
    let compacted = opts.compaction.enabled
        && try_compact(
            gateway,
            run,
            opts,
            state,
            sink,
            // 窓あふれの直後は、逐語で残す直近を小さくする（有効窓の 10%）。
            crate::compaction::CompactBudget {
                effective_window: effective,
                keep_permille: 100,
                scale,
            },
        )
        .await?;
    if compacted {
        cs.last_step = Some(state.spent.steps);
    }
    let folded = crate::context::emergency_fold(&mut state.messages);
    Ok(compacted || folded)
}

/// プロバイダの「窓を超えた」拒否か（vLLM・OpenAI・Anthropic・Gemini の文言）。
pub(crate) fn is_context_overflow(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "maximum context length",
        "context_length_exceeded",
        "context length",
        "context window",
        "prompt is too long",
        "input is too long",
        "too many tokens",
        "exceeds the context",
        "input token count",
    ]
    .iter()
    .any(|p| m.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_provider_overflow_messages() {
        assert!(is_context_overflow(
            "openai 400 Bad Request: This model's maximum context length is 32768 tokens. However, you requested 33000 tokens"
        ));
        assert!(is_context_overflow(
            "anthropic 400: prompt is too long: 210000 tokens > 200000 maximum"
        ));
        assert!(is_context_overflow("code: context_length_exceeded"));
        assert!(!is_context_overflow("openai 401 Unauthorized"));
        assert!(!is_context_overflow("rate limited"));
    }
}
