//! 圧縮（[`crate::compaction`]）をループから実行する: LLM 呼び出し・会計・イベント・失敗時の後退。

use llm_gateway::{GenerationRecord, LlmGateway};

use crate::agent::RunContext;
use crate::checkpoint::Checkpoint;
use crate::event::{AgentError, AgentEvent, EventSink, RecoveryAction};
use crate::profile::AgentOptions;

/// 圧縮を試みる。置き換えたら `true`。要約の失敗（LLM エラー・空・縮まない）はループを止めず
/// `false` を返す（呼び出し側は剪定へ後退する）。
///
/// `forced` は窓あふれで拒否された直後の呼び出し（逐語で残す直近を小さくする）。
pub(crate) async fn try_compact(
    gateway: &LlmGateway,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    state: &mut Checkpoint,
    sink: &mut dyn EventSink,
    effective_window: usize,
    forced: bool,
) -> Result<bool, AgentError> {
    if sink.is_cancelled() {
        return Ok(false);
    }
    let keep = if forced {
        100
    } else {
        opts.compaction.keep_recent_permille
    };
    let started_at = chrono::Utc::now();
    let outcome = crate::compaction::compact(
        gateway,
        run.ctx,
        opts,
        &state.messages,
        &state.plan,
        effective_window,
        keep,
    )
    .await;
    let compacted = match outcome {
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

/// ループをまたぐ圧縮の状態。
#[derive(Default)]
pub(crate) struct CompactionState {
    /// 直近に圧縮したステップ（直後の 2 ステップは繰り返さない）。
    last_step: Option<usize>,
    /// 直近の圧縮が失敗した（soft の剪定を後ろ盾として戻す）。
    failing: bool,
}

/// ステップの前の文脈管理（自律版）: 窓を知っていて見積りが soft を超えていれば要約で置き換え、
/// 残りを剪定する。
///
/// 圧縮を使う run では、soft の畳み込みは圧縮が受け持つ（非常閾値だけ畳む）。直近の圧縮が
/// 失敗していれば、従来どおり soft でも畳む。圧縮の直後 2 ステップは繰り返さない（尻尾そのものが
/// 大きいときに毎ステップ要約しない。そのときは剪定が受け持つ）。
#[allow(clippy::too_many_arguments)] // ループの状態をそのまま受け取る（束ねると呼び出し側が読みにくい）。
pub(crate) async fn before_step(
    gateway: &LlmGateway,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    state: &mut Checkpoint,
    sink: &mut dyn EventSink,
    limits: &crate::context::ContextLimits,
    gauge: &crate::context::ContextGauge,
    cs: &mut CompactionState,
) -> Result<(), AgentError> {
    if let Some(w) = limits.window.filter(|_| opts.compaction.enabled) {
        let cooled = cs.last_step.is_none_or(|s| state.spent.steps >= s + 2);
        if cooled && gauge.tokens(&state.messages) > w.soft {
            let steps = state.spent.steps;
            let ok = try_compact(gateway, run, opts, state, sink, w.effective, false).await?;
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
///
/// 拒否は応答の前に起きるので、履歴にはそのステップの痕跡が無い（やり直して安全）。窓を
/// 知らないモデルでは、今の見積りの 3/4 を窓とみなして要約する。
#[allow(clippy::too_many_arguments)] // before_step と同じ理由。
pub(crate) async fn recover_overflow(
    gateway: &LlmGateway,
    run: &RunContext<'_>,
    opts: &AgentOptions,
    state: &mut Checkpoint,
    sink: &mut dyn EventSink,
    limits: &crate::context::ContextLimits,
    gauge: &crate::context::ContextGauge,
    cs: &mut CompactionState,
) -> Result<bool, AgentError> {
    let effective = limits
        .window
        .map_or_else(|| gauge.tokens(&state.messages) * 3 / 4, |w| w.effective);
    let compacted = opts.compaction.enabled
        && try_compact(gateway, run, opts, state, sink, effective, true).await?;
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
