use serde_json::Value;
use wasmtime::Store;

use super::{ExecOutcome, HostFn, HostState, Limits, ScriptEngine, Termination};
use crate::compile::compile;
use crate::host::{HostCall, HostResponse};

fn noop_host() -> HostFn {
    Box::new(|_call: &HostCall| HostResponse::Ok(Value::Null))
}

fn run(src: &str, input: &str, limits: Limits, host: HostFn) -> ExecOutcome {
    let engine = ScriptEngine::new().expect("engine");
    let compiled = compile(src).expect("compile");
    engine.run("e-unit", &compiled.compiled_js, input, limits, host)
}

#[test]
fn epoch_helpers_are_usable() {
    // テスト補助 API（epoch を進める・複製ハンドルからも進める）が動く。
    let engine = ScriptEngine::new().expect("engine");
    engine.increment_epoch();
    let handle = engine.engine_handle();
    handle.increment_epoch();
}

#[test]
fn oversized_result_is_trapped_not_read() {
    // 256KB を超える戻り値は read せず trap にする（step-output cap・ホスト RAM 保護）。
    let out = run(
        "function main() { return 'x'.repeat(300000); }",
        "null",
        Limits::default(),
        noop_host(),
    );
    assert!(!out.ok, "{out:?}");
    assert!(matches!(out.termination, Termination::Trap(_)));
    let (msg, _code, _r) = out.error.expect("error tuple");
    assert!(msg.contains("result too large"), "{msg}");
}

#[test]
fn oversized_host_call_request_is_rejected() {
    // 1MB を超える hostcall リクエストは read せず 0（失敗）を返す（runtime→server ≤1MB）。
    // ゲストは packed=0 を「host returned empty」として受け取る。
    let out = run(
            "function main() { var s = 'x'.repeat(1100000); return __shiki_hostcall(JSON.stringify({ api: 'log', args: { message: s } })); }",
            "null",
            Limits::default(),
            noop_host(),
        );
    assert!(out.ok, "{out:?}");
    let v = out.value.expect("value");
    let s = v.as_str().unwrap_or_default();
    assert!(s.contains("host returned empty"), "{s}");
}

#[test]
fn raw_hostcall_with_invalid_json_is_frame_violation() {
    // フレームが JSON でない → 「invalid host call json」で違反記録・実行破棄。
    let out = run(
        "function main() { __shiki_hostcall('this is not json'); return 1; }",
        "null",
        Limits::default(),
        noop_host(),
    );
    assert!(!out.ok, "{out:?}");
    assert!(matches!(out.termination, Termination::FrameViolation(_)));
    let (_msg, code, _r) = out.error.expect("error tuple");
    assert_eq!(code, "frame_violation");
}

#[test]
fn frame_violation_then_trap_stays_frame_violation() {
    // 違反記録後に trap しても、trap 分類より違反を優先する（classify_trap の最優先分岐）。
    let limits = Limits {
        fuel: 200_000_000,
        ..Limits::default()
    };
    let out = run(
            "function main() { __shiki_hostcall(JSON.stringify({ api: 'secrets.get', args: {} })); while (true) {} }",
            "null",
            limits,
            noop_host(),
        );
    assert!(!out.ok, "{out:?}");
    assert!(matches!(out.termination, Termination::FrameViolation(_)));
}

#[test]
fn long_log_line_is_truncated() {
    // 1 行 16KB 上限を超える log は UTF-8 境界で切って "…[truncated]" を付す。
    let out = run(
        "function main() { Shiki.log.info('y'.repeat(20000)); return 1; }",
        "null",
        Limits::default(),
        noop_host(),
    );
    assert!(out.ok, "{out:?}");
    assert_eq!(out.logs.len(), 1);
    let line = &out.logs[0];
    assert!(line.ends_with("…[truncated]"), "{line}");
    // 16KB 分の 'y' ＋ 付与マーカのみ（元の 20000 より短い）。
    assert!(line.len() < 20000);
    assert!(line.starts_with("yyyy"));
}

#[test]
fn logs_over_hundred_lines_are_capped() {
    // 100 行上限に達したら 101 行目に打切りマーカを 1 度だけ足し、以降は捨てる。
    let out = run(
        "function main() { for (var i = 0; i < 150; i++) { Shiki.log.info('l' + i); } return 1; }",
        "null",
        Limits::default(),
        noop_host(),
    );
    assert!(out.ok, "{out:?}");
    assert_eq!(out.logs.len(), 101);
    assert_eq!(out.logs[0], "l0");
    assert_eq!(out.logs[99], "l99");
    assert_eq!(out.logs[100], "…[log truncated: 100 行上限]");
}

#[test]
fn terminated_maps_every_variant() {
    // ExecOutcome::terminated（runtime_io）の全中断種別 → (message, code) 対応を検証する。
    let eng = wasmtime::Engine::default();
    let mut store = Store::new(&eng, HostState::new_for_test(noop_host()));

    let cases = [
        (Termination::Fuel, "fuel exhausted", "resource"),
        (Termination::Epoch, "time limit exceeded", "resource"),
        (Termination::Memory, "memory limit exceeded", "resource"),
        (Termination::Cancelled, "cancelled", "cancelled"),
        (Termination::Completed, "completed", "internal"),
    ];
    for (term, msg, code) in cases {
        let o = ExecOutcome::terminated(term.clone(), &mut store);
        assert!(!o.ok);
        assert!(o.value.is_none());
        assert_eq!(
            o.error,
            Some((msg.to_string(), code.to_string(), false)),
            "variant {term:?}"
        );
    }

    let fv = ExecOutcome::terminated(
        Termination::FrameViolation("bad-api".to_string()),
        &mut store,
    );
    assert_eq!(
        fv.error,
        Some((
            "frame violation: bad-api".to_string(),
            "frame_violation".to_string(),
            false
        ))
    );

    let tr = ExecOutcome::terminated(Termination::Trap("kaboom".to_string()), &mut store);
    assert_eq!(
        tr.error,
        Some(("kaboom".to_string(), "internal".to_string(), false))
    );
}
