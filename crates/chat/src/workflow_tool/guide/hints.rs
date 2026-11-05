//! 検証エラーの補足（emit_workflow の誤りの観測に添える、LLM が自分で直すための材料・guide.rs から分割）。
//!
//! 形は単一定義から作る（ブロックと線の形は guide.rs の定数、ブロックの設定の形は typed 契約の ts-rs 宣言）。

use std::fmt::Write as _;

use serde_json::Value;
use workflow_engine::ir::params::params_shape;
use workflow_engine::vocab::NodeType;
use workflow_engine::ValidationError;

use super::{EDGE_SHAPE, NODE_SHAPE};

/// 検証エラーの補足（LLM が自分で直すための材料）。どれも `ir.schema_violation` だけに添える。
///
/// - ブロックの設定の誤り（ノードに紐づき、位置が `/params` の下のもの）: そのノード種の params の形。
/// - ブロックの設定の外の誤り（ノードに紐づき、位置が `/colour`・`/retry/…` など params の外のもの）: ノードの形
///   （params の形を添えても直せない・BUG-220）。
/// - IR 全体の形の誤り（ノードに紐づかないもの）: ノードとエッジの形。
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
    let (mut whole_ir, mut node_shape) = (false, false);
    for e in errors.iter().filter(|e| e.code == "ir.schema_violation") {
        match e.node_id.as_deref() {
            Some(id) if under_params(e.path.as_deref()) => {
                if let Some(nt) = node_type_of(id) {
                    if !types.contains(&nt) {
                        types.push(nt);
                    }
                }
            }
            Some(_) => node_shape = true,
            None => whole_ir = true,
        }
    }
    let mut out = String::new();
    if whole_ir {
        let _ = writeln!(
            out,
            "IR の形: nodes の要素 = {NODE_SHAPE}（? は省略可）、edges の要素 = {EDGE_SHAPE}（それ以外のキーは書けない）。"
        );
    } else if node_shape {
        let _ = writeln!(
            out,
            "ブロックの形: nodes の要素 = {NODE_SHAPE}（? は省略可・それ以外のキーは書けない）。"
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

/// 位置（ブロックの中の位置）がブロックの設定（params）の下か。
fn under_params(path: Option<&str>) -> bool {
    path.is_some_and(|p| p == "/params" || p.starts_with("/params/"))
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

    /// ブロックの設定の外の型の誤り（`/colour`・`/retry/…`・位置の無いもの）には、params の形でなくブロックの形を
    /// 添える（BUG-220）。設定の誤りと並んでいれば両方を添え、全体の誤りと並んでいればブロックの形を重ねない。
    #[test]
    fn errors_outside_the_params_get_the_node_shape() {
        let ir = embedded(EXAMPLE_SUMMARIZE);
        let params_hint = "llm.invoke の params の形";
        let node_hint = format!("nodes の要素 = {NODE_SHAPE}");
        let at = |path: Option<&str>| {
            let e = ValidationError::new("ir.schema_violation", "x").at_node("summarize");
            match path {
                Some(p) => e.at_path(p),
                None => e,
            }
        };
        for path in [
            Some("/colour"),
            Some("/retry/max_attempts"),
            Some("/paramsx"),
            None,
        ] {
            let h = validation_hints(&[at(path)], &ir);
            assert!(h.starts_with("ブロックの形: "), "{path:?}: {h}");
            assert!(h.contains(&node_hint), "{path:?}: {h}");
            assert!(!h.contains(params_hint), "{path:?}: {h}");
            assert!(!h.contains(EDGE_SHAPE), "{path:?}: {h}");
        }
        for path in ["/params", "/params/prompt"] {
            let h = validation_hints(&[at(Some(path))], &ir);
            assert!(h.contains(params_hint), "{path}: {h}");
            assert!(!h.contains(&node_hint), "{path}: {h}");
        }
        let both = validation_hints(&[at(Some("/colour")), at(Some("/params/prompt"))], &ir);
        assert!(
            both.contains(&node_hint) && both.contains(params_hint),
            "{both}"
        );
        let whole = ValidationError::new("ir.schema_violation", "x").at_path("/surprise");
        let h = validation_hints(&[at(Some("/colour")), whole], &ir);
        assert_eq!(h.matches(NODE_SHAPE).count(), 1, "{h}");
        assert!(h.starts_with("IR の形: "), "{h}");
    }

    /// 保存の口（V1）が返すブロックの項目の誤りにも、ブロックの形が添わる（後退していないこと）。
    #[test]
    fn real_node_field_errors_get_the_node_shape() {
        let ir = serde_json::json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "read_file", "type": "storage.read",
                        "params": { "file": "x" }, "colour": "red", "retry": { "max_attempts": "3" } }],
            "edges": []
        });
        let errors =
            workflow_engine::validate(&ir, &workflow_engine::Catalog::default()).unwrap_err();
        assert!(
            errors
                .iter()
                .all(|e| e.node_id.as_deref() == Some("read_file")
                    && !e.path.as_deref().unwrap_or_default().starts_with("/params")),
            "{errors:?}"
        );
        let h = validation_hints(&errors, &ir);
        assert!(h.contains(&format!("nodes の要素 = {NODE_SHAPE}")), "{h}");
        assert!(!h.contains("storage.read の params の形"), "{h}");
    }
}
