//! ノードカタログの単一定義の試験（行数ゲート対応で catalog.rs から分離）。

use super::*;

#[test]
fn catalog_covers_all_vocab_in_order() {
    let catalog = node_catalog();
    assert_eq!(catalog.len(), NodeType::ALL.len());
    for (entry, nt) in catalog.iter().zip(NodeType::ALL) {
        assert_eq!(entry.node_type, *nt);
    }
}

#[test]
fn stage_a_entries_have_timeout_or_are_control() {
    for e in node_catalog().iter().filter(|e| e.available) {
        assert!(
            e.node_type.is_control() || e.timeout_default_sec.is_some(),
            "{} は timeout 既定が必要",
            e.node_type.as_str()
        );
    }
}

#[test]
fn structural_flags_match_vocab() {
    let catalog = node_catalog();
    let find = |nt: NodeType| catalog.iter().find(|e| e.node_type == nt).unwrap();
    assert!(find(NodeType::ControlJoin).multi_input);
    assert!(find(NodeType::ControlSwitch).dynamic_ports);
    assert_eq!(
        find(NodeType::ControlBranch).output_ports,
        ["true", "false"]
    );
    assert_eq!(
        find(NodeType::StorageWrite).required_scope,
        Some(Scope::StorageWrite)
    );
    assert_eq!(
        find(NodeType::StorageWrite).idempotency,
        IdempotencyClass::EngineDedup
    );
    // 出口の決まり方（V2・検証 API の node_ports が同じ規則で導く）。
    assert_eq!(
        find(NodeType::ControlSwitch).port_rule,
        PortRule::SwitchCases
    );
    assert_eq!(
        find(NodeType::HumanApproval).port_rule,
        PortRule::ApprovalChoices
    );
    assert!(!find(NodeType::HumanApproval).dynamic_ports);
    assert_eq!(find(NodeType::StorageRead).port_rule, PortRule::Static);
    // 判定（FEAT-1）: 出口は振り分けから決まる・pure（課金あり）・スコープ天井の対象外・60/300 秒。
    let decide = find(NodeType::AiDecide);
    assert_eq!(decide.port_rule, PortRule::DecideRoute);
    assert!(decide.dynamic_ports);
    assert!(decide.output_ports.is_empty());
    assert_eq!(decide.category, NodeCategory::Ai);
    assert_eq!(decide.idempotency, IdempotencyClass::Pure);
    assert_eq!(decide.required_scope, None);
    assert_eq!(
        (decide.timeout_default_sec, decide.timeout_max_sec),
        (Some(60), Some(300))
    );
    assert!(decide.available);
    let names: Vec<&str> = decide.output_fields.iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        [
            "answers",
            "model",
            "resolved_model",
            "calibrated",
            "method",
            "samples",
            "cached",
            "routed"
        ]
    );
    // 決定キャッシュ（step 属性 cache・T10）を付けられるのは判定だけ。
    assert_eq!(
        decide.step_cache,
        Some(crate::ir::StepCacheLimits::DECISION)
    );
    assert!(node_catalog()
        .iter()
        .filter(|e| e.node_type != NodeType::AiDecide)
        .all(|e| e.step_cache.is_none()));
    let json = serde_json::to_value(decide).unwrap();
    assert_eq!(json["port_rule"], "decide_route");
}

#[test]
fn labels_are_nonempty_and_unique_within_category() {
    use std::collections::BTreeSet;
    let mut seen: BTreeSet<(String, &str)> = BTreeSet::new();
    for e in node_catalog() {
        assert!(!e.label_ja.is_empty());
        assert!(!e.description_ja.is_empty());
        assert!(
            seen.insert((format!("{:?}", e.category), e.label_ja)),
            "カテゴリ内でラベル重複: {}",
            e.label_ja
        );
    }
}

/// 使えるノードは出力の形を持ち（AI の説明文・右パネルが参照する）、予約語彙は持たない。
#[test]
fn output_shape_is_defined_exactly_for_stage_a_nodes() {
    for e in node_catalog() {
        assert_eq!(
            !e.output_shape.is_empty(),
            e.available,
            "{} の出力の形",
            e.node_type.as_str()
        );
    }
    let find = |nt: NodeType| {
        node_catalog()
            .into_iter()
            .find(|e| e.node_type == nt)
            .unwrap()
    };
    // 出力の形（output_schema）を書いたときだけ、検証済みの data が付く（GAP-13）。
    assert_eq!(
        find(NodeType::LlmInvoke).output_shape,
        "{ text: string, model: string, data?: unknown（output_schema を書いたとき・その形で検証済みの値） }"
    );
}

/// D-42: 出力の形の文字列は構造化した項目から作る（項目の組なら必ず項目を持つ・名前は一意）。
#[test]
fn output_shape_text_is_built_from_fields() {
    let catalog = node_catalog();
    let find = |nt: NodeType| catalog.iter().find(|e| e.node_type == nt).unwrap();
    let record = find(NodeType::DataRecordCreate);
    assert_eq!(record.output_fields[0].name, "id");
    assert!(record
        .output_shape
        .starts_with("レコード { id: string, table_id: string,"));
    let map = find(NodeType::ControlMap);
    assert_eq!(
        map.output_shape,
        "{ items: Array<unknown>（領域の出口ノードの出力・要素の順）, errors?: Array<{ index: number, error: { code: string, message: string } }>（on_item_error: collect のとき） }"
    );
    assert!(map.output_fields[1].optional);
    assert_eq!(
        find(NodeType::StorageList).output_shape,
        "{ items: Array<{ id: string, name: string, kind: string, size: number }> }（最大 100 件）"
    );
    // 項目に分けられない形は項目を持たない（参照の候補は「結果（全体）」だけ）。
    for nt in [
        NodeType::ScriptRun,
        NodeType::ControlWait,
        NodeType::SkillInvoke,
    ] {
        assert!(find(nt).output_fields.is_empty(), "{}", nt.as_str());
        assert!(!find(nt).output_shape.is_empty());
    }
    for e in &catalog {
        let mut names: Vec<&str> = e.output_fields.iter().map(|f| f.name).collect();
        for f in e.output_fields {
            assert!(
                e.output_shape.contains(&format!(
                    "{}{}: {}",
                    f.name,
                    if f.optional { "?" } else { "" },
                    f.ty
                )),
                "{}: {}",
                e.node_type.as_str(),
                f.name
            );
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            e.output_fields.len(),
            "{}",
            e.node_type.as_str()
        );
    }
}

#[test]
fn catalog_serializes_to_json() {
    let json = serde_json::to_value(node_catalog()).unwrap();
    let arr = json.as_array().unwrap();
    assert_eq!(arr[0]["type"], "control.branch");
    assert_eq!(arr[0]["category_label_ja"], "流れの制御");
}
