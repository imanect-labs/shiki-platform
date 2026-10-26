//! 結果を画面に返すノード（workflow.reply・GAP-09）の保存時検証（行数ゲート対応で分離）。
//!
//! - V2: 本体（`parent: null`）にだけ置ける（`ir.reply_in_map`）。要素ごとの結果は map の集約出力を表で返す。
//! - V7: 1 つの IR に置ける reply は [`MAX_REPLIES`] 個まで（`ir.limit_exceeded`）。
//! - リテラルの型: `value` がリテラルなら形と合うこと（`ir.reply_type`）。text・markdown は文字（数値・真偽も
//!   文字にできる）、table は配列、file_link はオブジェクトか配列。
//! - 警告: interactive のトリガが無い IR の reply は画面に出ない（ボタン・会話から起動した run だけに出る）。

use super::ValidationError;
use crate::ir::expr::ValueExpr;
use crate::ir::params::{ReplyFormat, WorkflowReplyParams};
use crate::ir::{Trigger, WorkflowIr};
use crate::vocab::NodeType;

/// 1 つの IR に置ける reply の上限。
pub const MAX_REPLIES: usize = 10;

/// 警告のコード: interactive のトリガが無い IR に結果を画面に返すノードがある。
pub const REPLY_WITHOUT_INTERACTIVE: &str = "ir.reply_without_interactive";

fn replies(ir: &WorkflowIr) -> impl Iterator<Item = &crate::ir::Node> {
    ir.nodes
        .iter()
        .filter(|n| n.node_type == NodeType::WorkflowReply.as_str())
}

/// V2: reply は本体にだけ置ける。
pub(super) fn v2(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    for node in replies(ir).filter(|n| n.parent.is_some()) {
        errors.push(
            ValidationError::new(
                "ir.reply_in_map",
                format!(
                    "ノード {} は繰り返しの中に置けません（要素ごとの結果は、繰り返しの出力を表で返してください）",
                    node.id
                ),
            )
            .at_node(&node.id),
        );
    }
}

/// V7: reply の数と、リテラルの値の型。
pub(super) fn v7(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    let count = replies(ir).count();
    if count > MAX_REPLIES {
        errors.push(ValidationError::new(
            "ir.limit_exceeded",
            format!("結果を画面に返すノードは {MAX_REPLIES} 個までです（{count} 個あります）"),
        ));
    }
    for node in replies(ir) {
        let Ok(p) = serde_json::from_value::<WorkflowReplyParams>(node.params.clone()) else {
            continue; // 契約に合わないものは V1b が報告する。
        };
        let ValueExpr::Literal(v) = &p.value else {
            continue;
        };
        let ok = match p.format {
            ReplyFormat::Text | ReplyFormat::Markdown => {
                v.is_string() || v.is_number() || v.is_boolean() || v.is_null()
            }
            ReplyFormat::Table => v.is_array(),
            ReplyFormat::FileLink => v.is_object() || v.is_array(),
        };
        if !ok {
            let expected = match p.format {
                ReplyFormat::Text | ReplyFormat::Markdown => "文字",
                ReplyFormat::Table => "配列（1 要素が 1 行）",
                ReplyFormat::FileLink => "オブジェクトか配列",
            };
            errors.push(
                ValidationError::new(
                    "ir.reply_type",
                    format!("ノード {} の値は{expected}で書いてください", node.id),
                )
                .at_node(&node.id)
                .at_path("/params/value"),
            );
        }
    }
}

/// 警告: interactive のトリガが無いと、結果はどの画面にも出ない。
pub(super) fn warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    if ir
        .triggers
        .iter()
        .any(|t| matches!(t, Trigger::Interactive(_)))
    {
        return Vec::new();
    }
    replies(ir)
        .map(|n| {
            ValidationError::new(
                REPLY_WITHOUT_INTERACTIVE,
                "画面に返す結果は、ボタンや会話から起動したときだけ表示されます（このワークフローには手動で動かすきっかけがありません）",
            )
            .at_node(&n.id)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]
    use super::super::{save_warnings, validate, Catalog};
    use super::*;
    use serde_json::json;

    fn ir(nodes: serde_json::Value, triggers: serde_json::Value) -> serde_json::Value {
        json!({ "ir_version": 1, "name": "wf", "triggers": triggers, "nodes": nodes, "edges": [] })
    }

    #[test]
    fn reply_without_scopes_is_valid() {
        let v = ir(
            json!([{ "id": "done", "type": "workflow.reply",
                     "params": { "format": "text", "value": "受け付けました" } }]),
            json!([{ "kind": "interactive" }]),
        );
        let parsed = validate(&v, &Catalog::default()).unwrap();
        assert!(save_warnings(&parsed).is_empty());
    }

    #[test]
    fn reply_in_map_is_rejected() {
        let v = json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "m", "type": "control.map", "params": { "items": [1, 2] } },
                { "id": "r", "type": "workflow.reply", "parent": "m",
                  "params": { "format": "text", "value": "x" } }
            ],
            "edges": []
        });
        let errs = validate(&v, &Catalog::default()).unwrap_err();
        assert!(errs.iter().any(|e| e.code == "ir.reply_in_map"), "{errs:?}");
    }

    #[test]
    fn eleventh_reply_is_rejected() {
        let nodes: Vec<_> = (0..11)
            .map(|i| {
                json!({ "id": format!("r{i}"), "type": "workflow.reply",
                             "params": { "format": "text", "value": "x" } })
            })
            .collect();
        let errs = validate(
            &ir(json!(nodes), json!([{ "kind": "interactive" }])),
            &Catalog::default(),
        )
        .unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.code == "ir.limit_exceeded" && e.message.contains("10 個まで")),
            "{errs:?}"
        );
    }

    #[test]
    fn literal_value_must_match_the_format() {
        for (format, value, ok) in [
            ("text", json!(12), true),
            ("text", json!({ "a": 1 }), false),
            ("table", json!([{ "a": 1 }]), true),
            ("table", json!("x"), false),
            ("file_link", json!("x"), false),
        ] {
            let v = ir(
                json!([{ "id": "r", "type": "workflow.reply",
                         "params": { "format": format, "value": value } }]),
                json!([{ "kind": "interactive" }]),
            );
            let res = validate(&v, &Catalog::default());
            assert_eq!(res.is_ok(), ok, "{format} {value}: {res:?}");
            if let Err(errs) = res {
                let e = errs.iter().find(|e| e.code == "ir.reply_type").unwrap();
                assert_eq!(e.path.as_deref(), Some("/params/value"));
            }
        }
    }

    #[test]
    fn warns_when_nothing_starts_it_interactively() {
        let v = ir(
            json!([{ "id": "r", "type": "workflow.reply",
                     "params": { "format": "text", "value": "x" } }]),
            json!([{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]),
        );
        let parsed = validate(&v, &Catalog::default()).unwrap();
        let w = save_warnings(&parsed);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].code, REPLY_WITHOUT_INTERACTIVE);
        assert_eq!(w[0].node_id.as_deref(), Some("r"));
    }
}
