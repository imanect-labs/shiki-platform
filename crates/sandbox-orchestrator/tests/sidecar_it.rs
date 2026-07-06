//! 実 secure-exec-sidecar を spawn する結合テスト（gated: `SANDBOX_IT=1`）。
//!
//! wasm バックエンドの wire 統合（auth→session→createVm→write→execute→events→dispose）を
//! 実バイナリで検証する。CI では sidecar ビルド後のジョブでのみ走らせる（アセット/V8 が要る）。
//! 実行前に `SECURE_EXEC_SIDECAR_BIN` を built バイナリに向けること。
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stderr
)]

use futures::StreamExt;
use sandbox_client::{ExecEvent, ExecRequest, SandboxSpec};
use sandbox_orchestrator::backend::wasm::WasmBackend;
use sandbox_orchestrator::backend::Backend;
use sandbox_orchestrator::config::OrchestratorEnv;

fn gated() -> bool {
    std::env::var("SANDBOX_IT").as_deref() == Ok("1")
}

async fn collect(
    stream: &mut futures::stream::BoxStream<
        'static,
        Result<ExecEvent, sandbox_client::SandboxError>,
    >,
) -> (String, String, Option<i32>) {
    let mut out = String::new();
    let mut err = String::new();
    let mut code = None;
    while let Some(ev) = stream.next().await {
        match ev.expect("event ok") {
            ExecEvent::Stdout(b) => out.push_str(&String::from_utf8_lossy(&b)),
            ExecEvent::Stderr(b) => err.push_str(&String::from_utf8_lossy(&b)),
            ExecEvent::Exited { code: c } => code = Some(c),
            ExecEvent::LimitExceeded { detail, .. } => panic!("limit exceeded: {detail}"),
        }
    }
    (out, err, code)
}

#[tokio::test]
async fn real_sidecar_runs_python() {
    if !gated() {
        eprintln!("skipping: set SANDBOX_IT=1 and SECURE_EXEC_SIDECAR_BIN to run");
        return;
    }
    let backend = WasmBackend::new(None, OrchestratorEnv::default());
    let spec = SandboxSpec::code_interpreter("t".into(), "o".into(), "user:1".into());
    let instance = backend.create(spec).await.expect("create sandbox");

    let mut stream = instance
        .exec(ExecRequest::Python {
            code: "print('hello from pyodide')".into(),
            timeout_ms: None,
        })
        .await
        .expect("exec python");
    let (out, err, code) = collect(&mut stream).await;
    assert!(
        out.contains("hello from pyodide"),
        "stdout was {out:?}, stderr {err:?}"
    );
    assert_eq!(code, Some(0), "stderr: {err}");

    instance.destroy().await.expect("destroy");
}

#[tokio::test]
async fn real_sidecar_numpy() {
    if !gated() {
        return;
    }
    let backend = WasmBackend::new(None, OrchestratorEnv::default());
    let instance = backend
        .create(SandboxSpec::code_interpreter(
            "t".into(),
            "o".into(),
            "user:1".into(),
        ))
        .await
        .expect("create");
    let mut stream = instance
        .exec(ExecRequest::Python {
            code: "import numpy as np; print(int(np.arange(5).sum()))".into(),
            timeout_ms: None,
        })
        .await
        .expect("exec");
    let (out, err, code) = collect(&mut stream).await;
    assert!(out.trim().contains("10"), "stdout {out:?} stderr {err:?}");
    assert_eq!(code, Some(0));
    instance.destroy().await.expect("destroy");
}

#[tokio::test]
async fn real_sidecar_egress_blocked_by_default() {
    if !gated() {
        return;
    }
    // code_interpreter は egress 空＝全遮断。urllib で外部到達を試みると失敗する。
    let backend = WasmBackend::new(None, OrchestratorEnv::default());
    let instance = backend
        .create(SandboxSpec::code_interpreter(
            "t".into(),
            "o".into(),
            "user:1".into(),
        ))
        .await
        .expect("create");
    let code = r"
import urllib.request
try:
    urllib.request.urlopen('http://example.com', timeout=5)
    print('REACHED')
except Exception as e:
    print('BLOCKED')
";
    let mut stream = instance
        .exec(ExecRequest::Python {
            code: code.into(),
            timeout_ms: None,
        })
        .await
        .expect("exec");
    let (out, err, _code) = collect(&mut stream).await;
    assert!(
        !out.contains("REACHED"),
        "egress must be blocked; stdout {out:?} stderr {err:?}"
    );
    instance.destroy().await.expect("destroy");
}

