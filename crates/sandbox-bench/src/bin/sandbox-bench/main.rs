//! サンドボックス 3 ティア横断ベンチ（gated: `SANDBOX_BENCH=1`）。
//!
//! 利用可能なティア（env でランタイム/アセットが揃うもの）を Backend トレイトで in-process 構築し、
//! フルライフサイクル（create→echo→python→IO→destroy）を N 反復して median/p95 を出す。
//! 出力: markdown（stdout）＋JSON（target/bench/results.json）。
//!
//! CLI ベンチのため結果の print と統計の浮動小数キャストを許可する（コア品質床は維持）。
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::similar_names
)]

mod report;
mod rss;
mod scenarios;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sandbox_client::{SandboxBackend, SandboxSpec};
use sandbox_orchestrator::backend::firecracker::FirecrackerBackend;
use sandbox_orchestrator::backend::gvisor::GvisorBackend;
use sandbox_orchestrator::backend::wasm::WasmBackend;
use sandbox_orchestrator::backend::Backend;
use sandbox_orchestrator::config::OrchestratorEnv;

use report::{markdown, summarize, TierSummary};

/// 発見したティア（名前・バックエンド・spec ファクトリ）。
struct Tier {
    name: &'static str,
    backend: Arc<dyn Backend>,
    spec: fn() -> SandboxSpec,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn spec_for(backend: SandboxBackend) -> SandboxSpec {
    let mut s = SandboxSpec::code_interpreter("bench".into(), "org".into(), "u:bench".into());
    s.backend = backend;
    s
}

fn wasm_spec() -> SandboxSpec {
    spec_for(SandboxBackend::Wasm)
}
fn gvisor_spec() -> SandboxSpec {
    spec_for(SandboxBackend::Gvisor)
}
fn firecracker_spec() -> SandboxSpec {
    spec_for(SandboxBackend::Firecracker)
}

/// 利用可能なティアを env から発見する。未計測は理由付きで返す。
fn discover() -> (Vec<Tier>, Vec<(String, String)>) {
    let mut tiers = Vec::new();
    let mut skipped = Vec::new();

    // wasm: sidecar バイナリが要る。
    match env("SECURE_EXEC_SIDECAR_BIN") {
        Some(bin) if Path::new(&bin).is_file() => {
            let b = WasmBackend::new(Some(bin), OrchestratorEnv::default());
            tiers.push(Tier {
                name: "wasm",
                backend: Arc::new(b),
                spec: wasm_spec,
            });
        }
        _ => skipped.push(("wasm".into(), "SECURE_EXEC_SIDECAR_BIN 未設定/不在".into())),
    }

    // gvisor: runsc + rootfs。
    match (env("RUNSC_BIN"), env("GVISOR_ROOTFS")) {
        (Some(runsc), Some(rootfs)) => {
            match GvisorBackend::new(
                &runsc,
                PathBuf::from(rootfs),
                std::env::temp_dir().join("bench-gvisor"),
                env("NETNS_HOLDER_BIN").map(PathBuf::from),
            ) {
                Ok(b) => tiers.push(Tier {
                    name: "gvisor",
                    backend: Arc::new(b),
                    spec: gvisor_spec,
                }),
                Err(e) => skipped.push(("gvisor".into(), format!("{e}"))),
            }
        }
        _ => skipped.push(("gvisor".into(), "RUNSC_BIN/GVISOR_ROOTFS 未設定".into())),
    }

    // firecracker: bin+kernel+rootfs+/dev/kvm。
    let kvm = Path::new("/dev/kvm").exists();
    match (env("FC_BIN"), env("FC_KERNEL"), env("FC_ROOTFS")) {
        _ if !kvm => skipped.push((
            "firecracker".into(),
            "/dev/kvm 無し（KVM ホストが要る）".into(),
        )),
        (Some(bin), Some(kernel), Some(rootfs)) => {
            match FirecrackerBackend::new(
                &bin,
                PathBuf::from(kernel),
                PathBuf::from(rootfs),
                std::env::temp_dir().join("bench-fc"),
            ) {
                Ok(b) => tiers.push(Tier {
                    name: "firecracker",
                    backend: Arc::new(b),
                    spec: firecracker_spec,
                }),
                Err(e) => skipped.push(("firecracker".into(), format!("{e}"))),
            }
        }
        _ => skipped.push((
            "firecracker".into(),
            "FC_BIN/FC_KERNEL/FC_ROOTFS 未設定".into(),
        )),
    }

    (tiers, skipped)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if std::env::var("SANDBOX_BENCH").as_deref() != Ok("1") {
        eprintln!("skip: set SANDBOX_BENCH=1 to run the sandbox tier benchmark");
        return;
    }
    let iters: usize = env("BENCH_ITERS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
        .max(1);

    let (tiers, skipped) = discover();
    if tiers.is_empty() {
        eprintln!("no tiers available; skipped: {skipped:?}");
        return;
    }

    let mut summaries: Vec<TierSummary> = Vec::new();
    for tier in &tiers {
        eprintln!("→ benchmarking {} ({iters} iters + 1 warmup)", tier.name);
        // ウォームアップ 1 回（測定に含めない）。
        if let Err(e) = scenarios::run_once(&tier.backend, (tier.spec)()).await {
            eprintln!("  {} warmup failed: {e}; skipping", tier.name);
            continue;
        }
        let mut metrics = Vec::new();
        for i in 0..iters {
            match scenarios::run_once(&tier.backend, (tier.spec)()).await {
                Ok(m) => metrics.push(m),
                Err(e) => eprintln!("  {} iter {i} failed: {e}", tier.name),
            }
        }
        if !metrics.is_empty() {
            summaries.push(summarize(tier.name, &metrics));
        }
    }

    let md = markdown(&summaries, &skipped);
    println!("\n{md}");

    // JSON 成果物。
    let out_dir = PathBuf::from("target/bench");
    let _ = std::fs::create_dir_all(&out_dir);
    if let Ok(json) = serde_json::to_string_pretty(&summaries) {
        let _ = std::fs::write(out_dir.join("results.json"), json);
        eprintln!("→ wrote target/bench/results.json");
    }
}
