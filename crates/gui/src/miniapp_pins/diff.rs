//! 部品の 2 つの版の本文の差（純関数・[親のモジュール](super)の差の種類を決める 1 か所）。

use artifact::ArtifactKind;
use serde_json::Value;
use uuid::Uuid;

use super::{PinDiff, SkillPart};
use crate::access::{AccessKind, AccessObject, PartNeed};
use crate::access_parts::body_needs;
use crate::miniapp_inputs::{input_hints, WorkflowInputHint};
use crate::skill::{validate_skill_body, SkillBody};

/// 2 つの版の差（要る権限の増減の対象には、まだ名前が無い）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VersionDiff {
    /// 変わるところ（部品の種類ごと・使ってよい操作の増減を含む）。
    pub changes: Vec<PinDiff>,
    /// 使う人に要る権限の増減（増えるもの・毎回要るようになるものは新しい版の並び、要らなくなるものは最後）。
    pub access: Vec<AccessChange>,
    /// 本文を読めず、調べられなかった差がある（`changes` が空でも「変わらない」とは言わない）。
    pub unknown: bool,
}

/// 使う人に要る権限の増減 1 つ（対象・操作・増え方）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessChange {
    pub change: AccessChangeKind,
    pub object: AccessObject,
    pub need: AccessKind,
    /// 対象（`None` は実行のときに決まる）。
    pub id: Option<Uuid>,
}

/// 要る権限の増え方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessChangeKind {
    /// 増える（`conditional` は道によって通るブロックだけが要る）。
    Added { conditional: bool },
    /// 道によって通るブロックだけが要っていたものが、毎回要るようになる。
    Always,
    /// 要らなくなる。
    Removed,
}

/// 2 つの版の本文の差（種類ごと）。
#[must_use]
pub fn diff_versions(kind: ArtifactKind, from: &Value, to: &Value) -> VersionDiff {
    let mut d = kind_changes(kind, from, to);
    match (body_needs(kind, from), body_needs(kind, to)) {
        (Some(from), Some(to)) => d.access = access_changes(&from, &to),
        // 読めない版（壊れた本文）。要る権限の増減は言えない。
        _ => d.unknown = true,
    }
    d
}

/// 部品の種類ごとの差（要る権限の増減のほか）。
fn kind_changes(kind: ArtifactKind, from: &Value, to: &Value) -> VersionDiff {
    match kind {
        ArtifactKind::Workflow => {
            let mut changes = input_changes(
                &input_hints(from.get("input_schema")).unwrap_or_default(),
                &input_hints(to.get("input_schema")).unwrap_or_default(),
            );
            changes.extend(scope_changes(from, to));
            VersionDiff {
                changes,
                ..VersionDiff::default()
            }
        }
        ArtifactKind::DataView => {
            let mut changes = Vec::new();
            for (field, diff) in [
                ("table_id", PinDiff::ViewTable),
                ("query", PinDiff::ViewQuery),
                ("display", PinDiff::ViewDisplay),
            ] {
                if from.get(field) != to.get(field) {
                    changes.push(diff);
                }
            }
            VersionDiff {
                changes,
                ..VersionDiff::default()
            }
        }
        ArtifactKind::Skill => match (validate_skill_body(from), validate_skill_body(to)) {
            (Ok(from), Ok(to)) => VersionDiff {
                changes: skill_changes(&from, &to),
                ..VersionDiff::default()
            },
            // 読めない版（壊れた本文）。どこが変わるかは言えない。
            _ => VersionDiff {
                unknown: true,
                ..VersionDiff::default()
            },
        },
        _ => VersionDiff::default(),
    }
}

