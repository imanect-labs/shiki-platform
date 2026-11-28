//! サンプラ: 記録するかどうかを **api 側の方針だけ**で決める。
//!
//! 受信した `traceparent` の sampled フラグは外部（ブラウザなど）が自由に書けるため、
//! SDK 既定の `ParentBased` のように従うと、上流が記録を止めたり（flags=00 で 5xx の
//! span まで消える）、比率サンプリングを越えて記録を強制したり（flags=01）できる。
//! そこで、親が**このプロセス内の span** なら従い（1 本のトレースの中で判断を揃える）、
//! 親が**外部（remote）または無い**ときはフラグを見ずに root サンプラで決める。
//!
//! root サンプラは OTel 標準の環境変数 `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG`
//! から作る（未設定なら全量 = 変更前と同じ）。

use opentelemetry::{
    trace::{
        Link, SamplingDecision, SamplingResult, SpanKind, TraceContextExt, TraceId, TraceState,
    },
    Context, KeyValue,
};
use opentelemetry_sdk::trace::{Sampler, ShouldSample};

/// 外部の親のフラグを無視し、プロセス内の親にだけ従うサンプラ。
#[derive(Debug, Clone)]
pub(crate) struct LocalDecisionSampler {
    root: Sampler,
}

impl LocalDecisionSampler {
    pub(crate) fn new(root: Sampler) -> Self {
        Self { root }
    }

    /// `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG` から root を作る。
    pub(crate) fn from_env() -> Self {
        Self::new(root_sampler(
            std::env::var("OTEL_TRACES_SAMPLER").ok().as_deref(),
            std::env::var("OTEL_TRACES_SAMPLER_ARG").ok().as_deref(),
        ))
    }
}

/// 環境変数の値から root サンプラを作る。`parentbased_*` も親の扱いは本サンプラが担うので
/// root 部分だけを使う。不明な値・不正な比率は全量（変更前と同じ）に倒す。
pub(crate) fn root_sampler(kind: Option<&str>, arg: Option<&str>) -> Sampler {
    let kind = kind.map(str::trim).unwrap_or_default();
    match kind.strip_prefix("parentbased_").unwrap_or(kind) {
        "always_off" => Sampler::AlwaysOff,
        "traceidratio" => {
            let ratio = arg
                .and_then(|a| a.trim().parse::<f64>().ok())
                .filter(|r| (0.0..=1.0).contains(r))
                .unwrap_or(1.0);
            Sampler::TraceIdRatioBased(ratio)
        }
        _ => Sampler::AlwaysOn,
    }
}

impl ShouldSample for LocalDecisionSampler {
    fn should_sample(
        &self,
        parent_context: Option<&Context>,
        trace_id: TraceId,
        name: &str,
        span_kind: &SpanKind,
        attributes: &[KeyValue],
        links: &[Link],
    ) -> SamplingResult {
        if let Some(cx) = parent_context.filter(|cx| cx.has_active_span()) {
            let parent = cx.span().span_context().clone();
            if parent.is_valid() && !parent.is_remote() {
                return SamplingResult {
                    decision: if parent.is_sampled() {
                        SamplingDecision::RecordAndSample
                    } else {
                        SamplingDecision::Drop
                    },
                    attributes: Vec::new(),
                    trace_state: parent.trace_state().clone(),
                };
            }
        }
        let mut result =
            self.root
                .should_sample(parent_context, trace_id, name, span_kind, attributes, links);
        if result.trace_state == TraceState::default() {
            if let Some(cx) = parent_context {
                result.trace_state = cx.span().span_context().trace_state().clone();
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::{SpanContext, SpanId, TraceFlags};

    use super::*;

    fn decide(sampler: &LocalDecisionSampler, parent: Option<SpanContext>) -> SamplingDecision {
        let cx = parent.map(|sc| Context::new().with_remote_span_context(sc));
        sampler
            .should_sample(
                cx.as_ref(),
                TraceId::from(1u128),
                "x",
                &SpanKind::Server,
                &[],
                &[],
            )
            .decision
    }

    fn remote(sampled: bool) -> SpanContext {
        let flags = if sampled {
            TraceFlags::SAMPLED
        } else {
            TraceFlags::default()
        };
        SpanContext::new(
            TraceId::from(1u128),
            SpanId::from(2u64),
            flags,
            true,
            TraceState::default(),
        )
    }

    fn local(sampled: bool) -> SpanContext {
        let sc = remote(sampled);
        SpanContext::new(
            sc.trace_id(),
            sc.span_id(),
            sc.trace_flags(),
            false,
            TraceState::default(),
        )
    }

    #[test]
    fn remote_parent_flags_are_ignored() {
        let on = LocalDecisionSampler::new(Sampler::AlwaysOn);
        let off = LocalDecisionSampler::new(Sampler::AlwaysOff);
        // 外部が「記録しない」と言っても記録し、「記録する」と言っても root に従う。
        assert_eq!(
            decide(&on, Some(remote(false))),
            SamplingDecision::RecordAndSample
        );
        assert_eq!(decide(&off, Some(remote(true))), SamplingDecision::Drop);
        assert_eq!(decide(&on, None), SamplingDecision::RecordAndSample);
        assert_eq!(decide(&off, None), SamplingDecision::Drop);
    }

    #[test]
    fn local_parent_decision_is_followed() {
        let on = LocalDecisionSampler::new(Sampler::AlwaysOn);
        let off = LocalDecisionSampler::new(Sampler::AlwaysOff);
        assert_eq!(decide(&on, Some(local(false))), SamplingDecision::Drop);
        assert_eq!(
            decide(&off, Some(local(true))),
            SamplingDecision::RecordAndSample
        );
    }

    #[test]
    fn root_sampler_reads_standard_env_values() {
        assert!(matches!(root_sampler(None, None), Sampler::AlwaysOn));
        assert!(matches!(
            root_sampler(Some("always_off"), None),
            Sampler::AlwaysOff
        ));
        assert!(matches!(
            root_sampler(Some("parentbased_always_off"), None),
            Sampler::AlwaysOff
        ));
        assert!(matches!(
            root_sampler(Some("parentbased_traceidratio"), Some("0.25")),
            Sampler::TraceIdRatioBased(r) if (r - 0.25).abs() < f64::EPSILON
        ));
        // 不正な比率・不明な種類は全量に倒す。
        assert!(matches!(
            root_sampler(Some("traceidratio"), Some("2")),
            Sampler::TraceIdRatioBased(r) if (r - 1.0).abs() < f64::EPSILON
        ));
        assert!(matches!(
            root_sampler(Some("xray"), None),
            Sampler::AlwaysOn
        ));
    }
}
