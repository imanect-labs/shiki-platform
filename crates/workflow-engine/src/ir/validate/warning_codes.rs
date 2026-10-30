//! 保存を止めない警告のコードの一覧（web へは `export-workflow-ts` が定数として書き出す・BUG-48）。
//!
//! 画面は警告をコードで出し分ける（条件の意味論の切替だけなら「条件」と呼ぶ・必須の文字の欄の空は欄の下に
//! 出す）。web が同じ文字を手書きの定数で持つと、コードを変えたときに黙って外れる（codegen が正）。
//! [`super::save_warnings`] が出しうるコードは、すべてこの表に載せる（下のテストが確かめる）。

use super::{
    APPROVAL_TIMEOUTS_EXCEED_RUN, DECIDE_LOW_CONFIDENCE, DECIDE_MANY_PORTS, DECIDE_UNPINNED_MODEL,
    EFFORT_IGNORED, EMPTY_REGION, OUTPUT_SCHEMA_FEW_TOKENS, OVERLAPPING_DATA_TRIGGERS,
    REPLY_WITHOUT_INTERACTIVE, REQUIRED_TEXT_EMPTY, SEMANTICS_CHANGED, TEMPLATE_ESCAPED_VAR,
    TEMPLATE_UNCLOSED_BRACE, UNCONNECTED_NODE, WAIT_FILTER_ALIAS,
};

/// 定数の名前と値の組の表を作る（名前は Rust の定数の名前をそのまま使い、別の綴りを持たない）。
macro_rules! named {
    ($($name:ident),+ $(,)?) => {
        &[$((stringify!($name), $name)),+]
    };
}

