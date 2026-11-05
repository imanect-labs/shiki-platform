//! 検証エラーの補足（emit_workflow の誤りの観測に添える、LLM が自分で直すための材料・guide.rs から分割）。
//!
//! 形は単一定義から作る（ブロックと線の形は guide.rs の定数、ブロックの設定の形は typed 契約の ts-rs 宣言）。

use std::fmt::Write as _;

use serde_json::Value;
use workflow_engine::ir::params::params_shape;
use workflow_engine::vocab::NodeType;
use workflow_engine::ValidationError;

use super::{EDGE_SHAPE, NODE_SHAPE};

/// 検証エラーの補足（LLM が自分で直すための材料）。
///
/// - params の誤り（`ir.schema_violation` でノードに紐づくもの）: そのノード種の params の形。
/// - IR 全体の形の誤り（ノードに紐づかない `ir.schema_violation`）: ノードとエッジの形。
pub(in crate::workflow_tool) fn validation_hints(errors: &[ValidationError], ir: &Value) -> String {
    let node_type_of = |id: &str| -> Option<NodeType> {
        ir.get("nodes")?
            .as_array()?
            .iter()
            .find(|n| n.get("id").and_then(Value::as_str) == Some(id))?
            .get("type")?
            .as_str()
            .and_then(NodeType::parse)
    };
    let mut types: Vec<NodeType> = Vec::new();
    let mut whole_ir = false;
    for e in errors.iter().filter(|e| e.code == "ir.schema_violation") {
        match e.node_id.as_deref() {
            Some(id) => {
                if let Some(nt) = node_type_of(id) {
                    if !types.contains(&nt) {
                        types.push(nt);
                    }
                }
            }
            None => whole_ir = true,
        }
    }
    let mut out = String::new();
    if whole_ir {
        let _ = writeln!(
            out,
            "IR の形: nodes の要素 = {NODE_SHAPE}（? は省略可）、edges の要素 = {EDGE_SHAPE}（それ以外のキーは書けない）。"
        );
    }
    for nt in types {
        if let Some(shape) = params_shape(nt) {
            let _ = writeln!(
                out,
                "{} の params の形（? は省略可）: {}",
                nt.as_str(),
                shape.render()
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{example_json, EXAMPLE_SUMMARIZE};
    use super::*;

    fn embedded(raw: &str) -> Value {
        serde_json::from_str(&example_json(raw)).unwrap()
    }

    #[test]
    fn hints_add_the_params_shape_of_the_failing_node_type() {
        let ir = embedded(EXAMPLE_SUMMARIZE);
        let errors = vec![
            ValidationError::new(
                "ir.schema_violation",
                "params が不正です: 項目 'prompt' がありません",
            )
            .at_node("summarize")
            .at_path("/params"),
            ValidationError::new("ir.bad_ref", "x").at_node("save_summary"),
        ];
        let h = validation_hints(&errors, &ir);
        assert!(
            h.contains(
                "llm.invoke の params の形（? は省略可）: { model?: string, prompt: ValueExpr"
            ),
            "{h}"
        );
        assert!(
            !h.contains("storage.write"),
            "schema_violation 以外は補足しない: {h}"
        );
        let whole = vec![ValidationError::new(
            "ir.schema_violation",
            "unknown field `port`",
        )];
        let h = validation_hints(&whole, &ir);
        assert!(h.contains(&format!("edges の要素 = {EDGE_SHAPE}")), "{h}");
    }
}
