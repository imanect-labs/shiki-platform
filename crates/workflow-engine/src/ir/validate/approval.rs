//! 承認（human.approval・GAP-08）の保存時検証（行数ゲート対応で分離）。
//!
//! - V3: 束縛した行を読み、判断の遷移を実行主体の見え方で受け取るので `data.read` の宣言が要る。
//! - V4: テーブルは保存者に見えて FSM で管理され、状態の列を保存者が読めること（見えないテーブル・FSM でない
//!   テーブル・状態の列が読めないテーブルは同じ文言・存在の手がかりを出さない）。判断の行き先は FSM の状態、
//!   受信箱に出す列と担当者の列はテーブルの列。材料は API 層が保存者の見え方（owner でなければ読める列だけ）で
//!   引いて [`Catalog::tables`] に詰める（`None` なら照合しない）。読めない列は材料に入らないので、無い列と
//!   同じ誤りになる。文言も、無い列と読めない列のどちらとも言わない（「無いか、読めない列です」）。
//! - V7: 期限は `policies.run_timeout_sec` 未満（run の期限が先に来ると承認は時間切れの出口へ流れず、run ごと
//!   取り消される）。直列に並ぶ承認の期限の合計が run の期限以上なら警告する。

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use super::{Catalog, ValidationError};
use crate::ir::params::HumanApprovalParams;
use crate::ir::WorkflowIr;
use crate::vocab::{NodeType, Scope};

/// 警告のコード: 直列に並ぶ承認の期限の合計が run の期限（`policies.run_timeout_sec`）以上。
pub const APPROVAL_TIMEOUTS_EXCEED_RUN: &str = "ir.approval_timeouts_exceed_run";

/// 承認に使うテーブルの材料（保存者の見え方・API 層が詰める）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogTable {
    /// FSM の状態（FSM で管理しないテーブル・状態の列を保存者が読めないテーブルは `None`）。
    pub states: Option<Vec<String>>,
    /// 保存者に読める列の参照名 → 種別（data の `FieldType` の serde 名。例: `user_ref`）。
    pub fields: BTreeMap<String, String>,
}

/// 承認ノードと、その params（契約に合わないものは V1b が報告するので飛ばす）。
fn approvals(ir: &WorkflowIr) -> impl Iterator<Item = (&crate::ir::Node, HumanApprovalParams)> {
    ir.nodes
        .iter()
        .filter(|n| n.node_type == NodeType::HumanApproval.as_str())
        .filter_map(|n| {
            serde_json::from_value::<HumanApprovalParams>(n.params.clone())
                .ok()
                .map(|p| (n, p))
        })
}

/// V3: 承認は data.read を要する（停止時に行を読み、判断の遷移を実行主体の見え方で受け取る）。
pub(super) fn v3(ir: &WorkflowIr, declared: &BTreeSet<&str>, errors: &mut Vec<ValidationError>) {
    if declared.contains(Scope::DataRead.as_str()) {
        return;
    }
    for node in ir
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::HumanApproval.as_str())
    {
        errors.push(
            ValidationError::new(
                "ir.missing_scope",
                format!(
                    "ノード {} は scope data.read を要しますが declared_scopes に未宣言です",
                    node.id
                ),
            )
            .at_node(&node.id),
        );
    }
}

