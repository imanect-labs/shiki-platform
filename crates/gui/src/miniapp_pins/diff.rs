//! 部品の 2 つの版の本文の差（純関数・[親のモジュール](super)の差の種類を決める 1 か所）。

use artifact::ArtifactKind;
use serde_json::Value;

use super::{PinDiff, SkillPart};
use crate::miniapp_inputs::{input_hints, WorkflowInputHint};
use crate::skill::{validate_skill_body, SkillBody};

/// 2 つの版の差。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VersionDiff {
    /// 変わるところ（部品の種類ごと）。
    pub changes: Vec<PinDiff>,
    /// 本文を読めず、調べられなかった差がある（`changes` が空でも「変わらない」とは言わない）。
    pub unknown: bool,
}

/// 2 つの版の本文の差（種類ごと）。
#[must_use]
pub fn diff_versions(kind: ArtifactKind, from: &Value, to: &Value) -> VersionDiff {
    match kind {
        ArtifactKind::Workflow => VersionDiff {
            changes: input_changes(
                &input_hints(from.get("input_schema")).unwrap_or_default(),
                &input_hints(to.get("input_schema")).unwrap_or_default(),
            ),
            unknown: false,
        },
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
                unknown: false,
            }
        }
        ArtifactKind::Skill => match (validate_skill_body(from), validate_skill_body(to)) {
            (Ok(from), Ok(to)) => VersionDiff {
                changes: skill_changes(&from, &to),
                unknown: false,
            },
            // 読めない版（壊れた本文）。どこが変わるかは言えない。
            _ => VersionDiff {
                changes: Vec::new(),
                unknown: true,
            },
        },
        _ => VersionDiff::default(),
    }
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
}