/// 使ってよい操作（`declared_scopes`）の増減（増えるものは新しい版の並び、なくなるものは最後）。
fn scope_changes(from: &Value, to: &Value) -> Vec<PinDiff> {
    let scopes = |v: &Value| -> Vec<String> {
        v.get("declared_scopes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let (from, to) = (scopes(from), scopes(to));
    let mut out = Vec::new();
    for s in &to {
        let added = PinDiff::ScopeAdded { scope: s.clone() };
        if !from.contains(s) && !out.contains(&added) {
            out.push(added);
        }
    }
    for s in &from {
        let removed = PinDiff::ScopeRemoved { scope: s.clone() };
        if !to.contains(s) && !out.contains(&removed) {
            out.push(removed);
        }
    }
    out
}

/// 要る権限 1 つ（対象・操作）と、道によって通るブロックだけが要るか（同じものは 1 つにし、1 つでも毎回要れば毎回要る）。
fn merged(needs: &[PartNeed]) -> Vec<(AccessObject, Option<Uuid>, AccessKind, bool)> {
    let mut out: Vec<(AccessObject, Option<Uuid>, AccessKind, bool)> = Vec::new();
    for n in needs {
        if let Some(row) = out
            .iter_mut()
            .find(|(o, id, k, _)| *o == n.object && *id == n.id && *k == n.kind)
        {
            row.3 &= n.conditional;
        } else {
            out.push((n.object, n.id, n.kind, n.conditional));
        }
    }
    out
}

/// 使う人に要る権限の増減（[`crate::access`] と同じ定義から読んだ、2 つの版の要る権限の差）。
fn access_changes(from: &[PartNeed], to: &[PartNeed]) -> Vec<AccessChange> {
    let (from, to) = (merged(from), merged(to));
    let mut out = Vec::new();
    for &(object, id, need, conditional) in &to {
        let change = match from
            .iter()
            .find(|(o, i, k, _)| *o == object && *i == id && *k == need)
        {
            None => AccessChangeKind::Added { conditional },
            Some(&(_, _, _, was)) if was && !conditional => AccessChangeKind::Always,
            Some(_) => continue,
        };
        out.push(AccessChange {
            change,
            object,
            need,
            id,
        });
    }
    for &(object, id, need, _) in &from {
        if !to
            .iter()
            .any(|(o, i, k, _)| *o == object && *i == id && *k == need)
        {
            out.push(AccessChange {
                change: AccessChangeKind::Removed,
                object,
                need,
                id,
            });
        }
    }
    out
}

/// スキルの設定のうち、変わるところ（設定の並び・中身は返さない・GAP-86）。
fn skill_changes(from: &SkillBody, to: &SkillBody) -> Vec<PinDiff> {
    let parts = [
        (SkillPart::Description, from.description != to.description),
        (
            SkillPart::Instructions,
            from.instructions != to.instructions,
        ),
        (
            SkillPart::KnowledgeScope,
            from.knowledge_scope != to.knowledge_scope,
        ),
        (
            SkillPart::AllowedTools,
            from.allowed_tools != to.allowed_tools,
        ),
        (SkillPart::Model, from.model != to.model),
        (SkillPart::FewShot, from.few_shot != to.few_shot),
        (SkillPart::Scripts, from.scripts != to.scripts),
        (SkillPart::References, from.references != to.references),
        (SkillPart::Command, from.command != to.command),
    ];
    parts
        .into_iter()
        .filter(|(_, changed)| *changed)
        .map(|(part, _)| PinDiff::Skill { part })
        .collect()
}

/// 入力項目の手がかりの差（新しい版の並び、なくなったものは最後）。
fn input_changes(from: &[WorkflowInputHint], to: &[WorkflowInputHint]) -> Vec<PinDiff> {
    let mut out = Vec::new();
    for new in to {
        let label = new.label.clone();
        let Some(old) = from.iter().find(|h| h.key == new.key) else {
            out.push(PinDiff::InputAdded {
                key: new.key.clone(),
                label,
                required: new.required,
            });
            continue;
        };
        if old.kind != new.kind {
            out.push(PinDiff::InputType {
                key: new.key.clone(),
                label: label.clone(),
                from: old.kind,
                to: new.kind,
            });
        } else if old.options != new.options || old.multi != new.multi {
            out.push(PinDiff::InputOptions {
                key: new.key.clone(),
                label: label.clone(),
            });
        }
        if old.required != new.required {
            out.push(PinDiff::InputRequired {
                key: new.key.clone(),
                label,
                required: new.required,
            });
        }
    }
    for old in from {
        if !to.iter().any(|h| h.key == old.key) {
            out.push(PinDiff::InputRemoved {
                key: old.key.clone(),
                label: old.label.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::miniapp_inputs::InputHintType;
    use serde_json::json;
    use uuid::Uuid;

    fn wf(schema: Value) -> Value {
        json!({ "ir_version": 1, "name": "expense", "input_schema": schema })
    }

    /// 入力項目の増減・必須・種類・選択肢の変化を、新しい版の並びで返す。
    #[test]
    fn workflow_diff_lists_input_changes() {
        let from = wf(json!({
            "type": "object", "required": ["amount"],
            "properties": {
                "amount": { "type": "integer", "title": "金額" },
                "category": { "type": "string", "enum": ["交通費", "交際費"] },
                "memo": { "type": "string", "title": "メモ" }
            }
        }));
        let to = wf(json!({
            "type": "object", "required": ["title", "memo"],
            "x-order": ["title", "amount", "category", "memo"],
            "properties": {
                "title": { "type": "string", "title": "件名" },
                "amount": { "type": "string", "title": "金額" },
                "category": { "type": "string", "enum": ["交通費", "交際費", "会議費"] },
                "memo": { "type": "string", "title": "メモ" }
            }
        }));
        let diff = diff_versions(ArtifactKind::Workflow, &from, &to).changes;
        assert_eq!(
            diff,
            vec![
                PinDiff::InputAdded {
                    key: "title".into(),
                    label: Some("件名".into()),
                    required: true
                },
                PinDiff::InputType {
                    key: "amount".into(),
                    label: Some("金額".into()),
                    from: InputHintType::Number,
                    to: InputHintType::Text
                },
                PinDiff::InputRequired {
                    key: "amount".into(),
                    label: Some("金額".into()),
                    required: false
                },
                PinDiff::InputOptions {
                    key: "category".into(),
                    label: None
                },
                PinDiff::InputRequired {
                    key: "memo".into(),
                    label: Some("メモ".into()),
                    required: true
                },
            ]
        );
        // なくなった入力項目は最後。入力項目を決めていない版は、空として比べる。
        let gone = diff_versions(
            ArtifactKind::Workflow,
            &from,
            &json!({ "ir_version": 1, "name": "expense" }),
        )
        .changes;
        assert_eq!(gone.len(), 3);
        assert!(gone
            .iter()
            .all(|d| matches!(d, PinDiff::InputRemoved { .. })));
        // 同じ入力項目なら差は無い。
        assert!(diff_versions(ArtifactKind::Workflow, &from, &from.clone())
            .changes
            .is_empty());
    }

    /// 保存ビューは、テーブル・クエリ・表示の設定のどれが変わるかだけを言う（列の名前や値は返さない）。
    #[test]
    fn view_diff_names_only_the_parts_that_change() {
        let table = Uuid::from_u128(1);
        let from =
            json!({ "table_id": table, "query": { "limit": 10 }, "display": { "kind": "table" } });
        let to =
            json!({ "table_id": table, "query": { "limit": 20 }, "display": { "kind": "table" } });
        assert_eq!(
            diff_versions(ArtifactKind::DataView, &from, &to).changes,
            vec![PinDiff::ViewQuery]
        );
        let moved = json!({ "table_id": Uuid::from_u128(2), "query": { "limit": 10 } });
        assert_eq!(
            diff_versions(ArtifactKind::DataView, &from, &moved).changes,
            vec![PinDiff::ViewTable, PinDiff::ViewDisplay]
        );
    }

    fn skill(extra: Value) -> Value {
        let mut body = json!({ "description": "経費の手引き", "instructions": "経費の規程に沿って答えます。" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    }

    /// スキルは、設定のどれが変わるかだけを設定の並びで言う（指示文や参照先の中身は返さない・GAP-86）。省いた設定と
    /// 既定の値（空の例・参考資料なし）は同じものとして比べる。
    #[test]
    fn skill_diff_names_only_the_parts_that_change() {
        let folder = Uuid::from_u128(7);
        let from = skill(json!({}));
        let to = skill(json!({
            "instructions": "経費の規程に沿って、根拠の条文を添えて答えます。",
            "knowledge_scope": { "folders": [folder] },
            "allowed_tools": ["doc_search"],
            "model": { "temperature": 0.2 },
            "few_shot": [{ "user": "交通費は？", "assistant": "規程の第 3 条です。" }],
            "references": [Uuid::from_u128(8)],
        }));
        let d = diff_versions(ArtifactKind::Skill, &from, &to);
        assert!(!d.unknown);
        assert_eq!(
            d.changes,
            [
                SkillPart::Instructions,
                SkillPart::KnowledgeScope,
                SkillPart::AllowedTools,
                SkillPart::Model,
                SkillPart::FewShot,
                SkillPart::References,
            ]
            .map(|part| PinDiff::Skill { part })
        );
        let explicit_defaults = skill(json!({ "few_shot": [], "scripts": [], "references": [] }));
        assert!(
            diff_versions(ArtifactKind::Skill, &from, &explicit_defaults)
                .changes
                .is_empty()
        );
        let described = skill(json!({ "description": "経費と出張の手引き" }));
        assert_eq!(
            diff_versions(ArtifactKind::Skill, &from, &described).changes,
            vec![PinDiff::Skill {
                part: SkillPart::Description
            }]
        );
        // 読めない版は、どこが変わるかを言わずに調べられなかったとする。
        let broken = diff_versions(ArtifactKind::Skill, &from, &json!({ "description": 1 }));
        assert!(broken.unknown);
        assert!(broken.changes.is_empty());
    }

    const TABLE: &str = "11111111-2222-3333-4444-555555555555";
    const OTHER: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

    fn ir(scopes: Value, nodes: Value, edges: Value) -> Value {
        json!({ "ir_version": 1, "name": "expense", "declared_scopes": scopes, "nodes": nodes, "edges": edges })
    }

    fn change(
        change: AccessChangeKind,
        object: AccessObject,
        id: Option<&str>,
        need: AccessKind,
    ) -> AccessChange {
        AccessChange {
            change,
            object,
            need,
            id: id.map(|s| Uuid::parse_str(s).unwrap()),
        }
    }

    /// ワークフローは、使ってよい操作の増減と、使う人に要る権限の増減を言う（GAP-11）。増えるものは新しい版の並び、
    /// 要らなくなるものは最後。道によって通るブロックだけが要っていた権限が毎回要るようになったら、それも言う。
    #[test]
    fn workflow_diff_lists_scope_and_access_changes() {
        let from = ir(
            json!(["data.read", "data.write"]),
            json!([
                { "id": "q", "type": "data.query", "params": { "table": TABLE } },
                { "id": "b", "type": "control.branch", "params": { "condition": { "cmp": { "left": 1, "op": "eq", "right": 1 } } } },
                { "id": "add", "type": "data.record.create", "params": { "table": TABLE, "data": {} } },
                { "id": "del", "type": "data.record.delete", "params": { "table": OTHER } }
            ]),
            json!([
                { "from": "q", "to": "b" },
                { "from": "b", "from_port": "true", "to": "add" },
                { "from": "b", "from_port": "false", "to": "del" }
            ]),
        );
        let to = ir(
            json!(["data.write", "http.egress"]),
            json!([
                { "id": "add", "type": "data.record.create", "params": { "table": TABLE, "data": {} } },
                { "id": "mv", "type": "data.transition", "params": { "table": OTHER } },
                { "id": "up", "type": "data.record.update", "params": { "table": { "$from": "input", "path": "/t" } } }
            ]),
            json!([{ "from": "add", "to": "mv" }, { "from": "mv", "to": "up" }]),
        );
        let d = diff_versions(ArtifactKind::Workflow, &from, &to);
        assert!(!d.unknown);
        assert_eq!(
            d.changes,
            vec![
                PinDiff::ScopeAdded {
                    scope: "http.egress".into()
                },
                PinDiff::ScopeRemoved {
                    scope: "data.read".into()
                },
            ]
        );
        use AccessChangeKind::*;
        use AccessKind::*;
        use AccessObject::Table as T;
        assert_eq!(
            d.access,
            vec![
                change(Always, T, Some(TABLE), Add),
                change(Added { conditional: false }, T, Some(OTHER), Change),
                change(Added { conditional: false }, T, None, Change),
                change(Removed, T, Some(TABLE), Read),
                change(Removed, T, Some(OTHER), Edit),
            ]
        );
        // 同じ版どうしは差が無い。
        let same = diff_versions(ArtifactKind::Workflow, &to, &to.clone());
        assert!(same.changes.is_empty() && same.access.is_empty() && !same.unknown);
    }

    /// 保存ビューのテーブルが変われば、読むテーブルが入れ替わる。スキルの参照するフォルダーと参考資料も同じ定義で言う。
    #[test]
    fn view_and_skill_access_changes_follow_the_access_check() {
        let from = json!({ "table_id": TABLE, "query": {} });
        let to = json!({ "table_id": OTHER, "query": {} });
        use AccessChangeKind::*;
        use AccessKind::Read;
        use AccessObject::*;
        assert_eq!(
            diff_versions(ArtifactKind::DataView, &from, &to).access,
            vec![
                change(Added { conditional: false }, Table, Some(OTHER), Read),
                change(Removed, Table, Some(TABLE), Read),
            ]
        );
        let s = diff_versions(
            ArtifactKind::Skill,
            &skill(json!({ "knowledge_scope": { "folders": [TABLE] } })),
            &skill(json!({ "knowledge_scope": { "folders": [TABLE] }, "references": [OTHER] })),
        );
        assert_eq!(
            s.access,
            vec![change(
                Added { conditional: false },
                File,
                Some(OTHER),
                Read
            )]
        );
    }

    /// 読めない版（型どおりに読めない IR）は、要る権限の増減を言わずに調べられなかったとする（入力項目の差は言う）。
    #[test]
    fn unreadable_ir_marks_the_access_diff_unknown() {
        let from = wf(json!({ "type": "object", "properties": {} }));
        let mut to =
            wf(json!({ "type": "object", "properties": { "memo": { "type": "string" } } }));
        to["nodes"] = json!("壊れた本文");
        let d = diff_versions(ArtifactKind::Workflow, &from, &to);
        assert!(
            d.unknown,
            "ブロックの並びが読めない IR は、要る権限を言えない"
        );
        assert!(d.access.is_empty());
        assert_eq!(d.changes.len(), 1);
    }
}