/// V4: テーブル・状態・列の照合（材料が無ければ照合しない）。
pub(super) fn v4(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    let Some(tables) = catalog.tables.as_ref() else {
        return;
    };
    for (node, p) in approvals(ir) {
        let err = |code: &str, msg: String, path: &str| {
            ValidationError::new(code, msg)
                .at_node(&node.id)
                .at_path(path.to_string())
        };
        let Some((table, states)) = tables
            .get(&p.record.table)
            .and_then(|t| t.states.as_ref().map(|s| (t, s)))
        else {
            errors.push(err(
                "ir.unknown_table",
                "承認するテーブルが見つからないか、状態の遷移（FSM）で管理されていないか、状態の列を読めません"
                    .into(),
                "/params/record/table",
            ));
            continue;
        };
        for (kind, choice) in p.choices() {
            if !states.contains(&choice.to) {
                // params の項目名は判断の語彙と同じ（approve / reject / return）。
                let path = format!("/params/{}/to", kind.as_str());
                errors.push(err(
                    "ir.unknown_state",
                    format!(
                        "状態「{}」はこのテーブルの状態にありません（使える状態: {}）",
                        choice.to,
                        states.join("、")
                    ),
                    &path,
                ));
            }
        }
        for (i, key) in p.show_fields().iter().enumerate() {
            if !table.fields.contains_key(key) {
                errors.push(err(
                    "ir.unknown_field",
                    format!("列 {key} はこのテーブルに無いか、読めない列です"),
                    &format!("/params/show_fields/{i}"),
                ));
            }
        }
        for (i, a) in p.assignees.iter().enumerate() {
            let Some(key) = &a.field else { continue };
            match table.fields.get(key).map(String::as_str) {
                Some("user_ref" | "role_ref") => {}
                Some(_) => errors.push(err(
                    "ir.bad_assignee",
                    format!("担当者の列 {key} はユーザーかロールの列ではありません"),
                    &format!("/params/assignees/{i}/field"),
                )),
                None => errors.push(err(
                    "ir.unknown_field",
                    format!("担当者の列 {key} はこのテーブルに無いか、読めない列です"),
                    &format!("/params/assignees/{i}/field"),
                )),
            }
        }
    }
}

/// V7: 承認の期限は run の期限未満。
pub(super) fn v7(ir: &WorkflowIr, errors: &mut Vec<ValidationError>) {
    let run = ir.policies.run_timeout_sec;
    for (node, p) in approvals(ir) {
        if p.timeout_sec.is_some_and(|t| t >= run) {
            errors.push(
                ValidationError::new(
                    "ir.limit_exceeded",
                    format!(
                        "承認の期限（{} 秒）は実行の期限（run_timeout_sec: {run} 秒）より短くしてください。\
                         実行の期限が先に来ると、承認は時間切れの出口へ流れずに取り消されます",
                        p.timeout_sec.unwrap_or_default()
                    ),
                )
                .at_node(&node.id)
                .at_path("/params/timeout_sec"),
            );
        }
    }
}

/// 警告: 直列に並ぶ承認の期限の合計（グラフの最長の道）が run の期限以上。
pub(super) fn warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let weight: HashMap<&str, u64> = approvals(ir)
        .filter_map(|(n, p)| p.timeout_sec.map(|t| (n.id.as_str(), u64::from(t))))
        .collect();
    if weight.len() < 2 {
        return Vec::new(); // 1 つだけなら V7 のエラーが同じことを言う。
    }
    let Some(longest) = longest_path(ir, &weight) else {
        return Vec::new(); // 循環は V2 が報告する。
    };
    let run = u64::from(ir.policies.run_timeout_sec);
    if longest < run {
        return Vec::new();
    }
    vec![ValidationError::new(
        APPROVAL_TIMEOUTS_EXCEED_RUN,
        format!(
            "続けて待つ承認の期限の合計（{longest} 秒）が実行の期限（run_timeout_sec: {run} 秒）以上です。\
             後の承認を待つ間に実行の期限が来ると、承認は取り消されます。実行の期限を延ばしてください"
        ),
    )
    .at_path("/policies/run_timeout_sec")]
}