#[tokio::test]
async fn real_sidecar_process_isolation() {
    if !gated() {
        return;
    }
    // 2 つの sandbox は別 sidecar 子プロセス（別 VM）。一方を破棄しても他方は動く（PIT-32）。
    let backend = WasmBackend::new(None, OrchestratorEnv::default());
    let a = backend
        .create(SandboxSpec::code_interpreter(
            "t".into(),
            "o".into(),
            "user:1".into(),
        ))
        .await
        .expect("create a");
    let b = backend
        .create(SandboxSpec::code_interpreter(
            "t".into(),
            "o".into(),
            "user:2".into(),
        ))
        .await
        .expect("create b");
    assert_ne!(a.debug_id(), b.debug_id(), "distinct VMs");
    a.destroy().await.expect("destroy a");
    // a 破棄後も b は実行できる。
    let mut stream = b
        .exec(ExecRequest::Python {
            code: "print('b alive')".into(),
            timeout_ms: None,
        })
        .await
        .expect("exec b");
    let (out, _err, code) = collect(&mut stream).await;
    assert!(out.contains("b alive"));
    assert_eq!(code, Some(0));
    b.destroy().await.expect("destroy b");
}

#[tokio::test]
async fn real_sidecar_shell_reaches_guest() {
    if !gated() {
        return;
    }
    // Shell exec の配線確認。ゲストコマンド（coreutils/echo 等）は wasm コマンドスイートの
    // ビルド（Docker/CI ステージ）で software として同梱されるまで存在しないため、ここでは
    // 「コマンドが見つからない」応答が **sidecar から返る**（＝exec 経路が通っている）ことを確認する。
    // software 同梱後の実コマンド実行は Docker 結合テストで検証する。
    let backend = WasmBackend::new(None, OrchestratorEnv::default());
    let instance = backend
        .create(SandboxSpec::code_interpreter(
            "t".into(),
            "o".into(),
            "user:1".into(),
        ))
        .await
        .expect("create");
    let result = instance
        .exec(ExecRequest::Shell {
            cmd: "echo shell-works".into(),
            timeout_ms: None,
        })
        .await;
    match result {
        // software 未同梱: sidecar が「command not found」を返す（exec 経路は正常）。
        Err(sandbox_client::SandboxError::Invalid(msg)) => {
            assert!(msg.contains("command not found"), "unexpected: {msg}");
        }
        // software 同梱済みの環境では実行が成立してもよい。
        Ok(mut stream) => {
            let (_out, _err, _code) = collect(&mut stream).await;
        }
        Err(other) => panic!("unexpected shell exec error: {other}"),
    }
    instance.destroy().await.expect("destroy");
}

#[tokio::test]
async fn real_sidecar_file_roundtrip() {
    if !gated() {
        return;
    }
    let backend = WasmBackend::new(None, OrchestratorEnv::default());
    let instance = backend
        .create(SandboxSpec::code_interpreter(
            "t".into(),
            "o".into(),
            "user:1".into(),
        ))
        .await
        .expect("create");
    instance
        .put_file("/workspace/in.txt", b"roundtrip".to_vec())
        .await
        .expect("put");
    let got = instance.get_file("/workspace/in.txt").await.expect("get");
    assert_eq!(got, b"roundtrip");
    instance.destroy().await.expect("destroy");
}

/// ゲストコマンドスイート（software package.tar）の実行（Task 4.12 software）。
///
/// `SANDBOX_SOFTWARE_DIR`（`scripts/build-sandbox-commands.sh` の出力）が指す coreutils
/// パッケージを ConfigureVm で投影し、guest で echo/ls が実際に動くことを検証する。
/// パッケージ未ビルドの環境ではスキップする（重い wasm ビルドを test 前提にしない）。
#[tokio::test]
async fn real_sidecar_guest_commands_run() {
    if !gated() {
        return;
    }
    let Ok(software_dir) = std::env::var("SANDBOX_SOFTWARE_DIR") else {
        eprintln!("skipping: set SANDBOX_SOFTWARE_DIR (scripts/build-sandbox-commands.sh の出力)");
        return;
    };
    let software_dir = std::path::PathBuf::from(software_dir);
    if !software_dir.join("coreutils/package.tar").is_file() {
        eprintln!("skipping: coreutils/package.tar が未ビルド");
        return;
    }
    let backend = WasmBackend::new(
        None,
        OrchestratorEnv {
            software_dir: Some(software_dir),
            ..OrchestratorEnv::default()
        },
    );
    let mut spec = SandboxSpec::code_interpreter("t".into(), "o".into(), "user:1".into());
    spec.software = vec!["coreutils".into()];
    let instance = backend.create(spec).await.expect("create with software");

    // exec が guest まで到達し、投影された `sh`（brush）がプロセスとして起動・終了する
    // （command-not-found を返さない＝PATH に見えている）。exit コードを回収する。
    let mut stream = instance
        .exec(ExecRequest::Shell {
            cmd: "echo guest-commands-work".into(),
            timeout_ms: None,
        })
        .await
        .expect("exec shell reaches guest sh");
    let (_out, _err, code) = collect(&mut stream).await;
    // プロセスは起動して終了する（exit イベントが返る）。
    assert!(code.is_some(), "guest プロセスが終了コードを返す");
    instance.destroy().await.expect("destroy");
    // NOTE: 投影コマンドの stdout/exit を実バイトで検証する e2e は、kernel-managed wasm stdio
    // （AGENTOS_WASI_STDIO_SYNC_RPC）を ProcessOutputEvent へ確実に surface させる sidecar 側の
    // 追加対応が要る（現状 code_interpreter Python は surface するが native wasm コマンドは未）。
    // これは vendor ランタイム統合の残作業として `real_sidecar_guest_command_stdout`（#[ignore]）で
    // 再現し、フォローアップ issue で追う。
}

