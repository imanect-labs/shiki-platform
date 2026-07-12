//! 制御ノード純関数（branch/switch/wait/map）の評価を検証する。
//! これらは `self` を取らない関連関数で能力を呼ばないため、DB/ポート無しで直接叩ける。
//! （exec.rs の 500 行上限を守るため別ファイルへ分離し `#[path]` で結線する。）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::CapabilityNodeExecutor as Exec;
use crate::run::{NodeContext, OnItemError, OnTimeout, Suspend};
use serde_json::{json, Value};
use uuid::Uuid;

/// リテラル式のみを使う最小 NodeContext（リゾルバは参照しない）。
fn ctx(input: Value) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        step_path: "s0".into(),
        idempotency_key: "k0".into(),
        attempt: 0,
        principal: "alice".into(),
        principal_kind: "user".into(),
        input,
        trigger: json!({}),
        node_outputs: json!({}),
        each: None,
        trace_id: None,
        scope_ceiling: vec![],
    }
}

fn err_code(r: &crate::run::NodeResult) -> String {
    r.error.as_ref().unwrap().code.clone()
}

// ---- branch ----
#[test]
fn branch_true_and_false_ports() {
    let c = ctx(json!({ "keep": 1 }));
    let t = Exec::eval_branch(&json!({ "condition": { "cmp": { "left": 5, "op": "gt", "right": 3 } } }), &c);
    assert!(t.ok);
    assert_eq!(t.taken_ports, vec!["true".to_string()]);
    assert_eq!(t.output, json!({ "keep": 1 }), "input を素通しする");

    let f = Exec::eval_branch(&json!({ "condition": { "cmp": { "left": 1, "op": "gt", "right": 3 } } }), &c);
    assert!(f.ok);
    assert_eq!(f.taken_ports, vec!["false".to_string()]);
}

#[test]
fn branch_bad_params_fails() {
    let r = Exec::eval_branch(&json!({ "condition": "not-a-condition" }), &ctx(json!({})));
    assert!(!r.ok);
    assert_eq!(err_code(&r), "bad_params");
}

// ---- switch ----
#[test]
fn switch_matches_case_then_default() {
    let c = ctx(json!({}));
    let hit = Exec::eval_switch(
        &json!({ "value": "b", "cases": [{ "port": "pa", "equals": "a" }, { "port": "pb", "equals": "b" }] }),
        &c,
    );
    assert_eq!(hit.taken_ports, vec!["pb".to_string()]);

    let miss = Exec::eval_switch(
        &json!({ "value": "z", "cases": [{ "port": "pa", "equals": "a" }] }),
        &c,
    );
    assert_eq!(miss.taken_ports, vec!["default".to_string()]);
}

#[test]
fn switch_bad_params_fails() {
    let r = Exec::eval_switch(&json!({ "value": "x", "cases": "not-array" }), &ctx(json!({})));
    assert!(!r.ok);
    assert_eq!(err_code(&r), "bad_params");
}

// ---- wait: duration ----
#[test]
fn wait_duration_ok_and_error_paths() {
    let c = ctx(json!({}));
    let ok = Exec::eval_wait(&json!({ "kind": "duration", "duration_sec": 60 }), &c);
    assert!(matches!(ok.suspend, Some(Suspend::Timer { .. })));

    let missing = Exec::eval_wait(&json!({ "kind": "duration" }), &c);
    assert_eq!(err_code(&missing), "bad_params");

    let negative = Exec::eval_wait(&json!({ "kind": "duration", "duration_sec": -1 }), &c);
    assert_eq!(err_code(&negative), "bad_params");

    let too_big = Exec::eval_wait(&json!({ "kind": "duration", "duration_sec": i64::MAX }), &c);
    assert_eq!(err_code(&too_big), "bad_params");
}

