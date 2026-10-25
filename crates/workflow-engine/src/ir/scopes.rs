//! 保存時の `declared_scopes` の補完（検証 C-P1-2）。
//!
//! エディタには `declared_scopes` の編集手段が無く、data ノード・ストレージノードを置いたワークフローは
//! `ir.missing_scope` で保存できなかった。ノードが要するスコープ（[`crate::vocab::required_scope`] が
//! 単一の定義）を保存の入口で `declared_scopes` へ足す。足すのは**ノードの構成から決まる分だけ**で、
//! 実効スコープは従来どおり「宣言 ∩ 委譲」（有効化で委譲が足りなければ 409 `missing_scopes`）なので、
//! 補完で権限が広がることはない。画面は保存後の IR の `declared_scopes` を表示する。

use serde_json::Value;

use crate::vocab::{required_scope, NodeType};

/// IR（JSON）の `declared_scopes` に、ノードが要するスコープを足した IR を返す（足した分を併せて返す）。
/// 形が不正な IR はそのまま返す（検証が誤りを報告する）。
pub fn complete_declared_scopes(ir: &Value) -> (Value, Vec<String>) {
    let mut out = ir.clone();
    let Some(obj) = out.as_object_mut() else {
        return (out, Vec::new());
    };
    let required: std::collections::BTreeSet<&'static str> = obj
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|n| n.get("type").and_then(Value::as_str))
        .filter_map(NodeType::parse)
        .filter(|nt| nt.available_stage_a())
        .filter_map(required_scope)
        .map(crate::vocab::Scope::as_str)
        .collect();
    let declared = obj
        .entry("declared_scopes")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(arr) = declared.as_array_mut() else {
        return (out, Vec::new());
    };
    let mut added = Vec::new();
    for s in required {
        if !arr.iter().any(|v| v.as_str() == Some(s)) {
            arr.push(Value::String(s.to_string()));
            added.push(s.to_string());
        }
    }
    (out, added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adds_scopes_required_by_nodes_once() {
        let ir = json!({
            "name": "w",
            "declared_scopes": ["storage.read"],
            "nodes": [
                {"id": "a", "type": "data.query"},
                {"id": "b", "type": "data.query"},
                {"id": "c", "type": "storage.read"},
                {"id": "d", "type": "control.branch"},
            ],
        });
        let (out, added) = complete_declared_scopes(&ir);
        assert_eq!(added, vec!["data.read".to_string()]);
        assert_eq!(out["declared_scopes"], json!(["storage.read", "data.read"]));
        // 2 回目は何も足さない（冪等）。
        let (again, added) = complete_declared_scopes(&out);
        assert!(added.is_empty());
        assert_eq!(again, out);
    }

    #[test]
    fn missing_declared_scopes_is_created_and_bad_shapes_are_left_alone() {
        let (out, added) = complete_declared_scopes(&json!({
            "nodes": [{"id": "a", "type": "data.record.create"}]
        }));
        assert_eq!(added, vec!["data.write".to_string()]);
        assert_eq!(out["declared_scopes"], json!(["data.write"]));
        let bad =
            json!({"declared_scopes": "x", "nodes": [{"id": "a", "type": "data.record.create"}]});
        assert_eq!(complete_declared_scopes(&bad).0, bad);
        assert_eq!(complete_declared_scopes(&json!([1])).0, json!([1]));
    }
}
