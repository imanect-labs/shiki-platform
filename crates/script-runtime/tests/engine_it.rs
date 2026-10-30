//! script-runtime エンジンの結合テスト（Task 10.7 受け入れ条件）。
//!
//! 実 wasmtime＋vendored QuickJS ゲストで:
//! - 同期スタイル script が実行でき、`Shiki.*` が通常経路（host_fn）へ合流する
//! - 無限ループ/メモリ爆発が fuel/上限で強制中断される
//! - wasm 内からホスト関数以外の外界（fs/net）に到達できない（WASI 未付与）
//! - フレーム違反（未知 api）が実行破棄される（INV-4）
//! - コールドスタートが ms 級（スプレッドシート関数要件）

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use script_runtime::compile::compile;
use script_runtime::engine::{Limits, ScriptEngine, Termination};
use script_runtime::host::{HostCall, HostResponse};

/// storage.read だけ答えるテスト用 host_fn を作る。
fn echo_host() -> script_runtime::engine::HostFn {
    Box::new(|call: &HostCall| match call.api.as_str() {
        "storage.read" => {
            HostResponse::Ok(serde_json::json!({ "id": call.args["id"], "body": "hello" }))
        }
        "workflow.start" => HostResponse::Ok(serde_json::json!({ "runId": "r-1" })),
        other => HostResponse::Err {
            message: format!("unexpected api {other}"),
            code: "internal".into(),
            retryable: false,
        },
    })
}

fn run(
    src: &str,
    input: &str,
    limits: Limits,
    host: script_runtime::engine::HostFn,
) -> script_runtime::engine::ExecOutcome {
    let engine = ScriptEngine::new().expect("engine");
    let compiled = compile(src).expect("compile");
    engine.run("e1", &compiled.compiled_js, input, limits, host)
}

#[test]
fn runs_pure_script_and_returns_value() {
    let out = run(
        "function main(input) { return input.a + input.b; }",
        "{\"a\":2,\"b\":3}",
        Limits::default(),
        echo_host(),
    );
    assert!(out.ok, "{:?}", out.error);
    assert_eq!(out.value, Some(serde_json::json!(5)));
    assert_eq!(out.termination, Termination::Completed);
}

#[test]
fn shiki_hostcall_flows_through_host_fn() {
    let out = run(
        "function main(input) { var r = Shiki.storage.read(input.id); return r.body; }",
        "{\"id\":\"doc-1\"}",
        Limits::default(),
        echo_host(),
    );
    assert!(out.ok, "{:?}", out.error);
    assert_eq!(out.value, Some(serde_json::json!("hello")));
}

#[test]
fn shiki_log_is_captured() {
    let out = run(
        "function main() { Shiki.log.info('step 1'); Shiki.log.warn('careful'); return 1; }",
        "{}",
        Limits::default(),
        echo_host(),
    );
    assert!(out.ok);
    assert_eq!(out.logs, vec!["step 1".to_string(), "careful".to_string()]);
}

#[test]
fn infinite_loop_is_killed_by_fuel_or_epoch() {
    let limits = Limits {
        fuel: 50_000_000,
        epoch_deadline: Duration::from_secs(2),
        ..Limits::default()
    };
    let out = run(
        "function main() { while (true) {} }",
        "{}",
        limits,
        echo_host(),
    );
    assert!(!out.ok);
    assert!(
        matches!(out.termination, Termination::Fuel | Termination::Epoch),
        "無限ループは fuel/epoch で中断される: {:?}",
        out.termination
    );
}

#[test]
fn memory_explosion_is_bounded() {
    let limits = Limits {
        memory_bytes: 16 * 1024 * 1024,
        fuel: 5_000_000_000,
        ..Limits::default()
    };
    let out = run(
        "function main() { var a = []; for (var i = 0; i < 100000000; i++) { a.push(new Array(1000).fill(i)); } return a.length; }",
        "{}",
        limits,
        echo_host(),
    );
    // メモリ上限・fuel・epoch のいずれかで必ず中断され、成功しない。
    assert!(!out.ok, "メモリ爆発は成功してはならない: {:?}", out.value);
}

#[test]
fn no_fs_or_net_access() {
    // WASI を与えないため fetch/require/fs 等は存在しない → 参照時に例外。
    let out = run(
        "function main() { return typeof fetch + ',' + typeof require + ',' + typeof globalThis.process; }",
        "{}",
        Limits::default(),
        echo_host(),
    );
    assert!(out.ok, "{:?}", out.error);
    assert_eq!(
        out.value,
        Some(serde_json::json!("undefined,undefined,undefined")),
        "外界 API（fetch/require/process）は存在しない"
    );
}

#[test]
fn unknown_api_is_rejected_as_frame_violation() {
    // Shiki 経由ではないが、生の __shiki_hostcall で未知 api を投げてもフレーム検証で弾く。
    let out = run(
        "function main() { return __shiki_hostcall(JSON.stringify({ api: 'secrets.get', args: {} })); }",
        "{}",
        Limits::default(),
        echo_host(),
    );
    // フレーム違反は実行破棄（ok=false・termination=FrameViolation）か、
    // ゲストへ frame エラーが返り main がそれを返す（どちらでも「秘密は取れない」）。
    if out.ok {
        // ゲストが frame エラー応答をそのまま返した場合。
        let v = out.value.unwrap();
        let s = v.as_str().unwrap_or("");
        assert!(s.contains("frame") || s.contains("ok\":false"), "got {s}");
    } else {
        assert!(
            matches!(out.termination, Termination::FrameViolation(_)),
            "{:?}",
            out.termination
        );
    }
}