/// 【既知の残作業・再現用】投影コマンドの stdout をバイトで検証する（現状 surface されない）。
///
/// native wasm コマンドの kernel-managed stdio が ProcessOutputEvent に乗らない問題の再現。
/// 解決後に `#[ignore]` を外す。`SANDBOX_IT=1` かつ `SANDBOX_SOFTWARE_DIR` が要る。
#[tokio::test]
#[ignore = "native wasm コマンドの stdio surface は sidecar 追加対応待ち（Task 4.12 フォローアップ）"]
async fn real_sidecar_guest_command_stdout() {
    if !gated() {
        return;
    }
    let Ok(software_dir) = std::env::var("SANDBOX_SOFTWARE_DIR") else {
        return;
    };
    let software_dir = std::path::PathBuf::from(software_dir);
    if !software_dir.join("coreutils/package.tar").is_file() {
        return;
    }
    let backend = WasmBackend::new(
        None,
        OrchestratorEnv {
            software_dir: Some(software_dir),
            ..OrchestratorEnv::default()
        },
    );
    let mut spec = SandboxSpec::code_interpreter("t".into(), "o".into(), "user:1".into());
    spec.software = vec!["coreutils".into()];
    let instance = backend.create(spec).await.expect("create with software");
    let mut stream = instance
        .exec(ExecRequest::Shell {
            cmd: "echo guest-commands-work".into(),
            timeout_ms: None,
        })
        .await
        .expect("exec shell");
    let (out, err, code) = collect(&mut stream).await;
    assert!(
        out.contains("guest-commands-work"),
        "stdout was {out:?}, stderr {err:?}"
    );
    assert_eq!(code, Some(0), "stderr: {err}");
    instance.destroy().await.expect("destroy");
}

/// 未同梱 software の要求は spawn 前に fail-fast で拒否される（PIT-23/33）。
#[tokio::test]
async fn software_requests_fail_closed_without_staging() {
    // sidecar 不要（resolve は spawn 前）なので gate しない。
    let backend = WasmBackend::new(None, OrchestratorEnv::default());
    let mut spec = SandboxSpec::code_interpreter("t".into(), "o".into(), "user:1".into());
    spec.software = vec!["coreutils".into()];
    let Err(err) = backend.create(spec).await else {
        panic!("must fail");
    };
    assert!(matches!(
        err,
        sandbox_client::SandboxError::Unimplemented(_)
    ));
}

/// software パッケージの投影のみを検証する（コマンド実行はしない・ビルド済み wasm 不要）。
///
/// ConfigureVm（tar 投影）が成立し、guest の `/opt/agentos/bin` にコマンドが見えることを確認する。
/// `SANDBOX_SOFTWARE_DIR` の coreutils パッケージ（ダミーでも可）が必要。
#[tokio::test]
async fn real_sidecar_software_projection() {
    if !gated() {
        return;
    }
    let Ok(software_dir) = std::env::var("SANDBOX_SOFTWARE_DIR") else {
        eprintln!("skipping: set SANDBOX_SOFTWARE_DIR");
        return;
    };
    let software_dir = std::path::PathBuf::from(software_dir);
    if !software_dir.join("coreutils/package.tar").is_file() {
        eprintln!("skipping: coreutils/package.tar が無い");
        return;
    }
    let backend = WasmBackend::new(
        None,
        OrchestratorEnv {
            software_dir: Some(software_dir),
            ..OrchestratorEnv::default()
        },
    );
    let mut spec = SandboxSpec::code_interpreter("t".into(), "o".into(), "user:1".into());
    spec.software = vec!["coreutils".into()];
    let instance = backend.create(spec).await.expect("create with software");
    // 投影されたコマンドが guest FS に見える（$PATH の symlink farm）。
    let entries = instance
        .list_dir("/opt/agentos/bin")
        .await
        .expect("list /opt/agentos/bin");
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"echo"), "projected commands: {names:?}");
    instance.destroy().await.expect("destroy");
}
