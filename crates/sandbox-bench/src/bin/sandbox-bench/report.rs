//! 計測値の集計（median/p95）と markdown/JSON 出力。

use crate::scenarios::Metrics;

/// 1 ティアの集計結果。
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct TierSummary {
    pub tier: String,
    pub iterations: usize,
    pub create_ready_ms_p50: f64,
    pub create_ready_ms_p95: f64,
    pub exec_ms_p50: f64,
    pub python_ms_p50: f64,
    pub python_ok: bool,
    pub io_ms_p50: f64,
    pub destroy_ms_p50: f64,
    pub peak_rss_mb_p50: f64,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn field(metrics: &[Metrics], f: impl Fn(&Metrics) -> f64, p: f64) -> f64 {
    let mut v: Vec<f64> = metrics.iter().map(&f).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    percentile(&v, p)
}

/// 反復計測をティア集計へ。
#[must_use]
pub(crate) fn summarize(tier: &str, metrics: &[Metrics]) -> TierSummary {
    TierSummary {
        tier: tier.to_string(),
        iterations: metrics.len(),
        create_ready_ms_p50: field(metrics, |m| m.create_ready_ms, 0.5),
        create_ready_ms_p95: field(metrics, |m| m.create_ready_ms, 0.95),
        exec_ms_p50: field(metrics, |m| m.exec_ms, 0.5),
        python_ms_p50: field(metrics, |m| m.python_ms, 0.5),
        python_ok: metrics.iter().all(|m| m.python_ok),
        io_ms_p50: field(metrics, |m| m.io_ms, 0.5),
        destroy_ms_p50: field(metrics, |m| m.destroy_ms, 0.5),
        peak_rss_mb_p50: field(metrics, |m| m.peak_rss_kb as f64 / 1024.0, 0.5),
    }
}

/// markdown 表を組む。
#[must_use]
pub(crate) fn markdown(summaries: &[TierSummary], skipped: &[(String, String)]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    s.push_str("| ティア | N | create→ready p50 / p95 (ms) | exec p50 (ms) | python p50 (ms) | put/get 1MiB p50 (ms) | destroy p50 (ms) | RSS p50 (MB) |\n");
    s.push_str("|---|---|---|---|---|---|---|---|\n");
    for t in summaries {
        let _ = writeln!(
            s,
            "| {} | {} | {:.1} / {:.1} | {:.1} | {:.1}{} | {:.1} | {:.1} | {:.0} |",
            t.tier,
            t.iterations,
            t.create_ready_ms_p50,
            t.create_ready_ms_p95,
            t.exec_ms_p50,
            t.python_ms_p50,
            if t.python_ok { "" } else { " ⚠" },
            t.io_ms_p50,
            t.destroy_ms_p50,
            t.peak_rss_mb_p50,
        );
    }
    if !skipped.is_empty() {
        s.push_str("\n**未計測ティア:**\n");
        for (tier, why) in skipped {
            let _ = writeln!(s, "- `{tier}`: {why}");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(create: f64) -> Metrics {
        Metrics {
            create_ready_ms: create,
            exec_ms: 1.0,
            python_ms: 2.0,
            python_ok: true,
            io_ms: 3.0,
            destroy_ms: 4.0,
            peak_rss_kb: 2048,
        }
    }

    #[test]
    fn percentiles_and_summary() {
        let metrics: Vec<Metrics> = (1..=10).map(|i| m(f64::from(i))).collect();
        let s = summarize("wasm", &metrics);
        assert_eq!(s.iterations, 10);
        assert!((s.create_ready_ms_p50 - 6.0).abs() < 2.0);
        assert!(s.create_ready_ms_p95 >= s.create_ready_ms_p50);
        assert!((s.peak_rss_mb_p50 - 2.0).abs() < 0.01);
    }

    #[test]
    fn markdown_lists_skipped() {
        let s = summarize("gvisor", &[m(5.0)]);
        let md = markdown(&[s], &[("firecracker".into(), "no /dev/kvm".into())]);
        assert!(md.contains("gvisor"));
        assert!(md.contains("firecracker") && md.contains("no /dev/kvm"));
    }
}