/// DAG の最長の道の重み（ノードの重みの和・循環があれば `None`）。
fn longest_path(ir: &WorkflowIr, weight: &HashMap<&str, u64>) -> Option<u64> {
    let mut indeg: HashMap<&str, usize> = ir.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &ir.edges {
        if !indeg.contains_key(e.from.as_str()) || !indeg.contains_key(e.to.as_str()) {
            continue;
        }
        *indeg.entry(e.to.as_str()).or_default() += 1;
        out.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    let w = |id: &str| weight.get(id).copied().unwrap_or(0);
    let mut best: HashMap<&str, u64> = HashMap::new();
    let mut queue: VecDeque<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(id, _)| *id)
        .collect();
    for id in &queue {
        best.insert(id, w(id));
    }
    let mut seen = 0usize;
    while let Some(id) = queue.pop_front() {
        seen += 1;
        let here = best.get(id).copied().unwrap_or(0);
        for next in out.get(id).into_iter().flatten() {
            let cand = here + w(next);
            let e = best.entry(next).or_insert(0);
            *e = (*e).max(cand);
            let d = indeg.entry(next).or_default();
            *d -= 1;
            if *d == 0 {
                queue.push_back(next);
            }
        }
    }
    (seen == ir.nodes.len()).then(|| best.values().copied().max().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]
    use serde_json::json;

    use super::super::{save_warnings, validate, Catalog};
    use super::*;

    const TABLE: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";

    fn approval(id: &str, timeout: Option<u32>) -> serde_json::Value {
        let mut p = json!({
            "title": "経費申請の承認",
            "record": { "table": TABLE, "id": { "$from": "trigger", "path": "/record_id" } },
            "assignees": [{ "field": "approver" }],
            "approve": { "to": "approved" },
            "reject": { "to": "rejected", "comment": "required" },
            "show_fields": ["title", "amount"]
        });
        if let Some(t) = timeout {
            p["timeout_sec"] = json!(t);
            p["on_timeout"] = json!("continue");
        }
        json!({ "id": id, "type": "human.approval", "params": p })
    }

    fn ir(nodes: serde_json::Value, edges: serde_json::Value) -> serde_json::Value {
        json!({
            "ir_version": 1, "name": "expense",
            "declared_scopes": ["data.read", "notify.send"],
            "triggers": [{ "kind": "event", "source": "data.transition", "scope": { "table": TABLE } }],
            "nodes": nodes, "edges": edges
        })
    }

    fn table(states: Option<&[&str]>) -> Catalog {
        let fields = [
            ("title", "text"),
            ("amount", "number"),
            ("approver", "user_ref"),
            ("status", "select"),
        ];
        Catalog {
            tables: Some(BTreeMap::from([(
                TABLE.to_string(),
                CatalogTable {
                    states: states.map(|s| s.iter().map(ToString::to_string).collect()),
                    fields: fields
                        .iter()
                        .map(|(k, t)| ((*k).to_string(), (*t).to_string()))
                        .collect(),
                },
            )])),
            ..Catalog::default()
        }
    }

    /// 出口は指定した判断と取り下げ・時間切れだけ（存在しない出口へのつながりは保存で止める）。
    #[test]
    fn approval_ports_follow_the_params() {
        let ok = ir(
            json!([approval("ok", Some(3600)), { "id": "a", "type": "control.join", "params": {} },
                   { "id": "b", "type": "control.join", "params": {} },
                   { "id": "c", "type": "control.join", "params": {} },
                   { "id": "d", "type": "control.join", "params": {} }]),
            json!([{ "from": "ok", "from_port": "approved", "to": "a" },
                   { "from": "ok", "from_port": "rejected", "to": "b" },
                   { "from": "ok", "from_port": "withdrawn", "to": "c" },
                   { "from": "ok", "from_port": "timeout", "to": "d" }]),
        );
        assert!(
            validate(&ok, &Catalog::default()).is_ok(),
            "{:?}",
            validate(&ok, &Catalog::default()).err()
        );
        let bad = ir(
            json!([approval("ok", None), { "id": "a", "type": "control.join", "params": {} }]),
            json!([{ "from": "ok", "from_port": "returned", "to": "a" }]),
        );
        let errs = validate(&bad, &Catalog::default()).unwrap_err();
        let e = errs.iter().find(|e| e.code == "ir.unknown_port").unwrap();
        assert!(e.message.contains("return（差し戻し）"), "{}", e.message);
    }

    #[test]
    fn approval_needs_data_read() {
        let mut v = ir(json!([approval("ok", None)]), json!([]));
        v["declared_scopes"] = json!(["notify.send"]);
        let errs = validate(&v, &Catalog::default()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.code == "ir.missing_scope" && e.message.contains("data.read")),
            "{errs:?}"
        );
    }

    /// 見えないテーブルと FSM でないテーブルは同じ文言。状態・列はテーブルのものだけ。
    #[test]
    fn tables_states_and_fields_are_checked_against_the_catalog() {
        let v = ir(json!([approval("ok", None)]), json!([]));
        assert!(validate(&v, &table(Some(&["submitted", "approved", "rejected"]))).is_ok());
        let not_fsm = validate(&v, &table(None)).unwrap_err();
        let unseen = validate(
            &v,
            &Catalog {
                tables: Some(BTreeMap::new()),
                ..Catalog::default()
            },
        )
        .unwrap_err();
        assert_eq!(not_fsm[0].message, unseen[0].message);
        assert_eq!(not_fsm[0].path.as_deref(), Some("/params/record/table"));

        let errs = validate(&v, &table(Some(&["submitted", "approved"]))).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, "ir.unknown_state");
        assert_eq!(errs[0].path.as_deref(), Some("/params/reject/to"));

        let mut bad = approval("ok", None);
        bad["params"]["show_fields"] = json!(["title", "ghost"]);
        bad["params"]["assignees"] = json!([{ "field": "amount" }]);
        let errs = validate(
            &ir(json!([bad]), json!([])),
            &table(Some(&["submitted", "approved", "rejected"])),
        )
        .unwrap_err();
        let codes: Vec<&str> = errs.iter().map(|e| e.code.as_str()).collect();
        assert_eq!(codes, ["ir.unknown_field", "ir.bad_assignee"], "{errs:?}");
        // 材料は保存者の見え方なので、材料に無い列は「無い」とも「読めない」とも言い切らない（BUG-65）。
        assert_eq!(
            errs[0].message,
            "列 ghost はこのテーブルに無いか、読めない列です"
        );
        let mut bad = approval("ok", None);
        bad["params"]["assignees"] = json!([{ "field": "deputy" }]);
        let errs = validate(
            &ir(json!([bad]), json!([])),
            &table(Some(&["submitted", "approved", "rejected"])),
        )
        .unwrap_err();
        assert_eq!(
            (errs[0].code.as_str(), errs[0].message.as_str()),
            (
                "ir.unknown_field",
                "担当者の列 deputy はこのテーブルに無いか、読めない列です"
            )
        );
    }

    /// 期限は run の期限未満。続けて待つ承認の期限の合計が run の期限以上なら警告する。
    #[test]
    fn timeouts_are_checked_against_the_run_timeout() {
        let mut v = ir(json!([approval("ok", Some(259_200))]), json!([]));
        let errs = validate(&v, &Catalog::default()).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == "ir.limit_exceeded"
                && e.path.as_deref() == Some("/params/timeout_sec")));
        v["policies"] = json!({ "run_timeout_sec": 1_209_600 });
        assert!(validate(&v, &Catalog::default()).is_ok());

        let serial = ir(
            json!([
                approval("first", Some(172_800)),
                approval("second", Some(172_800))
            ]),
            json!([{ "from": "first", "from_port": "approved", "to": "second" }]),
        );
        let parsed = validate(&serial, &Catalog::default()).unwrap();
        let w = save_warnings(&parsed);
        assert!(
            w.iter().any(|e| e.code == APPROVAL_TIMEOUTS_EXCEED_RUN),
            "{w:?}"
        );
        // 並べて置いた（直列でない）承認は合計しない。
        let parallel = ir(
            json!([
                approval("first", Some(172_800)),
                approval("second", Some(172_800))
            ]),
            json!([]),
        );
        let parsed = validate(&parallel, &Catalog::default()).unwrap();
        assert!(save_warnings(&parsed)
            .iter()
            .all(|e| e.code != APPROVAL_TIMEOUTS_EXCEED_RUN));
    }
}
