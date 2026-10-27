//! 判定のノードの V3（モデルの照合）と警告の試験。

#![allow(clippy::needless_pass_by_value)] // json! をそのまま渡す。

use serde_json::{json, Value};

use super::*;
use crate::ir::validate::{save_warnings, validate};

fn decide_node(model: &str) -> Value {
    json!({ "id": "judge", "type": "ai.decide", "params": {
        "model": model, "state": { "$from": "input" },
        "questions": [{ "key": "angry", "type": "boolean", "instructions": "強い不満があるか" }],
        "route": { "question": "angry", "min_confidence": 0.8 } } })
}

fn ir(nodes: Value) -> Value {
    json!({ "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": nodes, "edges": [] })
}

fn catalog(chat: &[&str], decision: &[&str]) -> Catalog {
    let ids = |v: &[&str]| Some(v.iter().map(ToString::to_string).collect());
    Catalog {
        models: ids(chat),
        decision_models: ids(decision),
        ..Catalog::default()
    }
}

fn errors(ir: &Value, catalog: &Catalog) -> Vec<ValidationError> {
    validate(ir, catalog).err().unwrap_or_default()
}

#[test]
fn a_decision_model_passes_and_others_are_told_apart() {
    let cat = catalog(&["haiku", "sonnet"], &["decide-haiku", "decide-vote"]);
    assert!(errors(&ir(json!([decide_node("decide-haiku")])), &cat).is_empty());

    // 文章を生成するモデルは種類の違い（使える判定用のモデルを添える）。
    let errs = errors(&ir(json!([decide_node("haiku")])), &cat);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].code, "ir.model_modality_mismatch");
    assert_eq!(errs[0].node_id.as_deref(), Some("judge"));
    assert_eq!(errs[0].path.as_deref(), Some("/params/model"));
    assert!(
        errs[0]
            .message
            .contains("使える判定用のモデル: decide-haiku, decide-vote"),
        "{}",
        errs[0].message
    );

    // どちらにも無いモデルは未知のモデル。
    let errs = errors(&ir(json!([decide_node("nope")])), &cat);
    assert_eq!(errs[0].code, "ir.unknown_model");
    assert!(
        errs[0].message.contains("decide-haiku"),
        "{}",
        errs[0].message
    );

    // 判定用のモデルが 0 件の組織では、どの判定も拒否する（照合を飛ばさない）。
    let errs = errors(
        &ir(json!([decide_node("decide-haiku")])),
        &catalog(&["haiku"], &[]),
    );
    assert_eq!(errs[0].code, "ir.unknown_model");
    assert!(
        errs[0]
            .message
            .contains("組織で使える判定用のモデルがありません"),
        "{}",
        errs[0].message
    );

    // 照合の材料が無い構成（LLM 未構成）は照合しない（実行時に gateway が拒否する）。
    assert!(errors(&ir(json!([decide_node("anything")])), &Catalog::default()).is_empty());
}

/// 生成のノード（llm.invoke・agent.invoke）に判定用のモデルを書いたら、種類の違いとして拒否する。
#[test]
fn generation_nodes_reject_decision_models() {
    let cat = catalog(&["haiku"], &["decide-haiku"]);
    let llm = json!({ "id": "ask", "type": "llm.invoke",
                      "params": { "model": "decide-haiku", "prompt": "要約して" } });
    let errs = errors(&ir(json!([llm])), &cat);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].code, "ir.model_modality_mismatch");
    assert_eq!(errs[0].path.as_deref(), Some("/params/model"));
    assert!(errs[0].message.contains("ai.decide"), "{}", errs[0].message);
    let agent = json!({ "id": "agent", "type": "agent.invoke",
                        "params": { "model": "decide-haiku", "instruction": "調べて" } });
    let errs = errors(&ir(json!([agent])), &cat);
    assert_eq!(errs[0].code, "ir.model_modality_mismatch", "{errs:?}");
}

/// 組織がモデルの選択を禁じていても、判定のモデルには掛けない（判定には組織の既定が無い）。
#[test]
fn the_model_lock_does_not_apply_to_decisions() {
    let cat = Catalog {
        model_lock: Some("auto".into()),
        ..catalog(&["haiku"], &["decide-haiku"])
    };
    assert!(errors(&ir(json!([decide_node("decide-haiku")])), &cat).is_empty());
}

#[test]
fn warnings_flag_low_thresholds_many_ports_and_unpinned_models() {
    let parse = |v: Value| crate::ir::WorkflowIr::from_json(&v).unwrap();
    let codes = |v: Value| -> Vec<String> {
        save_warnings(&parse(v))
            .into_iter()
            .map(|w| w.code)
            .collect()
    };
    assert!(codes(ir(json!([decide_node("decide-haiku")]))).is_empty());
    let mut low = decide_node("decide-haiku");
    low["params"]["route"]["min_confidence"] = json!(0.59);
    assert_eq!(codes(ir(json!([low]))), [DECIDE_LOW_CONFIDENCE]);
    let mut ok = decide_node("decide-haiku");
    ok["params"]["route"]["min_confidence"] = json!(0.6);
    assert!(codes(ir(json!([ok]))).is_empty());
    assert_eq!(
        codes(ir(json!([decide_node("decide-preview")]))),
        [DECIDE_UNPINNED_MODEL]
    );
    let options =
        |n: usize| -> Vec<Value> { (0..n).map(|i| json!({ "key": format!("o{i}") })).collect() };
    let many = |by: &str, n: usize| {
        json!({ "id": "d", "type": "ai.decide", "params": { "model": "decide-haiku", "state": "x",
            "questions": [{ "key": "kind", "type": "choice", "instructions": "種類", "options": options(n) }],
            "route": { "question": "kind", "by": by, "min_confidence": 0.8 } } })
    };
    assert_eq!(codes(ir(json!([many("value", 10)]))), [DECIDE_MANY_PORTS]);
    assert!(codes(ir(json!([many("value", 9)]))).is_empty());
    // 確からしさで分けるなら出口は 2 つなので警告しない。
    assert!(codes(ir(json!([many("confidence", 30)]))).is_empty());
}
