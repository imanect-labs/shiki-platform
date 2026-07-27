//! Pyodide heap-snapshot fast-path coverage.
//!
//! The first execution's prewarm boots Pyodide with `_makeSnapshot`, persists
//! the heap snapshot into the cross-process store, and the execution itself
//! (plus every later execution on the same asset set) restores from it instead
//! of paying the full CPython-on-WASM bootstrap.
//!
//! Hosts without the pinned Pyodide bundle can point the suite at any
//! self-consistent Pyodide dist directory via `SECURE_EXEC_TEST_PYODIDE_DIST`.

use secure_exec_execution::{
    CreatePythonContextRequest, PythonExecutionEngine, PythonExecutionEvent,
    StartPythonExecutionRequest,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::tempdir;

const PYTHON_WARMUP_METRICS_PREFIX: &str = "__AGENTOS_PYTHON_WARMUP_METRICS__:";

fn setup_engine() -> (PythonExecutionEngine, String) {
    let mut engine = PythonExecutionEngine::default();
    let bundled_dir = engine
        .bundled_pyodide_dist_path_for_vm("vm-python-snapshot")
        .expect("materialize bundled pyodide");
    let pyodide_dir = std::env::var_os("SECURE_EXEC_TEST_PYODIDE_DIST")
        .map(PathBuf::from)
        .unwrap_or(bundled_dir);
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python-snapshot"),
        pyodide_dist_path: pyodide_dir,
    });
    (engine, context.context_id)
}

fn run_python_execution(
    engine: &mut PythonExecutionEngine,
    context_id: &str,
    cwd: &Path,
    code: &str,
    extra_env: &[(&str, &str)],
) -> (String, String, i32) {
    let mut env = BTreeMap::from([(
        String::from("AGENTOS_PYTHON_WARMUP_DEBUG"),
        String::from("1"),
    )]);
    for (key, value) in extra_env {
        env.insert((*key).to_string(), (*value).to_string());
    }
    let mut execution = engine
        .start_execution(StartPythonExecutionRequest {
            guest_runtime: Default::default(),
            limits: Default::default(),
            vm_id: String::from("vm-python-snapshot"),
            context_id: context_id.to_string(),
            code: code.to_string(),
            file_path: None,
            env,
            cwd: cwd.to_path_buf(),
        })
        .expect("start Python execution");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        match execution
            .poll_event_blocking(Duration::from_secs(120))
            .expect("poll Python event")
        {
            Some(PythonExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(PythonExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(PythonExecutionEvent::JavascriptSyncRpcRequest(request)) => {
                let serviced = execution
                    .try_service_standalone_module_sync_rpc(&request)
                    .expect("service module sync RPC");
                assert!(serviced, "unexpected JS sync RPC request: {request:?}");
            }
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                execution
                    .respond_vfs_rpc_error(request.id, "ENOSYS", "no VFS backend in snapshot test")
                    .expect("respond to VFS RPC");
            }
            Some(PythonExecutionEvent::Exited(exit_code)) => {
                return (
                    String::from_utf8(stdout).expect("stdout utf8"),
                    String::from_utf8(stderr).expect("stderr utf8"),
                    exit_code,
                );
            }
            None => panic!("timed out waiting for Python execution event"),
        }
    }
}

fn parse_metrics(stderr: &str, phase: &str) -> Value {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix(PYTHON_WARMUP_METRICS_PREFIX))
        .map(|line| serde_json::from_str::<Value>(line).expect("parse metrics json"))
        .find(|value| value.get("phase").and_then(Value::as_str) == Some(phase))
        .unwrap_or_else(|| panic!("missing {phase} metrics in stderr: {stderr}"))
}

fn python_snapshot_restores_after_first_execution() {
    let temp = tempdir().expect("create temp dir");
    let store = tempdir().expect("create snapshot store dir");
    // Isolate the cross-process store per test run.
    std::env::set_var("AGENTOS_PYTHON_SNAPSHOT_STORE", store.path());
    let (mut engine, context_id) = setup_engine();

    let (first_stdout, first_stderr, first_exit) = run_python_execution(
        &mut engine,
        &context_id,
        temp.path(),
        "print('snap-first')",
        &[],
    );
    assert_eq!(first_exit, 0, "stderr: {first_stderr}");
    assert_eq!(first_stdout, "snap-first\n");

    let first_snapshot = parse_metrics(&first_stderr, "snapshot");
    let first_startup = parse_metrics(&first_stderr, "startup");
    assert_eq!(
        first_snapshot["state"], "created",
        "first prewarm should create the heap snapshot: {first_stderr}"
    );
    assert_eq!(
        first_startup["snapshot"], "restored",
        "first execution should restore from the freshly created snapshot: {first_stderr}"
    );

    let (second_stdout, second_stderr, second_exit) = run_python_execution(
        &mut engine,
        &context_id,
        temp.path(),
        "print('snap-second')",
        &[],
    );
    assert_eq!(second_exit, 0, "stderr: {second_stderr}");
    assert_eq!(second_stdout, "snap-second\n");

    assert!(
        second_stderr.contains("wasm-compile-cache:hit"),
        "second execution in the same process should reuse the compiled pyodide.asm.wasm: {second_stderr}"
    );
    let second_snapshot = parse_metrics(&second_stderr, "snapshot");
    let second_startup = parse_metrics(&second_stderr, "startup");
    assert_eq!(
        second_snapshot["state"], "reused",
        "second execution should reuse the stored snapshot: {second_stderr}"
    );
    assert_eq!(second_startup["snapshot"], "restored");
    assert_eq!(
        second_startup["snapshotTransport"], "typed",
        "snapshot payload should arrive over the typed binary channel: {second_stderr}"
    );
    assert!(
        second_startup["stages"]["micropipMs"].is_null(),
        "micropip should come from the snapshot, not a per-exec loadPackage: {second_stderr}"
    );

    let first_prewarm = parse_metrics(&first_stderr, "prewarm");
    println!(
        "snapshot timings: prewarm-with-create={}ms first-restore loadPyodide={}ms (read {}ms) second-restore loadPyodide={}ms (read {}ms)",
        first_prewarm["durationMs"],
        first_startup["loadPyodideMs"],
        first_startup["snapshotMs"],
        second_startup["loadPyodideMs"],
        second_startup["snapshotMs"],
    );
    println!("restored startup metrics: {second_startup}");
}

