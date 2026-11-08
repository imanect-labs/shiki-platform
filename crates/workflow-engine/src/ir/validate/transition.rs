//! 状態を進める（`data.transition`）と、状態の変化のきっかけ（`data.transition` のイベントトリガ）の保存時検証
//! V4（BUG-105）。
//!
//! どちらも、テーブルに状態の進め方（FSM）が無いと動かない（ノードは実行のたびに失敗し、きっかけはいつまでも
//! 起きない）。テーブルは保存者に見えて FSM で管理され、状態の列を保存者が読めること。材料は承認（GAP-08）と
//! 同じ [`Catalog::tables`]（API 層が IR のテーブルを保存者の見え方で引いて詰める・`None` なら照合しない）。
//! 見えないテーブル・FSM でないテーブル・状態の列が読めないテーブルは同じ code と文言にする（存在の手がかりを
//! 出さない）。テーブルを参照（`$from`）で決めるノードは、実行時まで分からないので照合しない。

use super::{Catalog, ValidationError};
use crate::ir::{Trigger, WorkflowIr};
use crate::vocab::{EventSource, NodeType};

/// 誤りのコード（承認と同じ・見えない・FSM でない・状態の列を読めないテーブル）。
const UNKNOWN_TABLE: &str = "ir.unknown_table";

/// テーブルの id の文字列を、材料の鍵の形（小文字・ハイフンつき）へ。UUID でなければ None（V1・V3 が報告する）。
fn table_key(raw: &str) -> Option<String> {
    uuid::Uuid::parse_str(raw)
        .ok()
        .map(|u| u.hyphenated().to_string())
}

/// 材料にあり、FSM の状態を保存者が読めるテーブルか。
fn has_states(
    catalog_tables: &std::collections::BTreeMap<String, super::CatalogTable>,
    key: &str,
) -> bool {
    catalog_tables.get(key).is_some_and(|t| t.states.is_some())
}

/// V4: 状態を進めるテーブルと、状態の変化のきっかけのテーブル。
pub(super) fn v4(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    let Some(tables) = catalog.tables.as_ref() else {
        return;
    };
    for node in ir
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::DataTransition.as_str())
    {
        let Some(key) = node
            .params
            .get("table")
            .and_then(serde_json::Value::as_str)
            .and_then(table_key)
        else {
            continue;
        };
        if !has_states(tables, &key) {
            errors.push(
                ValidationError::new(
                    UNKNOWN_TABLE,
                    "「テーブル」に選んだテーブルが見つからないか、状態の進め方がありません。状態の進め方（例: 申請中 → 承認済み）を決めてあるテーブルを選んでください。",
                )
                .at_node(&node.id)
                .at_path("/params/table"),
            );
        }
    }
    for (i, t) in ir.triggers.iter().enumerate() {
        let Trigger::Event(e) = t else { continue };
        if EventSource::parse(&e.source) != Some(EventSource::DataTransition) {
            continue;
        }
        let Some(key) = e
            .scope
            .get("table")
            .and_then(serde_json::Value::as_str)
            .and_then(table_key)
        else {
            continue; // scope の形の誤りは V3 が報告する。
        };
        if !has_states(tables, &key) {
            errors.push(
                ValidationError::new(
                    UNKNOWN_TABLE,
                    "きっかけ: 「対象のテーブル」に選んだテーブルが見つからないか、状態の進め方がありません。状態の進め方（例: 申請中 → 承認済み）を決めてあるテーブルを選んでください。",
                )
                .at_path(format!("/triggers/{i}/scope/table")),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::ir::validate::CatalogTable;

    const FSM: &str = "00000000-0000-0000-0000-0000000000f1";
    const PLAIN: &str = "00000000-0000-0000-0000-0000000000a2";
    const HIDDEN: &str = "00000000-0000-0000-0000-0000000000b3";

    fn catalog() -> Catalog {
        let mut tables = BTreeMap::new();
        tables.insert(
            FSM.to_string(),
            CatalogTable {
                states: Some(vec!["申請中".into(), "承認済み".into()]),
                status_field: Some("status".into()),
                fields: BTreeMap::new(),
                unique: std::collections::BTreeSet::new(),
                schema: None,
                fsm_initial: None,
            },
        );
        // 見えるが FSM で管理していない（状態の列を読めないテーブルも states が None）。
        tables.insert(PLAIN.to_string(), CatalogTable::default());
        Catalog {
            tables: Some(tables),
            ..Catalog::default()
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn ir(triggers: serde_json::Value, nodes: serde_json::Value) -> WorkflowIr {
        WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "triggers": triggers, "nodes": nodes, "edges": []
        }))
        .unwrap_or_else(|e| unreachable!("{e}"))
    }

    /// （コード・ノード id・path）の並び。
    type Located<'a> = (&'a str, Option<&'a str>, Option<&'a str>);

    fn located(errs: &[ValidationError]) -> Vec<Located<'_>> {
        errs.iter()
            .map(|e| (e.code.as_str(), e.node_id.as_deref(), e.path.as_deref()))
            .collect()
    }

    /// 状態の進め方のないテーブル・見えないテーブルは、ノードでもきっかけでも同じ code で止める。FSM のある
    /// テーブル（大文字の UUID で書いても）・参照で決めるテーブル・ほかのきっかけは止めない。
    #[test]
    fn transitions_need_a_table_with_a_state_flow() {
        let ir = ir(
            json!([
                { "kind": "event", "source": "data.transition", "scope": { "table": PLAIN } },
                { "kind": "event", "source": "data.transition", "scope": { "table": FSM } },
                { "kind": "event", "source": "data.record.created", "scope": { "table": PLAIN } }
            ]),
            json!([
                { "id": "plain", "type": "data.transition",
                  "params": { "table": PLAIN, "record": "r", "to": "承認済み" } },
                { "id": "hidden", "type": "data.transition",
                  "params": { "table": HIDDEN, "record": "r", "to": "承認済み" } },
                { "id": "ok", "type": "data.transition",
                  "params": { "table": FSM.to_uppercase(), "record": "r", "to": "承認済み" } },
                { "id": "dynamic", "type": "data.transition",
                  "params": { "table": { "$from": "input", "path": "/table" }, "record": "r", "to": "承認済み" } }
            ]),
        );
        let mut errors = Vec::new();
        v4(&ir, &catalog(), &mut errors);
        assert_eq!(
            located(&errors),
            vec![
                (UNKNOWN_TABLE, Some("plain"), Some("/params/table")),
                (UNKNOWN_TABLE, Some("hidden"), Some("/params/table")),
                (UNKNOWN_TABLE, None, Some("/triggers/0/scope/table")),
            ]
        );
        // 見えないテーブルと FSM のないテーブルは同じ文言（存在を明かさない）。
        assert_eq!(errors[0].message, errors[1].message);
        assert!(
            errors[2].message.starts_with("きっかけ: "),
            "{}",
            errors[2].message
        );

        // 材料が無い構成（AI 編集の説明文の検査など）は照合しない。
        let mut errors = Vec::new();
        v4(&ir, &Catalog::default(), &mut errors);
        assert!(errors.is_empty());
    }
}
