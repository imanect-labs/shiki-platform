//! llm.invoke の保存を止めない警告（GAP-13・行数ゲート対応で分離）。
//!
//! - 出力の形（`output_schema`）を書いたのに、上限トークン（`max_tokens`）が小さい
//!   （[`OUTPUT_SCHEMA_FEW_TOKENS`]）。思考も上限に数えるモデル（Haiku 5.5 など）では、JSON の途中で切れて
//!   `llm_output_truncated` になりやすい。上限を書いていない（gateway の既定）・値式で書いたものは見ない。
//! - 書いた思考の深さ（`effort`）が使われない（[`EFFORT_IGNORED`]・D-75）。組織がモデルの選択を禁じているときと、
//!   モデルを省略して組織の既定の Auto で動くときは、実行時に組織の設定に従う（チャットと揃える・llm-gateway の
//!   `node_effort_applies` と同じ規則）。組織の設定は [`Catalog`] から読む。

use serde_json::Value;

use super::{Catalog, ValidationError};
use crate::ir::expr::ValueExpr;
use crate::ir::params::LlmInvokeParams;
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

/// 警告のコード: 出力の形を書いた llm.invoke の上限トークンが [`MIN_STRUCTURED_MAX_TOKENS`] 未満。
pub const OUTPUT_SCHEMA_FEW_TOKENS: &str = "ir.output_schema_few_tokens";

/// 出力の形を書いたときに勧める上限トークンの下限。
pub const MIN_STRUCTURED_MAX_TOKENS: u64 = 1024;

/// 警告のコード: 書いた思考の深さが実行時に使われない（組織の設定に従う・D-75）。
pub const EFFORT_IGNORED: &str = "ir.effort_ignored";

/// 組織の AI 設定によって出る llm.invoke の警告（[`EFFORT_IGNORED`]）。
pub(super) fn org_warnings(ir: &WorkflowIr, catalog: &Catalog) -> Vec<ValidationError> {
    let mut out = Vec::new();
    for node in ir
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::LlmInvoke.as_str())
    {
        let Ok(p) = serde_json::from_value::<LlmInvokeParams>(node.params.clone()) else {
            continue; // 契約に合わないものは V1b が報告する。
        };
        if p.effort.is_none() {
            continue;
        }
        let omitted = p.model.as_deref().is_none_or(str::is_empty);
        let message = if catalog.model_lock.is_some() {
            "この組織ではモデルを選べないため、思考の深さ（effort）は使われず、組織の設定で動きます。\
             「組織の既定に従う」に戻してください"
        } else if omitted && catalog.default_is_auto {
            "モデルを省略すると組織の既定（Auto）で動き、思考の深さも Auto の設定に従うため、この思考の深さ\
             （effort）は使われません。思考の深さを決めるときはモデルを選んでください"
        } else {
            continue;
        };
        out.push(
            ValidationError::new(EFFORT_IGNORED, message)
                .at_node(&node.id)
                .at_path("/params/effort"),
        );
    }
    out
}

/// llm.invoke の警告。
pub(super) fn warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = Vec::new();
    for node in ir
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::LlmInvoke.as_str())
    {
        let Ok(p) = serde_json::from_value::<LlmInvokeParams>(node.params.clone()) else {
            continue; // 契約に合わないものは V1b が報告する。
        };
        if p.output_schema.is_none() {
            continue;
        }
        let Some(ValueExpr::Literal(Value::Number(n))) = &p.max_tokens else {
            continue;
        };
        if n.as_u64().is_some_and(|t| t < MIN_STRUCTURED_MAX_TOKENS) {
            out.push(
                ValidationError::new(
                    OUTPUT_SCHEMA_FEW_TOKENS,
                    format!(
                        "出力の形（output_schema）を書いた AI の上限トークン（max_tokens）が {n} です。\
                         思考の分も上限に数えるため、JSON の途中で切れて失敗しやすくなります\
                         （{MIN_STRUCTURED_MAX_TOKENS} 以上を勧めます）"
                    ),
                )
                .at_node(&node.id)
                .at_path("/params/max_tokens"),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]
    use serde_json::json;

    use super::*;

    fn ir(params: Value) -> WorkflowIr {
        WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ai", "type": "llm.invoke", "params": params }], "edges": []
        }))
        .unwrap()
    }

    /// 書いた思考の深さが使われないとき（選択の禁止・省略して Auto）だけ警告する（D-75）。
    #[test]
    fn warns_when_the_effort_follows_the_organization() {
        let codes = |params: Value, catalog: &Catalog| -> Vec<(String, Option<String>)> {
            org_warnings(&ir(params), catalog)
                .into_iter()
                .map(|w| (w.code, w.path))
                .collect()
        };
        let want = vec![(
            EFFORT_IGNORED.to_string(),
            Some("/params/effort".to_string()),
        )];
        let auto = Catalog {
            default_is_auto: true,
            ..Catalog::default()
        };
        let locked = Catalog {
            model_lock: Some("a".into()),
            ..Catalog::default()
        };
        // 省略して Auto で動く（空文字も省略）・選択の禁止（省略でも明示でも）。
        assert_eq!(
            codes(json!({ "prompt": "p", "effort": "low" }), &auto),
            want
        );
        assert_eq!(
            codes(
                json!({ "prompt": "p", "model": "", "effort": "low" }),
                &auto
            ),
            want
        );
        assert_eq!(
            codes(json!({ "prompt": "p", "effort": "high" }), &locked),
            want
        );
        assert_eq!(
            codes(
                json!({ "prompt": "p", "model": "a", "effort": "high" }),
                &locked
            ),
            want
        );
        // 使われるとき（Auto でもモデルを選んだ・既定が特定のモデル）と、書いていないときは出さない。
        for (params, catalog) in [
            (
                json!({ "prompt": "p", "model": "a", "effort": "low" }),
                &auto,
            ),
            (
                json!({ "prompt": "p", "effort": "low" }),
                &Catalog::default(),
            ),
            (json!({ "prompt": "p" }), &auto),
            (json!({ "prompt": "p" }), &locked),
        ] {
            assert!(codes(params.clone(), catalog).is_empty(), "{params}");
        }
    }

    #[test]
    fn warns_only_for_small_literal_limits_with_a_schema() {
        let schema = json!({ "type": "object" });
        let got = warnings(&ir(
            json!({ "prompt": "p", "output_schema": schema, "max_tokens": 512 }),
        ));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].code, OUTPUT_SCHEMA_FEW_TOKENS);
        assert_eq!(got[0].node_id.as_deref(), Some("ai"));
        assert_eq!(got[0].path.as_deref(), Some("/params/max_tokens"));
        for params in [
            json!({ "prompt": "p", "output_schema": schema, "max_tokens": 1024 }),
            json!({ "prompt": "p", "output_schema": schema }),
            json!({ "prompt": "p", "output_schema": schema,
                    "max_tokens": { "$from": "input", "path": "/n" } }),
            json!({ "prompt": "p", "max_tokens": 100 }),
        ] {
            assert!(warnings(&ir(params.clone())).is_empty(), "{params}");
        }
    }
}
