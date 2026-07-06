//! Firecracker バックエンドの gated 結合テスト（`SANDBOX_FC_IT=1`＋`FC_BIN`＋`FC_KERNEL`＋`FC_ROOTFS`）。
//!
//! **実 KVM（/dev/kvm）が要る**。本開発ホスト（非特権 LXC・KVM 無し）では skip。KVM ホストで
//! `firecracker`＋vsock 対応 vmlinux＋agent 入り rootfs.ext4 を渡して回す。
//! create→exec(Python/シェル)→ファイル→破棄、2 VM 分離を検証する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use sandbox_client::{ExecEvent, ExecRequest, SandboxBackend, SandboxSpec};
use sandbox_orchestrator::backend::firecracker::FirecrackerBackend;
use sandbox_orchestrator::backend::{Backend, Instance};

struct Env {
    bin: String,
    kernel: PathBuf,
    rootfs: PathBuf,
    state: PathBuf,
}

fn gated() -> Option<Env> {
    if std::env::var("SANDBOX_FC_IT").as_deref() != Ok("1") {
        eprintln!("skip: set SANDBOX_FC_IT=1 (needs /dev/kvm)");
        return None;
    }
    let (Ok(bin), Ok(kernel), Ok(rootfs)) = (
        std::env::var("FC_BIN"),
        std::env::var("FC_KERNEL"),
        std::env::var("FC_ROOTFS"),
    ) else {
        eprintln!("skip: set FC_BIN, FC_KERNEL, FC_ROOTFS");
        return None;
    };
    Some(Env {
        bin,
        kernel: PathBuf::from(kernel),
        rootfs: PathBuf::from(rootfs),
        state: std::env::temp_dir().join(format!("fc-it-{}", std::process::id())),
    })
}

fn fc_spec() -> SandboxSpec {
    let mut s = SandboxSpec::code_interpreter("t".into(), "o".into(), "u:1".into());
    s.backend = SandboxBackend::Firecracker;
    s
}

async fn collect_stdout(inst: &Arc<dyn Instance>, req: ExecRequest) -> (String, Option<i32>) {
    let mut stream = inst.exec(req).await.expect("exec");
    let mut out = String::new();
    let mut code = None;
    while let Some(Ok(ev)) = stream.next().await {
        match ev {
            ExecEvent::Stdout(b) => out.push_str(&String::from_utf8_lossy(&b)),
            ExecEvent::Exited { code: c } => code = Some(c),
            _ => {}
        }
    }
    (out, code)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn firecracker_code_interpreter_and_files() {
    let Some(env) = gated() else { return };
    let backend = FirecrackerBackend::new(
        &env.bin,
        env.kernel.clone(),
        env.rootfs.clone(),
        env.state.clone(),
    )
    .expect("backend");

    let inst = backend.create(fc_spec()).await.expect("create");

    let (out, code) = collect_stdout(
        &inst,
        ExecRequest::Python {
            code: "print(6*7)".into(),
            timeout_ms: None,
        },
    )
    .await;
    assert!(out.contains("42"), "python stdout={out:?}");
    assert_eq!(code, Some(0));

    let (out, _) = collect_stdout(
        &inst,
        ExecRequest::Shell {
            cmd: "echo hello-fc".into(),
            timeout_ms: None,
        },
    )
    .await;
    assert!(out.contains("hello-fc"), "shell stdout={out:?}");

    // ファイル put/get/list（エージェント経由）。
    inst.put_file("/workspace/data.txt", b"payload".to_vec())
        .await
        .expect("put");
    assert_eq!(inst.get_file("data.txt").await.expect("get"), b"payload");
    let names: Vec<String> = inst
        .list_dir("/workspace")
        .await
        .expect("list")
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(names.contains(&"data.txt".to_string()));

    inst.destroy().await.expect("destroy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn firecracker_two_instances_isolated() {
    let Some(env) = gated() else { return };
    let backend = FirecrackerBackend::new(
        &env.bin,
        env.kernel.clone(),
        env.rootfs.clone(),
        env.state.clone(),
    )
    .expect("backend");
    let a = backend.create(fc_spec()).await.expect("a");
    let b = backend.create(fc_spec()).await.expect("b");
    assert_ne!(a.debug_id(), b.debug_id());
    a.destroy().await.expect("destroy a");
    let (out, _) = collect_stdout(
        &b,
        ExecRequest::Shell {
            cmd: "echo still-alive".into(),
            timeout_ms: None,
        },
    )
    .await;
    assert!(out.contains("still-alive"));
    b.destroy().await.expect("destroy b");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn firecracker_rejects_egress() {
    let Some(env) = gated() else { return };
    let backend = FirecrackerBackend::new(
        &env.bin,
        env.kernel.clone(),
        env.rootfs.clone(),
        env.state.clone(),
    )
    .expect("backend");
    let mut spec = fc_spec();
    spec.egress = sandbox_client::SandboxSpec::web_fetch(
        "t".into(),
        "o".into(),
        "u:1".into(),
        "example.com".into(),
        443,
    )
    .egress;
    // FC は egress 非対応（post-alpha）→ Unimplemented。
    assert!(matches!(
        backend.create(spec).await,
        Err(sandbox_client::SandboxError::Unimplemented(_))
    ));
}
