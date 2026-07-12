//! skill body 検証（`validate_skill_body` / `validate_scripts`・Task 6.7）の検証マトリクス。
//! 純粋・依存なし: JSON を与えて収集されるエラーコードを検査する。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use gui::validate_skill_body;
use serde_json::{json, Value};

/// 収集された検証エラーコード一覧。
fn error_codes(raw: Value) -> Vec<String> {
    match validate_skill_body(&raw) {
        Ok(_) => Vec::new(),
        Err(errors) => errors.into_iter().map(|e| e.code).collect(),
    }
}

fn assert_rejected_with(raw: Value, code: &str) {
    let codes = error_codes(raw);
    assert!(
        codes.iter().any(|c| c == code),
        "expected code {code}, got {codes:?}"
    );
}

/// 最小の妥当な body（description ＋ instructions のみ）。
fn minimal() -> Value {
    json!({ "description": "経費の質問に答える", "instructions": "# 手順\n丁寧に答える。" })
}

const NIL: &str = "00000000-0000-0000-0000-000000000000";

#[test]
fn minimal_body_is_valid() {
    let body = validate_skill_body(&minimal()).expect("minimal は妥当");
    assert_eq!(body.description, "経費の質問に答える");
    assert!(body.knowledge_scope.is_none());
    assert!(body.scripts.is_empty());
}

#[test]
fn full_valid_body_passes() {
    let raw = json!({
        "description": "d",
        "instructions": "i",
        "knowledge_scope": { "folders": [NIL], "files": [] },
        "model": { "model": "claude-sonnet-5", "temperature": 0.7, "max_tokens": 2048 },
        "few_shot": [{ "user": "q", "assistant": "a" }],
        "scripts": [{ "path": "scripts/run.shiki", "kind": "shiki", "source": "1" }],
        "references": [NIL]
    });
    assert!(validate_skill_body(&raw).is_ok(), "codes={:?}", error_codes(raw));
}

#[test]
fn unknown_field_is_schema_violation() {
    let mut raw = minimal();
    raw["bogus"] = json!(1);
    assert_rejected_with(raw, "skill.schema_violation");
}

#[test]
fn empty_description_and_instructions_rejected() {
    assert_rejected_with(
        json!({ "description": "  ", "instructions": "i" }),
        "skill.empty_description",
    );
    assert_rejected_with(
        json!({ "description": "d", "instructions": "   " }),
        "skill.empty_instructions",
    );
}

#[test]
fn overlong_description_and_instructions_rejected() {
    let long_desc = "あ".repeat(1025);
    assert_rejected_with(
        json!({ "description": long_desc, "instructions": "i" }),
        "skill.too_long",
    );
    let long_instr = "x".repeat(32 * 1024 + 1);
    assert_rejected_with(
        json!({ "description": "d", "instructions": long_instr }),
        "skill.too_long",
    );
}

#[test]
fn knowledge_scope_empty_and_overfull_rejected() {
    let mut raw = minimal();
    raw["knowledge_scope"] = json!({ "folders": [], "files": [] });
    assert_rejected_with(raw, "skill.empty_scope");

    let too_many = vec![NIL; 101];
    let mut raw = minimal();
    raw["knowledge_scope"] = json!({ "folders": too_many, "files": [] });
    assert_rejected_with(raw, "skill.too_many_refs");
}

#[test]
fn empty_allowed_tools_rejected() {
    let mut raw = minimal();
    raw["allowed_tools"] = json!([]);
    assert_rejected_with(raw, "skill.empty_allowed_tools");
}

#[test]
fn invalid_model_params_rejected() {
    let mut raw = minimal();
    raw["model"] = json!({ "temperature": 2.5 });
    assert_rejected_with(raw, "skill.invalid_temperature");

    let mut raw = minimal();
    raw["model"] = json!({ "max_tokens": 0 });
    assert_rejected_with(raw, "skill.invalid_max_tokens");

    let mut raw = minimal();
    raw["model"] = json!({ "max_tokens": 999_999 });
    assert_rejected_with(raw, "skill.invalid_max_tokens");
}

#[test]
fn few_shot_limits_rejected() {
    let many: Vec<Value> = (0..9).map(|_| json!({ "user": "u", "assistant": "a" })).collect();
    let mut raw = minimal();
    raw["few_shot"] = json!(many);
    assert_rejected_with(raw, "skill.too_many_few_shot");

    let mut raw = minimal();
    raw["few_shot"] = json!([{ "user": "x".repeat(4001), "assistant": "a" }]);
    assert_rejected_with(raw, "skill.too_long");
}

#[test]
fn script_path_safety_enforced() {
    for bad in ["../evil.shiki", "/abs/x.shiki", "a\\b.shiki", "scripts/€.shiki"] {
        let mut raw = minimal();
        raw["scripts"] = json!([{ "path": bad, "kind": "shiki", "source": "1" }]);
        assert_rejected_with(raw, "skill.invalid_script_path");
    }
}

#[test]
fn script_kind_extension_mismatch_rejected() {
    let mut raw = minimal();
    raw["scripts"] = json!([{ "path": "scripts/a.sh", "kind": "shiki", "source": "1" }]);
    assert_rejected_with(raw, "skill.script_kind_mismatch");
}

#[test]
fn duplicate_script_path_rejected() {
    let mut raw = minimal();
    raw["scripts"] = json!([
        { "path": "scripts/a.shiki", "kind": "shiki", "source": "1" },
        { "path": "scripts/a.shiki", "kind": "shiki", "source": "2" }
    ]);
    assert_rejected_with(raw, "skill.duplicate_script_path");
}

#[test]
fn too_many_references_rejected() {
    let refs = vec![NIL; 51];
    let mut raw = minimal();
    raw["references"] = json!(refs);
    assert_rejected_with(raw, "skill.too_many_refs");
}