fn python_snapshot_disable_env_keeps_fresh_boot() {
    let temp = tempdir().expect("create temp dir");
    let store = tempdir().expect("create snapshot store dir");
    std::env::set_var("AGENTOS_PYTHON_SNAPSHOT_STORE", store.path());
    let (mut engine, context_id) = setup_engine();

    let (stdout, stderr, exit_code) = run_python_execution(
        &mut engine,
        &context_id,
        temp.path(),
        "print('fresh')",
        &[("AGENTOS_PYTHON_SNAPSHOT", "0")],
    );
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "fresh\n");

    let startup = parse_metrics(&stderr, "startup");
    assert_eq!(startup["snapshot"], "off");
    assert!(
        std::fs::read_dir(store.path())
            .expect("read snapshot store dir")
            .next()
            .is_none(),
        "disabled runs must not populate the snapshot store"
    );
    println!(
        "fresh-boot timings: loadPyodide={}ms",
        startup["loadPyodideMs"]
    );
    println!("fresh startup metrics: {startup}");
}

/// Restored interpreters must be indistinguishable from fresh boots for guest
/// code: same cwd, env, argv, version, and the same failure behavior.
fn python_snapshot_restore_matches_fresh_runtime_state() {
    let temp = tempdir().expect("create temp dir");
    let store = tempdir().expect("create snapshot store dir");
    std::env::set_var("AGENTOS_PYTHON_SNAPSHOT_STORE", store.path());
    let (mut engine, context_id) = setup_engine();

    let state_probe = "import os, sys\n\
        print(os.getcwd())\n\
        print(os.environ.get('HOME'))\n\
        print(sys.argv)\n\
        print(sys.version.split()[0])";

    let (fresh_stdout, fresh_stderr, fresh_exit) = run_python_execution(
        &mut engine,
        &context_id,
        temp.path(),
        state_probe,
        &[("AGENTOS_PYTHON_SNAPSHOT", "0")],
    );
    assert_eq!(fresh_exit, 0, "stderr: {fresh_stderr}");

    let (restored_stdout, restored_stderr, restored_exit) =
        run_python_execution(&mut engine, &context_id, temp.path(), state_probe, &[]);
    assert_eq!(restored_exit, 0, "stderr: {restored_stderr}");
    assert_eq!(
        parse_metrics(&restored_stderr, "startup")["snapshot"],
        "restored"
    );
    assert_eq!(
        restored_stdout, fresh_stdout,
        "restored interpreter state must match a fresh boot"
    );

    let failing = "1/0";
    let (_, fresh_fail_stderr, fresh_fail_exit) = run_python_execution(
        &mut engine,
        &context_id,
        temp.path(),
        failing,
        &[("AGENTOS_PYTHON_SNAPSHOT", "0")],
    );
    let (_, restored_fail_stderr, restored_fail_exit) =
        run_python_execution(&mut engine, &context_id, temp.path(), failing, &[]);
    assert_eq!(fresh_fail_exit, restored_fail_exit);
    assert!(
        fresh_fail_stderr.contains("ZeroDivisionError")
            && restored_fail_stderr.contains("ZeroDivisionError"),
        "both paths must surface the Python exception"
    );
}

// Same shared-process V8 constraint as `python_prewarm.rs`: keep the collapsed
// suite for `cargo test` and expose per-case tests for cargo-nextest.
#[test]
fn python_snapshot_suite() {
    if std::env::var_os("NEXTEST").is_some() {
        return;
    }
    python_snapshot_restores_after_first_execution();
    python_snapshot_disable_env_keeps_fresh_boot();
    python_snapshot_restore_matches_fresh_runtime_state();
}

mod python_snapshot_split {
    macro_rules! nextest_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[test]
                fn $case() {
                    if std::env::var_os("NEXTEST").is_none() {
                        return;
                    }
                    super::$case();
                }
            )+
        };
    }

    nextest_cases!(
        python_snapshot_restores_after_first_execution,
        python_snapshot_disable_env_keeps_fresh_boot,
        python_snapshot_restore_matches_fresh_runtime_state,
    );
}
