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
/// - IR 全体の形の誤り（ノードに紐づかないもの）: ノードとエッジの形。ただし入力項目の誤り（位置が `/input_schema`
///   の下のもの・pattern の数や長さの上限を含む）には添えない（ノードとエッジの形では直せない）。
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
            None if under_input_schema(e.path.as_deref()) => {}
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
    let mut omitted: Vec<&str> = Vec::new();
    for nt in types {
        if let Some(shape) = params_shape(nt) {
            let line = format!(
                "{} の params の形（? は省略可）: {}\n",
                nt.as_str(),
                shape.render()
            );
            // 合計の上限を超える種は形を添えず、名前だけを挙げる（観測を膨らませない・HD-2）。
            if out.chars().count() + line.chars().count() > MAX_HINTS_CHARS {
                omitted.push(nt.as_str());
                continue;
            }
            out.push_str(&line);
        }
    }
    if !omitted.is_empty() {
        let _ = writeln!(
            out,
            "（長くなるため {} の params の形は省きました。上の誤りを直してから、もう一度保存して確かめてください）",
            omitted.join(", ")
        );
    }
    out
}

/// 補足の合計の文字数の上限（形の 1 行がこれを超えるなら、その種の形は添えない・HD-2）。
const MAX_HINTS_CHARS: usize = 4_000;

/// 位置（ブロックの中の位置）がブロックの設定（params）の下か。
fn under_params(path: Option<&str>) -> bool {
    path.is_some_and(|p| p == "/params" || p.starts_with("/params/"))
}

/// 位置（IR の中の位置）が入力項目（input_schema）の下か。
fn under_input_schema(path: Option<&str>) -> bool {
    path.is_some_and(|p| p == "/input_schema" || p.starts_with("/input_schema/"))
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

    /// 入力項目の誤り（位置が `/input_schema` の下）には、ノードとエッジの形を添えない。保存の口（V1c）が返す
    /// pattern の数の上限の誤りでも同じ（BUG-222 で増えた誤り）。
    #[test]
    fn input_schema_errors_get_no_ir_shape() {
        let ir = embedded(EXAMPLE_SUMMARIZE);
        for path in ["/input_schema", "/input_schema/properties/memo/pattern"] {
            let e = ValidationError::new("ir.schema_violation", "x").at_path(path);
            assert_eq!(validation_hints(&[e], &ir), "", "{path}");
        }
        // 入力項目と全体の誤りが並べば、全体の誤りのぶんだけ形を添える。
        let both = [
            ValidationError::new("ir.schema_violation", "x").at_path("/input_schema"),
            ValidationError::new("ir.schema_violation", "x").at_path("/input_schemax"),
        ];
        assert!(validation_hints(&both, &ir).starts_with("IR の形: "));

        let properties: serde_json::Map<String, Value> = (0..60)
            .map(|i| {
                (
                    format!("f{i}"),
                    serde_json::json!({ "type": "string", "pattern": "^a$" }),
                )
            })
            .collect();
        let ir = serde_json::json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "input_schema": { "type": "object", "properties": properties },
            "nodes": [{ "id": "done", "type": "workflow.reply", "params": { "format": "text", "value": "受け付けました" } }],
            "edges": []
        });
        let errors =
            workflow_engine::validate(&ir, &workflow_engine::Catalog::default()).unwrap_err();
        assert!(
            errors.iter().any(|e| e.code == "ir.schema_violation"
                && e.node_id.is_none()
                && e.path
                    .as_deref()
                    .is_some_and(|p| p.starts_with("/input_schema"))),
            "{errors:?}"
        );
        let h = validation_hints(&errors, &ir);
        assert!(!h.contains(EDGE_SHAPE), "{h}");
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

    /// params の形の補足は合計 4,000 字までにし、入らない種は名前だけを挙げる（T12・HD-2）。
    #[test]
    fn params_shapes_are_capped_in_total() {
        // 契約のある種を全部並べる（形の合計は上限を超える）。
        let types: Vec<&str> = NodeType::ALL
            .iter()
            .filter(|nt| params_shape(**nt).is_some())
            .map(|nt| nt.as_str())
            .collect();
        let nodes: Vec<Value> = types
            .iter()
            .enumerate()
            .map(|(i, t)| serde_json::json!({ "id": format!("n{i}"), "type": t, "params": {} }))
            .collect();
        let ir = serde_json::json!({ "nodes": nodes });
        let errors: Vec<ValidationError> = (0..types.len())
            .map(|i| {
                ValidationError::new("ir.schema_violation", "x")
                    .at_node(format!("n{i}"))
                    .at_path("/params")
            })
            .collect();
        let h = validation_hints(&errors, &ir);
        let (shapes, note) = h
            .rsplit_once("（長くなるため ")
            .unwrap_or_else(|| panic!("{h}"));
        assert!(
            shapes.chars().count() <= MAX_HINTS_CHARS,
            "{}",
            shapes.chars().count()
        );
        let (listed, _) = note
            .split_once(" の params の形は省きました")
            .unwrap_or_else(|| panic!("{note}"));
        let omitted: Vec<&str> = listed.split(", ").collect();
        assert!(!omitted.is_empty());
        // 省いた種は形を添えず、名前だけを挙げる（どの種も、形か名前のどちらか一方で 1 回だけ出る）。
        for t in &types {
            let shown = shapes.contains(&format!("{t} の params の形"));
            assert_ne!(shown, omitted.contains(t), "{t}: {h}");
        }
    }
}
