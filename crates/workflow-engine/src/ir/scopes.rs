//! 保存時の `declared_scopes` の補完（検証 C-P1-2）。
//!
//! エディタには `declared_scopes` の編集手段が無く、data ノード・ストレージノードを置いたワークフローは
//! `ir.missing_scope` で保存できなかった。ノードが要するスコープ（[`crate::vocab::required_scope`] が
//! 単一の定義）を保存の入口で `declared_scopes` へ足す。足すのは**ノードの構成から決まる分だけ**で、
//! 実効スコープは従来どおり「宣言 ∩ 委譲」（有効化で委譲が足りなければ 409 `missing_scopes`）なので、
//! 補完で権限が広がることはない。画面は保存後の IR の `declared_scopes` を表示する。

use serde_json::Value;

use crate::vocab::{required_scope, AgentTool, EventSource, NodeType, Scope};

/// データ変更（`data.*` の source）を待つ・きっかけにするなら `data.read` が要る（変更を委譲元・実行主体の
/// 見え方で読む・GAP-07）。トリガの `source` と control.wait の `params.source` を見る。承認（human.approval）も
/// 停止時に束縛した行を読み、判断の遷移を実行主体の見え方で受け取るので要る（GAP-08）。
fn needs_data_read(obj: &serde_json::Map<String, Value>) -> bool {
    let is_data = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .and_then(EventSource::parse)
            .is_some_and(EventSource::is_data)
    };
    let triggers = obj
        .get("triggers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|t| {
            t.get("kind").and_then(Value::as_str) == Some("event") && is_data(t.get("source"))
        });
    let waits = obj
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|n| match n.get("type").and_then(Value::as_str) {
            Some(t) if t == NodeType::ControlWait.as_str() => {
                is_data(n.get("params").and_then(|p| p.get("source")))
            }
            Some(t) => t == NodeType::HumanApproval.as_str(),
            None => false,
        });
    triggers || waits
}

/// agent.invoke の `allowed_tools` に明示した道具が要するスコープ（予約の道具・未知の名前は足さない）。
fn agent_tool_scopes(obj: &serde_json::Map<String, Value>) -> Vec<&'static str> {
    obj.get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|n| n.get("type").and_then(Value::as_str) == Some(NodeType::AgentInvoke.as_str()))
        .filter_map(|n| n.pointer("/params/allowed_tools").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(AgentTool::parse)
        .filter(|t| t.available())
        .map(|t| t.required_scope().as_str())
        .collect()
}

/// IR（JSON）の `declared_scopes` に、ノードが要するスコープを足した IR を返す（足した分を併せて返す）。
/// 形が不正な IR はそのまま返す（検証が誤りを報告する）。
pub fn complete_declared_scopes(ir: &Value) -> (Value, Vec<String>) {
    let mut out = ir.clone();
    let Some(obj) = out.as_object_mut() else {
        return (out, Vec::new());
    };
    let mut required: std::collections::BTreeSet<&'static str> = obj
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|n| n.get("type").and_then(Value::as_str))
        .filter_map(NodeType::parse)
        .filter(|nt| nt.available_stage_a())
        .filter_map(required_scope)
        .map(Scope::as_str)
        .collect();
    if needs_data_read(obj) {
        required.insert(Scope::DataRead.as_str());
    }
    // AI エージェントは**明示した道具の分だけ**足す（省略は道具なし・BUG-6）。
    required.extend(agent_tool_scopes(obj));
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

    /// データ変更のトリガ・wait(event) には data.read を足す（変更を読む見え方の権限・GAP-07）。
    #[test]
    fn data_change_triggers_and_waits_need_data_read() {
        for ir in [
            json!({ "name": "w", "triggers": [{ "kind": "event", "source": "data.record.created",
                     "scope": { "table": "t" } }], "nodes": [] }),
            json!({ "name": "w", "nodes": [{ "id": "w", "type": "control.wait",
                     "params": { "kind": "event", "source": "data.transition", "scope": { "table": "t" } } }] }),
        ] {
            let (out, added) = complete_declared_scopes(&ir);
            assert_eq!(added, vec!["data.read".to_string()], "{ir}");
            assert_eq!(out["declared_scopes"], json!(["data.read"]));
        }
        // 承認は人へのお知らせ（notify.send）と、束縛した行を読む data.read の両方（GAP-08・D-25）。
        let (out, added) = complete_declared_scopes(&json!({ "name": "w",
            "nodes": [{ "id": "a", "type": "human.approval", "params": {} }] }));
        assert_eq!(
            added,
            vec!["data.read".to_string(), "notify.send".to_string()]
        );
        assert_eq!(out["declared_scopes"], json!(["data.read", "notify.send"]));
        let (_, added) = complete_declared_scopes(&json!({ "name": "w",
            "triggers": [{ "kind": "event", "source": "storage.write", "scope": { "folder": "f" } }],
            "nodes": [] }));
        assert!(
            added.is_empty(),
            "ファイル保存のトリガは足さない（従来どおり）"
        );
    }

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

    /// AI エージェントは明示した道具の分だけ足す（省略・予約・未知の名前では足さない・BUG-6）。
    #[test]
    fn agent_tools_add_only_their_own_scopes() {
        let (out, added) = complete_declared_scopes(&json!({ "name": "w", "nodes": [
            { "id": "a", "type": "agent.invoke",
              "params": { "instruction": "x", "allowed_tools": ["rag.search", "doc.read", "web.fetch", "code.run", "data.query", "bogus"] } },
            { "id": "b", "type": "agent.invoke", "params": { "instruction": "y" } }
        ] }));
        assert_eq!(
            added,
            vec!["doc.read", "http.egress", "rag.query", "sandbox.exec"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            out["declared_scopes"],
            json!(["doc.read", "http.egress", "rag.query", "sandbox.exec"])
        );
        let (_, added) = complete_declared_scopes(&json!({ "name": "w", "nodes": [
            { "id": "b", "type": "agent.invoke", "params": { "instruction": "y" } }
        ] }));
        assert!(added.is_empty(), "道具を省略したエージェントは何も足さない");
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
