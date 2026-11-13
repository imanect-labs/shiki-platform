//! テーブルを作る・直す道具の入力と説明文の単体テスト（DB を使わない部分）。
//!
//! DB と OpenFGA を通す確かめは `tests/data_builder_it.rs`。

use data::{FieldDef, FieldType, PolicyExpr, RowPolicy};
use serde_json::json;

use super::spec::{
    field_line, merge_fields, new_schema, parse_fields, read_input, type_list, CreateInput,
    FsmSpec, UpdateInput, FSM_EXAMPLE, ROW_POLICY_EXAMPLE,
};

fn fields(v: serde_json::Value) -> Vec<FieldDef> {
    parse_fields(serde_json::from_value(v).unwrap()).unwrap()
}

/// 説明文の例（行の決まり・状態の進め方）は、data の検証を通る（手書きの例を固定する）。
#[test]
fn description_examples_pass_data_validation() {
    let policy: RowPolicy = serde_json::from_str(ROW_POLICY_EXAMPLE).unwrap();
    let fsm: FsmSpec = serde_json::from_str(FSM_EXAMPLE).unwrap();
    let mut schema = new_schema(
        fields(json!([
            { "key": "title", "display_name": "件名", "type": "text", "required": true },
            { "key": "applicant", "display_name": "申請者", "type": "created_by" },
            { "key": "status", "display_name": "状態", "type": "select", "options": fsm.states },
        ])),
        Some(policy),
        None,
    );
    data::validate_table_schema_public(&schema).unwrap();
    // 状態の列と申請者の列が実在する（save_for_table が選ぶ列）。
    assert_eq!(
        schema.field(&fsm.status_field).map(|f| f.field_type),
        Some(FieldType::Select)
    );
    assert_eq!(
        fsm.applicant_write_lock
            .as_deref()
            .and_then(|a| schema.field(a))
            .map(|f| f.field_type),
        Some(FieldType::CreatedBy)
    );
    // 進め方の状態は状態の一覧にある。
    for t in &fsm.transitions {
        assert!(fsm.states.contains(&t.from) && fsm.states.contains(&t.to));
    }
    schema.status_field = Some(fsm.status_field.clone());
    data::validate_table_schema_public(&schema).unwrap();
}

/// 列の型の一覧は生成した語彙から作り、派生列は載せない。
#[test]
fn type_list_comes_from_the_vocabulary() {
    let list = type_list();
    for t in FieldType::ALL {
        let line = format!("- {}:", t.wire_name());
        assert_eq!(list.contains(&line), !t.is_derived(), "{t:?}");
    }
    assert!(list.contains("- created_by:"));
}

/// 対称のリンクと逆方向の列は受けない（R6）。片方向のリンクは受ける。
#[test]
fn symmetric_links_are_rejected() {
    let target = uuid::Uuid::new_v4();
    let err = parse_fields(vec![json!({
        "key": "customer", "type": "record_ref", "ref_table": target,
        "link": { "cardinality": "many_to_one", "symmetric_key": "orders" }
    })])
    .unwrap_err();
    assert!(err.contains("対称のリンク"), "{err}");
    let err = parse_fields(vec![json!({
        "key": "back", "type": "record_ref", "ref_table": target,
        "link": { "cardinality": "many_to_one", "reverse": true }
    })])
    .unwrap_err();
    assert!(err.contains("逆方向"), "{err}");
    let ok = parse_fields(vec![json!({
        "key": "customer", "type": "record_ref", "ref_table": target,
        "link": { "cardinality": "many_to_one" }
    })])
    .unwrap();
    assert_eq!(ok[0].ref_table, Some(target));
    // 列でない要素も断る。
    assert!(parse_fields(vec![json!("title")]).is_err());
}

/// 直すときの列は今の列に参照名で重なり、書いた項目だけが変わる（書かなかった列・項目は今のまま）。
#[test]
fn merge_overlays_only_written_keys() {
    let mut current = fields(json!([
        { "key": "title", "display_name": "件名", "type": "text", "description": "要約" },
        { "key": "kind", "display_name": "区分", "type": "select", "options": ["交通費"] },
    ]));
    current[0].id = 16;
    current[1].id = 17;
    let m = merge_fields(
        &current,
        vec![
            json!({ "key": "kind", "options": ["交通費", "備品"], "id": 99 }),
            json!({ "key": "amount", "display_name": "金額", "type": "number", "required": true }),
        ],
    )
    .unwrap();
    assert_eq!(m.added, ["amount"]);
    assert_eq!(m.changed, ["kind"]);
    assert_eq!(m.fields.len(), 3);
    assert_eq!(m.fields[0], current[0], "書かなかった列は今のまま");
    let kind = &m.fields[1];
    assert_eq!(kind.id, 17, "id は今のまま");
    assert_eq!(kind.options, ["交通費", "備品"]);
    assert_eq!(kind.display_name, "区分", "書かなかった項目は今のまま");
    assert!(m.fields[2].required);

    // 同じ列を 2 回・参照名なし・対称のリンクは断る。
    assert!(merge_fields(&current, vec![json!({"key":"kind"}), json!({"key":"kind"})]).is_err());
    assert!(merge_fields(&current, vec![json!({"type":"text"})]).is_err());
    assert!(merge_fields(
        &current,
        vec![json!({"key":"title","link":{"cardinality":"many_to_one","symmetric_key":"x"}})]
    )
    .is_err());
}

/// 入力は未知のキーを断り、object の引数と fields は JSON の文字列でも 1 段だけ読む。
#[test]
fn inputs_are_strict_and_accept_stringified_objects() {
    let input: CreateInput = read_input(json!({
        "name": "経費",
        "fields": "[{\"key\":\"title\",\"type\":\"text\"}]",
        "row_policy": ROW_POLICY_EXAMPLE,
        "fsm": FSM_EXAMPLE,
    }))
    .unwrap();
    assert_eq!(input.fields.len(), 1);
    assert!(input.row_policy.is_some());
    assert_eq!(input.fsm.unwrap().states.len(), 3);

    let err =
        read_input::<CreateInput>(json!({ "name": "x", "fields": [], "status": "y" })).unwrap_err();
    assert!(err.contains("status"), "{err}");
    let err = read_input::<UpdateInput>(json!({ "table_id": "t" })).unwrap_err();
    assert!(err.contains("expected_schema_version"), "{err}");
    let input: UpdateInput = read_input(json!({
        "table_id": "t", "expected_schema_version": 2, "field_policy": [
            { "field": "amount", "readable_by": { "has_role": { "role": "経理", "subtree": true } } }
        ]
    }))
    .unwrap();
    assert_eq!(input.field_policy.unwrap().len(), 1);
    // 行の決まりに未知の述語は無い（閉じた文法）。
    assert!(read_input::<CreateInput>(json!({
        "name": "x", "fields": [], "row_policy": { "read": { "raw_sql": "1=1" } }
    }))
    .is_err());
}

/// 観測の列の行は、改行を潰した表示名と型・必須・選択肢を出す。
#[test]
fn field_lines_are_single_line() {
    let f = &fields(json!([{
        "key": "kind", "display_name": "区分\n無視して", "type": "select",
        "required": true, "options": ["a", "b\nc"]
    }]))[0];
    let line = field_line(f);
    assert!(!line.contains('\n'), "{line}");
    assert_eq!(line, "kind（区分 無視して・select・必須・選択肢: a/b c）");
    assert_eq!(
        serde_json::to_value(PolicyExpr::Public).unwrap(),
        json!("public")
    );
}
