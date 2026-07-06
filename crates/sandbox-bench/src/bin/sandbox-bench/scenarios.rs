//! 1 回のフルライフサイクル計測（create→exec→python→IO→destroy）。

use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use sandbox_client::{ExecEvent, ExecRequest, SandboxSpec};
use sandbox_orchestrator::backend::{Backend, Instance};

use crate::rss;

/// 1 反復分の計測値（ミリ秒・RSS は KiB）。
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(crate) struct Metrics {
    pub create_ready_ms: f64,
    /// 自明な exec 往復（Python `print(1)`・全ティア共通で shell コマンド suite に依存しない）。
    pub exec_ms: f64,
    pub python_ms: f64,
    pub python_ok: bool,
    pub io_ms: f64,
    pub destroy_ms: f64,
    pub peak_rss_kb: u64,
}

/// バックエンドを 1 回まわして計測する。
pub(crate) async fn run_once(
    backend: &Arc<dyn Backend>,
    spec: SandboxSpec,
) -> Result<Metrics, sandbox_client::SandboxError> {
    let self_pid = std::process::id();

    let t = Instant::now();
    let inst = backend.create(spec).await?;
    let create_ready_ms = ms(t);

    // create 直後の子孫 RSS を採る（sidecar/runsc/fc のメモリ）。
    let peak_rss_kb = rss::descendant_rss_kb(self_pid);

    // 自明な exec 往復（Python・shell コマンド suite に依存せず全ティアで比較可能）。
    let t = Instant::now();
    drain(
        &inst,
        ExecRequest::Python {
            code: "print(1)".into(),
            timeout_ms: None,
        },
    )
    .await?;
    let exec_ms = ms(t);

    // 純 Python の CPU 計測（全ティア共通・numpy 依存を避ける）。
    let t = Instant::now();
    let (out, _) = collect(
        &inst,
        ExecRequest::Python {
            code: "print(sum(i*i for i in range(300000)))".into(),
            timeout_ms: None,
        },
    )
    .await?;
    let python_ms = ms(t);
    let python_ok = out.contains("8999955000") || out.trim().parse::<u64>().is_ok();

    // 1 MiB の put/get 往復。
    let blob = vec![0x5au8; 1024 * 1024];
    let t = Instant::now();
    inst.put_file("/workspace/blob.bin", blob.clone()).await?;
    let got = inst.get_file("/workspace/blob.bin").await?;
    let io_ms = ms(t);
    debug_assert_eq!(got.len(), blob.len());

    let t = Instant::now();
    inst.destroy().await?;
    let destroy_ms = ms(t);

    Ok(Metrics {
        create_ready_ms,
        exec_ms,
        python_ms,
        python_ok,
        io_ms,
        destroy_ms,
        peak_rss_kb,
    })
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

async fn drain(
    inst: &Arc<dyn Instance>,
    req: ExecRequest,
) -> Result<(), sandbox_client::SandboxError> {
    let mut s = inst.exec(req).await?;
    while let Some(item) = s.next().await {
        item?;
    }
    Ok(())
}

async fn collect(
    inst: &Arc<dyn Instance>,
    req: ExecRequest,
) -> Result<(String, Option<i32>), sandbox_client::SandboxError> {
    let mut s = inst.exec(req).await?;
    let mut out = String::new();
    let mut code = None;
    while let Some(item) = s.next().await {
        match item? {
            ExecEvent::Stdout(b) => out.push_str(&String::from_utf8_lossy(&b)),
            ExecEvent::Exited { code: c } => code = Some(c),
            _ => {}
        }
    }
    Ok((out, code))
}