#[test]
fn shiki_fail_produces_error() {
    let out = run(
        "function main() { Shiki.fail('boom', { permanent: true }); }",
        "{}",
        Limits::default(),
        echo_host(),
    );
    assert!(!out.ok);
    let (msg, code, retryable) = out.error.unwrap();
    assert!(msg.contains("boom"), "{msg}");
    // 作成者が止めた失敗は設定の誤り（分類できない失敗にしない・BUG-137）。
    assert_eq!(code, script_runtime::SCRIPT_ERROR);
    assert!(!retryable, "permanent は非リトライ");
}

/// 決まった誤りだけを返す host_fn（呼ばれた api を問わない）。
fn failing_host(
    message: &'static str,
    code: &'static str,
    retryable: bool,
) -> script_runtime::engine::HostFn {
    Box::new(move |_call: &HostCall| HostResponse::Err {
        message: message.into(),
        code: code.into(),
        retryable,
    })
}

/// 前置き（`Shiki.data.*`）つきで 1 回実行する（ワークフローの script と同じ形）。
fn run_with_prelude(
    src: &str,
    host: script_runtime::engine::HostFn,
) -> script_runtime::engine::ExecOutcome {
    let engine = ScriptEngine::new().expect("engine");
    let compiled = compile(src).expect("compile");
    let js = format!(
        "{}\n{}",
        script_runtime::prelude::workflow_prelude(),
        compiled.compiled_js
    );
    engine.run("e1", &js, "{}", Limits::default(), host)
}

/// 捕まえていないホスト呼び出しの誤りは、ホストが返した code・retryable・文のまま失敗になる（BUG-136・D-121）。
/// ゲストの `Shiki.*`（storage）と前置きの `Shiki.data.*` のどちらから投げても同じ。捕まえて同じ誤りを投げ直しても
/// ホストの誤りのまま。文にタブを含んでも（ゲストの封筒が JSON として崩れても）読める。
#[test]
fn uncaught_host_errors_keep_the_host_code() {
    let msg = "scope_ceiling 外の Shiki 呼び出し: storage.read";
    for src in [
        "function main() { return Shiki.storage.read('x'); }",
        "function main() { return Shiki.data.get('t', 'r'); }",
        "function main() { try { Shiki.storage.read('x'); } catch (e) { throw e; } }",
    ] {
        let out = run_with_prelude(src, failing_host(msg, "out_of_scope", false));
        assert!(!out.ok, "{src}");
        assert_eq!(
            out.error,
            Some((msg.to_string(), "out_of_scope".to_string(), false)),
            "{src}"
        );
    }
    let out = run_with_prelude(
        "function main() { return Shiki.storage.read('x'); }",
        failing_host("混んでいます\tしばらく待ってください", "unavailable", true),
    );
    assert_eq!(
        out.error,
        Some((
            "混んでいます\tしばらく待ってください".to_string(),
            "unavailable".to_string(),
            true
        ))
    );
}

/// 作成者が投げた誤り（throw・書き誤り・ホストの誤りの文を変えて投げ直したもの・code を名乗るもの）は
/// script_error（やり直さない）になる（BUG-137・D-121）。捕まえて続けた script は成功する。
#[test]
fn author_errors_are_script_errors() {
    for src in [
        "function main() { throw new Error('金額が負です'); }",
        "function main() { var a = null; return a.b; }",
        "function main() { throw 'str'; }",
        "function main() { try { Shiki.storage.read('x'); } catch (e) { throw new Error('読めません: ' + e.message); } }",
        "function main() { try { Shiki.storage.read('x'); } catch (e) { } throw { code: 'rate_limited', retryable: true }; }",
    ] {
        let out = run_with_prelude(src, failing_host("見つかりません", "not_found", false));
        assert!(!out.ok, "{src}");
        let (_msg, code, retryable) = out.error.clone().unwrap();
        assert_eq!(code, script_runtime::SCRIPT_ERROR, "{src}: {:?}", out.error);
        assert!(!retryable, "{src}");
    }
    let out = run_with_prelude(
        "function main() { try { Shiki.storage.read('x'); } catch (e) { return e.code; } }",
        failing_host("見つかりません", "not_found", false),
    );
    assert!(out.ok, "{:?}", out.error);
    assert_eq!(out.value, Some(serde_json::json!("not_found")));
}

#[test]
fn cold_start_is_sub_second() {
    // Module コンパイル込みでも 1 実行が十分速いこと（スプレッドシート関数要件の粗い担保）。
    let engine = ScriptEngine::new().expect("engine");
    let compiled = compile("function main(i){return i.n*2;}").expect("compile");
    let t = Instant::now();
    let out = engine.run(
        "e1",
        &compiled.compiled_js,
        "{\"n\":21}",
        Limits::default(),
        echo_host(),
    );
    let elapsed = t.elapsed();
    assert!(out.ok);
    assert_eq!(out.value, Some(serde_json::json!(42)));
    assert!(
        elapsed < Duration::from_millis(500),
        "1 実行が遅すぎる: {elapsed:?}"
    );
}
