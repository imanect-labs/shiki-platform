//! gVisor バックエンドの gated 結合テスト（`SANDBOX_GVISOR_IT=1`＋`RUNSC_BIN`＋`GVISOR_ROOTFS`）。
//!
//! 実 runsc（rootless・systrap）で create→exec(Python/シェル)→ファイル→破棄を検証する。egress は
//! 追加で `NETNS_HOLDER_BIN` が要る。CI（runsc 無し）では skip。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::io::{Read, Write};
use std::net::TcpListener as StdTcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use sandbox_client::{Egress, EgressRule, ExecEvent, ExecRequest, SandboxBackend, SandboxSpec};
use sandbox_orchestrator::backend::gvisor::GvisorBackend;
use sandbox_orchestrator::backend::{Backend, Instance};

struct Env {
    runsc: String,
    rootfs: PathBuf,
    state: PathBuf,
    holder: Option<PathBuf>,
}

fn gated() -> Option<Env> {
    if std::env::var("SANDBOX_GVISOR_IT").as_deref() != Ok("1") {
        eprintln!("skip: set SANDBOX_GVISOR_IT=1");
        return None;
    }
    let (Ok(runsc), Ok(rootfs)) = (std::env::var("RUNSC_BIN"), std::env::var("GVISOR_ROOTFS"))
    else {
        eprintln!("skip: set RUNSC_BIN and GVISOR_ROOTFS");
        return None;
    };
    Some(Env {
        runsc,
        rootfs: PathBuf::from(rootfs),
        state: std::env::temp_dir().join(format!("gvisor-it-{}", std::process::id())),
        holder: std::env::var("NETNS_HOLDER_BIN").ok().map(PathBuf::from),
    })
}

fn gvisor_spec() -> SandboxSpec {
    SandboxSpec::code_interpreter(SandboxBackend::Gvisor, "t".into(), "o".into(), "u:1".into())
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
async fn gvisor_code_interpreter_and_files() {
    let Some(env) = gated() else { return };
    let backend = GvisorBackend::new(&env.runsc, env.rootfs.clone(), env.state.clone(), None)
        .expect("backend");

    let inst = backend.create(gvisor_spec()).await.expect("create");

    // Python 実行。
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

    // シェルコマンド。
    let (out, _) = collect_stdout(
        &inst,
        ExecRequest::Shell {
            cmd: "echo hello-gvisor".into(),
            timeout_ms: None,
        },
    )
    .await;
    assert!(out.contains("hello-gvisor"), "shell stdout={out:?}");

    // ファイル put/get/list。
    inst.put_file("/workspace/data.txt", b"payload".to_vec())
        .await
        .expect("put");
    let got = inst.get_file("data.txt").await.expect("get");
    assert_eq!(got, b"payload");
    let names: Vec<String> = inst
        .list_dir("/workspace")
        .await
        .expect("list")
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(names.contains(&"data.txt".to_string()));

    // guest が /workspace に書いた成果物をホスト側で回収できる。
    let (_, _) = collect_stdout(
        &inst,
        ExecRequest::Shell {
            cmd: "sh -c 'echo generated > out.txt'".into(),
            timeout_ms: None,
        },
    )
    .await;
    let out_txt = inst.get_file("out.txt").await.expect("artifact");
    assert_eq!(String::from_utf8_lossy(&out_txt).trim(), "generated");

    inst.destroy().await.expect("destroy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gvisor_two_instances_isolated() {
    let Some(env) = gated() else { return };
    let backend = GvisorBackend::new(&env.runsc, env.rootfs.clone(), env.state.clone(), None)
        .expect("backend");
    let a = backend.create(gvisor_spec()).await.expect("a");
    let b = backend.create(gvisor_spec()).await.expect("b");
    assert_ne!(a.debug_id(), b.debug_id());
    // 一方を壊しても他方は動く。
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
async fn gvisor_egress_allows_listed_blocks_others() {
    let Some(env) = gated() else { return };
    let Some(holder) = env.holder.clone() else {
        eprintln!("skip egress: set NETNS_HOLDER_BIN");
        return;
    };
    std::env::set_var("SANDBOX_EGRESS_ALLOW_PRIVATE", "1");
    // 上流バナーサーバ（ホスト・ループバック）。
    let up = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let up_port = up.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in up.incoming() {
            let Ok(mut s) = s else { break };
            std::thread::spawn(move || {
                let _ = s.write_all(b"UPSTREAM_OK");
                let mut b = [0u8; 64];
                let _ = s.read(&mut b);
            });
        }
    });

    let backend = GvisorBackend::new(
        &env.runsc,
        env.rootfs.clone(),
        env.state.clone(),
        Some(holder),
    )
    .expect("backend");

    let mut spec = gvisor_spec();
    spec.egress = Egress {
        static_allow: vec![EgressRule {
            host_pattern: "127.0.0.1".into(),
            port: up_port,
        }],
        ..Egress::blocked()
    };
    let inst = backend.create(spec).await.expect("create egress");

    // guest から gateway:up_port へ SNI=127.0.0.1 の ClientHello → 上流バナー到達。
    let allow_py = format!(
        "import socket,struct\nh=b'127.0.0.1'\nsn=b'\\x00'+struct.pack('>H',len(h))+h\nsnl=struct.pack('>H',len(sn))+sn\next=b'\\x00\\x00'+struct.pack('>H',len(snl))+snl\nexts=struct.pack('>H',len(ext))+ext\nbody=b'\\x03\\x03'+b'\\x00'*32+b'\\x00'+struct.pack('>H',2)+b'\\x13\\x01'+b'\\x01'+b'\\x00'+exts\nhs=b'\\x01'+struct.pack('>I',len(body))[1:]+body\nch=b'\\x16\\x03\\x01'+struct.pack('>H',len(hs))+hs\ns=socket.create_connection(('169.254.0.1',{up_port}),3)\ns.sendall(ch)\ns.settimeout(3)\nprint(s.recv(32).decode(errors='replace'))"
    );
    let (out, _) = collect_stdout(
        &inst,
        ExecRequest::Python {
            code: allow_py,
            timeout_ms: None,
        },
    )
    .await;
    assert!(out.contains("UPSTREAM_OK"), "egress allow out={out:?}");

    // 443 に SNI=evil.test → 拒否（切断）。
    let deny_py = "import socket,struct\nh=b'evil.test'\nsn=b'\\x00'+struct.pack('>H',len(h))+h\nsnl=struct.pack('>H',len(sn))+sn\next=b'\\x00\\x00'+struct.pack('>H',len(snl))+snl\nexts=struct.pack('>H',len(ext))+ext\nbody=b'\\x03\\x03'+b'\\x00'*32+b'\\x00'+struct.pack('>H',2)+b'\\x13\\x01'+b'\\x01'+b'\\x00'+exts\nhs=b'\\x01'+struct.pack('>I',len(body))[1:]+body\nch=b'\\x16\\x03\\x01'+struct.pack('>H',len(hs))+hs\ntry:\n s=socket.create_connection(('169.254.0.1',443),3)\n s.sendall(ch)\n s.settimeout(3)\n print('GOT',s.recv(32).decode(errors='replace'))\nexcept Exception as e:\n print('BLOCKED')".to_string();
    let (out, _) = collect_stdout(
        &inst,
        ExecRequest::Python {
            code: deny_py,
            timeout_ms: None,
        },
    )
    .await;
    assert!(
        !out.contains("UPSTREAM_OK"),
        "egress deny must block, out={out:?}"
    );

    inst.destroy().await.expect("destroy");
}
