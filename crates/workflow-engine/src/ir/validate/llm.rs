//! llm.invoke の保存を止めない警告（GAP-13・行数ゲート対応で分離）。
//!
//! - 出力の形（`output_schema`）を書いたのに、上限トークン（`max_tokens`）が小さい
//!   （[`OUTPUT_SCHEMA_FEW_TOKENS`]）。思考も上限に数えるモデル（Haiku 5.5 など）では、JSON の途中で切れて
//!   `llm_output_truncated` になりやすい。上限を書いていない（gateway の既定）・値式で書いたものは見ない。

use serde_json::Value;

use super::ValidationError;
use crate::ir::expr::ValueExpr;
use crate::ir::params::LlmInvokeParams;
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

/// 警告のコード: 出力の形を書いた llm.invoke の上限トークンが [`MIN_STRUCTURED_MAX_TOKENS`] 未満。
pub const OUTPUT_SCHEMA_FEW_TOKENS: &str = "ir.output_schema_few_tokens";

/// 出力の形を書いたときに勧める上限トークンの下限。
pub const MIN_STRUCTURED_MAX_TOKENS: u64 = 1024;

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