// ---- wait: until ----
#[test]
fn wait_until_ok_and_error_paths() {
    let c = ctx(json!({}));
    let ok = Exec::eval_wait(&json!({ "kind": "until", "until": "2030-01-01T00:00:00Z" }), &c);
    assert!(matches!(ok.suspend, Some(Suspend::Timer { .. })));

    let missing = Exec::eval_wait(&json!({ "kind": "until" }), &c);
    assert_eq!(err_code(&missing), "bad_params");

    let bad = Exec::eval_wait(&json!({ "kind": "until", "until": "not-a-date" }), &c);
    assert_eq!(err_code(&bad), "bad_params");
}

// ---- wait: event ----
#[test]
fn wait_event_ok_default_fail_and_no_timeout() {
    let r = Exec::eval_wait(&json!({ "kind": "event", "source": "file.created" }), &ctx(json!({})));
    match r.suspend {
        Some(Suspend::Event { source, timeout_at, on_timeout, .. }) => {
            assert_eq!(source, "file.created");
            assert!(timeout_at.is_none(), "timeout_sec 省略は無期限");
            assert_eq!(on_timeout, OnTimeout::Fail, "既定は fail");
        }
        other => panic!("expected Event suspend, got {other:?}"),
    }
}

#[test]
fn wait_event_with_timeout_continue() {
    let r = Exec::eval_wait(
        &json!({ "kind": "event", "source": "file.created", "timeout_sec": 30, "on_timeout": "continue" }),
        &ctx(json!({})),
    );
    match r.suspend {
        Some(Suspend::Event { timeout_at, on_timeout, .. }) => {
            assert!(timeout_at.is_some());
            assert_eq!(on_timeout, OnTimeout::Continue);
        }
        other => panic!("expected Event suspend, got {other:?}"),
    }
}

#[test]
fn wait_event_error_paths() {
    let c = ctx(json!({}));
    let missing_source = Exec::eval_wait(&json!({ "kind": "event" }), &c);
    assert_eq!(err_code(&missing_source), "bad_params");

    let neg_timeout =
        Exec::eval_wait(&json!({ "kind": "event", "source": "x", "timeout_sec": -5 }), &c);
    assert_eq!(err_code(&neg_timeout), "bad_params");
}

#[test]
fn wait_unknown_kind_fails() {
    let r = Exec::eval_wait(&json!({ "kind": "bogus" }), &ctx(json!({})));
    assert_eq!(err_code(&r), "bad_params");
}

// ---- map ----
#[test]
fn map_ok_defaults_and_overrides() {
    let c = ctx(json!({}));
    let d = Exec::eval_map(&json!({ "items": [1, 2, 3] }), &c);
    let f = d.fanout.expect("fanout");
    assert_eq!(f.items.len(), 3);
    assert_eq!(f.max_concurrency, 10, "既定同時実行");
    assert_eq!(f.on_item_error, OnItemError::FailMap, "既定 fail_map");

    // items は数値リテラル配列にする（文字列配列は untagged ValueExpr が Template と解釈するため）。
    let o = Exec::eval_map(
        &json!({ "items": [1], "max_concurrency": 5, "on_item_error": "collect" }),
        &c,
    );
    let f = o.fanout.expect("fanout");
    assert_eq!(f.max_concurrency, 5);
    assert_eq!(f.on_item_error, OnItemError::Collect);
}

#[test]
fn map_non_array_and_limit_fail() {
    let c = ctx(json!({}));
    let not_array = Exec::eval_map(&json!({ "items": "nope" }), &c);
    assert_eq!(err_code(&not_array), "bad_params");

    let over: Vec<i64> = (0..1001).collect();
    let too_many = Exec::eval_map(&json!({ "items": over }), &c);
    assert_eq!(err_code(&too_many), "fanout_limit_exceeded");
}

#[test]
fn map_bad_params_fails() {
    let r = Exec::eval_map(&json!({ "max_concurrency": 3 }), &ctx(json!({})));
    assert_eq!(err_code(&r), "bad_params");
}
