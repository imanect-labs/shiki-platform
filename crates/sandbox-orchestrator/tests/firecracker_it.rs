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
    SandboxSpec::code_interpreter(
        SandboxBackend::Firecracker,
        "t".into(),
        "o".into(),
        "u:1".into(),
    )
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

/// #504: シェル行はゲストの `/bin/sh -c` が解釈する（パイプ・`&&`・`||`・リダイレクト）。
/// リダイレクト先は /workspace に残り、ホスト側で回収できる（`shell` ツールの sync-back の前提）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn firecracker_shell_line_is_interpreted_by_sh() {
    let Some(env) = gated() else { return };
    let backend = FirecrackerBackend::new(
        &env.bin,
        env.kernel.clone(),
        env.rootfs.clone(),
        env.state.clone(),
    )
    .expect("backend");
    let inst = backend.create(fc_spec()).await.expect("create");
    inst.put_file("/workspace/rows.csv", b"a,x\nb,y\nc,x\n".to_vec())
        .await
        .expect("put");

    let shell = |cmd: &str| ExecRequest::Shell {
        cmd: cmd.into(),
        timeout_ms: None,
    };
    let (out, code) = collect_stdout(
        &inst,
        shell("cut -d, -f2 rows.csv | sort | uniq -c > counts.txt && echo piped-ok"),
    )
    .await;
    assert_eq!(code, Some(0), "stdout={out:?}");
    assert!(out.contains("piped-ok"), "stdout={out:?}");
    let counts = inst.get_file("counts.txt").await.expect("redirect target");
    let counts = String::from_utf8_lossy(&counts);
    assert!(
        counts.contains("2 x") && counts.contains("1 y"),
        "{counts:?}"
    );

    // `&&` は左が失敗すれば右を実行しない・`||` は実行する（シェルの意味論そのもの）。
    let (out, code) = collect_stdout(&inst, shell("false && echo never || echo fallback")).await;
    assert_eq!(code, Some(0), "stdout={out:?}");
    assert!(
        !out.contains("never") && out.contains("fallback"),
        "{out:?}"
    );

    // バックグラウンドジョブがパイプを握っていても、シェルの終了で返る（壁時計上限まで待たない）。
    let started = std::time::Instant::now();
    let (out, code) = collect_stdout(&inst, shell("sleep 60 & echo started")).await;
    assert_eq!(code, Some(0), "stdout={out:?}");
    assert!(out.contains("started"), "{out:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );

    // 返った後にジョブが /workspace を書き換えない（書き戻し中の取りこぼし防止・ジョブは止まっている）。
    let (_, code) = collect_stdout(
        &inst,
        shell("(sleep 1; echo late > late.txt) & echo started"),
    )
    .await;
    assert_eq!(code, Some(0));
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        inst.get_file("late.txt").await.is_err(),
        "返った後もジョブが書き込んだ"
    );

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
    // 動的許可 1 件だけの egress（default-deny＋当該ホストのみ）。
    spec.egress = sandbox_client::Egress {
        static_allow: Vec::new(),
        dynamic_allow: vec![sandbox_client::EgressRule {
            host_pattern: "example.com".into(),
            port: 443,
        }],
        deny_overlay: Vec::new(),
        secret_attach: false,
    };
    // FC は egress 非対応（post-alpha）→ Unimplemented。
    assert!(matches!(
        backend.create(spec).await,
        Err(sandbox_client::SandboxError::Unimplemented(_))
    ));
}