/// 保存を止めない警告のコード（`(定数の名前, コード)`・[`super::save_warnings`] が出すものすべて）。
pub const SAVE_WARNING_CODES: &[(&str, &str)] = named![
    SEMANTICS_CHANGED,
    TEMPLATE_ESCAPED_VAR,
    TEMPLATE_UNCLOSED_BRACE,
    REQUIRED_TEXT_EMPTY,
    EMPTY_REGION,
    OVERLAPPING_DATA_TRIGGERS,
    WAIT_FILTER_ALIAS,
    APPROVAL_TIMEOUTS_EXCEED_RUN,
    REPLY_WITHOUT_INTERACTIVE,
    OUTPUT_SCHEMA_FEW_TOKENS,
    EFFORT_IGNORED,
    DECIDE_LOW_CONFIDENCE,
    DECIDE_MANY_PORTS,
    DECIDE_UNPINNED_MODEL,
    UNCONNECTED_NODE,
];

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]
    use std::collections::BTreeSet;

    use serde_json::{json, Value};

    use super::*;
    use crate::ir::validate::{save_warnings, Catalog};
    use crate::ir::WorkflowIr;

    const TABLE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";

    fn ir(triggers: Value, nodes: Value, edges: Value) -> WorkflowIr {
        WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "declared_scopes": [],
            "triggers": triggers, "nodes": nodes, "edges": edges
        }))
        .unwrap()
    }

    fn interactive() -> Value {
        json!([{ "kind": "interactive" }])
    }

    fn script(input: Value) -> Value {
        json!([{ "id": "s", "type": "script.run", "params": {
            "source": { "inline": "function main(i){ return i; }" }, "input": input } }])
    }

    fn approval(id: &str) -> Value {
        json!({ "id": id, "type": "human.approval", "params": {
            "title": "承認",
            "record": { "table": TABLE, "id": { "$from": "trigger", "path": "/record_id" } },
            "assignees": [{ "field": "approver" }],
            "approve": { "to": "approved" },
            "reject": { "to": "rejected" },
            "timeout_sec": 172_800, "on_timeout": "continue" } })
    }

    /// 値で振り分ける判定（しきい値・選択肢の数・モデルを変えて警告を起こす）。
    fn decide(min_confidence: &str, options: usize, model: &str) -> Value {
        let options: Vec<Value> = (0..options)
            .map(|i| json!({ "key": format!("o{i}") }))
            .collect();
        json!({ "id": "d", "type": "ai.decide", "params": {
            "model": model, "state": "本文",
            "questions": [{ "key": "kind", "type": "choice", "instructions": "種類", "options": options }],
            "route": { "question": "kind",
                       "min_confidence": min_confidence.parse::<f64>().unwrap() } } })
    }

    /// 各警告を 1 つずつ起こす IR（組織の設定で出る警告は、その設定のカタログと組にする・それ以外は既定）。
    fn samples() -> Vec<(&'static str, WorkflowIr, Catalog)> {
        let mut out: Vec<(&'static str, WorkflowIr, Catalog)> = ir_samples()
            .into_iter()
            .map(|(code, ir)| (code, ir, Catalog::default()))
            .collect();
        out.push((
            EFFORT_IGNORED,
            ir(
                interactive(),
                json!([{ "id": "ai", "type": "llm.invoke", "params": {
                    "prompt": "分類して", "effort": "low" } }]),
                json!([]),
            ),
            Catalog {
                default_is_auto: true,
                ..Catalog::default()
            },
        ));
        out
    }

    /// IR だけで起きる警告の例。
    fn ir_samples() -> Vec<(&'static str, WorkflowIr)> {
        vec![
            (
                SEMANTICS_CHANGED,
                ir(
                    interactive(),
                    json!([{ "id": "b", "type": "control.branch", "params": { "condition": {
                        "cmp": { "left": ["a", "b"], "op": "contains", "right": "a" } } } }]),
                    json!([]),
                ),
            ),
            (
                TEMPLATE_ESCAPED_VAR,
                ir(
                    interactive(),
                    script(json!({ "$template": "{{t}}", "vars": { "t": "x" } })),
                    json!([]),
                ),
            ),
            (
                TEMPLATE_UNCLOSED_BRACE,
                ir(
                    interactive(),
                    script(json!({ "$template": "a { b", "vars": {} })),
                    json!([]),
                ),
            ),
            (
                REQUIRED_TEXT_EMPTY,
                ir(
                    interactive(),
                    json!([{ "id": "ask", "type": "llm.invoke", "params": { "prompt": "" } }]),
                    json!([]),
                ),
            ),
            (
                EMPTY_REGION,
                ir(
                    interactive(),
                    json!([{ "id": "each_row", "type": "control.map",
                             "params": { "items": { "$from": "input", "path": "/rows" } } }]),
                    json!([]),
                ),
            ),
            (
                OVERLAPPING_DATA_TRIGGERS,
                ir(
                    json!([
                        { "kind": "event", "source": "data.record.updated", "scope": { "table": TABLE } },
                        { "kind": "event", "source": "data.transition", "scope": { "table": TABLE } }
                    ]),
                    json!([]),
                    json!([]),
                ),
            ),
            (
                WAIT_FILTER_ALIAS,
                ir(
                    interactive(),
                    json!([{ "id": "w", "type": "control.wait", "params": { "kind": "event",
                        "filter": { "cmp": { "left": { "$from": "trigger", "path": "/x" },
                                             "op": "eq", "right": "a" } } } }]),
                    json!([]),
                ),
            ),
            (
                APPROVAL_TIMEOUTS_EXCEED_RUN,
                ir(
                    interactive(),
                    json!([approval("first"), approval("second")]),
                    json!([{ "from": "first", "from_port": "approved", "to": "second" }]),
                ),
            ),
            (
                OUTPUT_SCHEMA_FEW_TOKENS,
                ir(
                    interactive(),
                    json!([{ "id": "ai", "type": "llm.invoke", "params": {
                        "prompt": "分類して", "max_tokens": 256,
                        "output_schema": { "type": "object" } } }]),
                    json!([]),
                ),
            ),
            (
                DECIDE_LOW_CONFIDENCE,
                ir(
                    interactive(),
                    json!([decide("0.55", 2, "decide-stub")]),
                    json!([]),
                ),
            ),
            (
                DECIDE_MANY_PORTS,
                ir(
                    interactive(),
                    json!([decide("0.8", 10, "decide-stub")]),
                    json!([]),
                ),
            ),
            (
                DECIDE_UNPINNED_MODEL,
                ir(
                    interactive(),
                    json!([decide("0.8", 2, "decide-latest")]),
                    json!([]),
                ),
            ),
            (
                UNCONNECTED_NODE,
                ir(
                    interactive(),
                    json!([
                        { "id": "a", "type": "storage.read", "params": {} },
                        { "id": "dropped", "type": "storage.read", "params": {} }
                    ]),
                    json!([]),
                ),
            ),
            (
                REPLY_WITHOUT_INTERACTIVE,
                ir(
                    json!([{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]),
                    json!([{ "id": "r", "type": "workflow.reply",
                             "params": { "format": "text", "value": "x" } }]),
                    json!([]),
                ),
            ),
        ]
    }

    /// 表は保存時の警告のコードと過不足なく一致する（警告を足したら表にも足す・web はこの表から生成する）。
    #[test]
    fn the_table_lists_every_save_warning() {
        let samples = samples();
        for (code, ir, catalog) in &samples {
            let got: Vec<String> = save_warnings(ir, catalog)
                .into_iter()
                .map(|w| w.code)
                .collect();
            assert!(got.iter().any(|c| c == code), "{code} が出ない: {got:?}");
            for c in &got {
                assert!(
                    SAVE_WARNING_CODES.iter().any(|(_, v)| v == c),
                    "{c} が SAVE_WARNING_CODES に無い"
                );
            }
        }
        let listed: BTreeSet<&str> = SAVE_WARNING_CODES.iter().map(|(_, v)| *v).collect();
        let raised: BTreeSet<&str> = samples.iter().map(|(c, _, _)| *c).collect();
        assert_eq!(listed, raised, "表に載せた警告を起こす例が足りない");
        assert_eq!(
            listed.len(),
            SAVE_WARNING_CODES.len(),
            "コードが重なっている"
        );
    }

    /// 名前は Rust の定数の名前（web はこの名前で引く）。
    #[test]
    fn names_are_the_rust_constant_names() {
        assert!(
            SAVE_WARNING_CODES.contains(&("SEMANTICS_CHANGED", "ir.condition_semantics_changed"))
        );
        assert!(SAVE_WARNING_CODES.contains(&("REQUIRED_TEXT_EMPTY", "ir.required_text_empty")));
    }
}
