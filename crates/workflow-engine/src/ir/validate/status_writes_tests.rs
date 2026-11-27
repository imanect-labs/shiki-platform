//! 状態の列への書き込みの保存時検証（BUG-251）の単体テスト。
#![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]

use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::ir::validate::CatalogTable;

const FLOW: &str = "00000000-0000-0000-0000-0000000000f1";
/// 状態の列を保存者が読めない表（見え方の定義では状態の列が外れている）。
const MASKED: &str = "00000000-0000-0000-0000-0000000000f2";
const PLAIN: &str = "00000000-0000-0000-0000-0000000000a2";

fn schema(status: Option<&str>) -> data::TableSchema {
    serde_json::from_value(json!({
        "fields": [
            { "key": "title", "type": "text" },
            { "key": "code", "type": "text", "unique": true },
            { "key": "qty", "type": "number" },
            { "key": "status", "display_name": "状態", "type": "select", "options": ["申請中", "承認済み", "却下"] }
        ],
        "status_field": status,
        "fsm_ref": { "artifact_id": "00000000-0000-0000-0000-0000000000ff", "version": 1 }
    }))
    .unwrap()
}

fn catalog() -> Catalog {
    let flow = CatalogTable {
        status_field: Some("status".into()),
        schema: Some(schema(Some("status"))),
        fsm_initial: Some(data::FsmInitial {
            status_field: "status".into(),
            initial: "申請中".into(),
        }),
        ..CatalogTable::default()
    };
    // 状態の列が読めなければ、見え方の定義に状態の列が無く、作成時の状態も引かない（承認と同じ扱い）。
    let masked = CatalogTable {
        schema: Some(schema(None)),
        ..CatalogTable::default()
    };
    let mut plain = schema(None);
    plain.fsm_ref = None;
    let plain = CatalogTable {
        schema: Some(plain),
        ..CatalogTable::default()
    };
    Catalog {
        tables: Some(BTreeMap::from([
            (FLOW.to_string(), flow),
            (MASKED.to_string(), masked),
            (PLAIN.to_string(), plain),
        ])),
        ..Catalog::default()
    }
}

fn ir(nodes: serde_json::Value) -> WorkflowIr {
    WorkflowIr::from_json(&json!({
        "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
        "nodes": nodes, "edges": []
    }))
    .unwrap()
}

fn located(errs: &[ValidationError]) -> Vec<(&str, &str, &str)> {
    errs.iter()
        .map(|e| {
            (
                e.code.as_str(),
                e.node_id.as_deref().unwrap_or_default(),
                e.path.as_deref().unwrap_or_default(),
            )
        })
        .collect()
}

fn run(nodes: serde_json::Value) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    v4(&ir(nodes), &catalog(), &mut errors);
    errors
}

/// 更新の変更する値・足す数と、追加か更新の入れる値・足す数に状態の列を書くと止める（実行では data が必ず断る）。
/// 状態の列でない列・値の欄の全体を参照で決めるもの・状態の進め方の無い表は通す。
#[test]
fn writing_the_status_column_outside_a_transition_is_refused() {
    let errors = run(json!([
        { "id": "patch", "type": "data.record.update", "params": {
            "table": FLOW, "record": { "$from": "input", "path": "/id" },
            "patch": { "title": "x", "status": "承認済み" } } },
        { "id": "add", "type": "data.record.update", "params": {
            "table": FLOW, "record": { "$from": "input", "path": "/id" }, "add": { "status": 1 } } },
        { "id": "upsert", "type": "data.record.upsert", "params": {
            "table": FLOW, "match_on": "code", "data": { "code": "A", "status": "申請中" } } },
        { "id": "ok", "type": "data.record.update", "params": {
            "table": FLOW, "record": { "$from": "input", "path": "/id" }, "patch": { "title": "x" },
            "add": { "qty": 1 } } },
        { "id": "dynamic", "type": "data.record.update", "params": {
            "table": FLOW, "record": { "$from": "input", "path": "/id" },
            "patch": { "$from": "input", "path": "/patch" } } },
        { "id": "plain", "type": "data.record.update", "params": {
            "table": PLAIN, "record": { "$from": "input", "path": "/id" }, "patch": { "status": "却下" } } },
        { "id": "by_ref", "type": "data.record.update", "params": {
            "table": { "$from": "input", "path": "/t" }, "record": { "$from": "input", "path": "/id" },
            "patch": { "status": "却下" } } }
    ]));
    assert_eq!(
        located(&errors),
        vec![
            (STATUS_NOT_WRITABLE, "patch", "/params/patch/status"),
            (STATUS_NOT_WRITABLE, "add", "/params/add/status"),
            (STATUS_NOT_WRITABLE, "upsert", "/params/data/status"),
        ]
    );
    let m = &errors[0].message;
    assert!(
        m.contains(
            "「状態」（status）は状態の列なので、「データを更新」の「変更する値」に書けません"
        ) && m.contains("「状態を進める」のブロックを使って"),
        "{m}"
    );
    let m = &errors[2].message;
    assert!(
        m.contains("書き換えるときに必ず断られます") && m.contains("最初の状態「申請中」"),
        "{m}"
    );
}

/// 追加の値の状態は最初の状態だけ（書かないか、最初の状態を書く）。参照で決める状態は実行時に確かめる。探す列が状態の
/// 列の追加か更新も、追加の値として同じく照らす。
#[test]
fn creating_with_a_later_state_is_refused() {
    let errors = run(json!([
        { "id": "later", "type": "data.record.create", "params": {
            "table": FLOW, "data": { "title": "x", "status": "承認済み" } } },
        { "id": "first", "type": "data.record.create", "params": {
            "table": FLOW, "data": { "title": "x", "status": "申請中" } } },
        { "id": "omitted", "type": "data.record.create", "params": {
            "table": FLOW, "data": { "title": "x" } } },
        { "id": "by_ref", "type": "data.record.create", "params": {
            "table": FLOW, "data": { "status": { "$from": "input", "path": "/s" } } } },
        { "id": "keyed", "type": "data.record.upsert", "params": {
            "table": FLOW, "match_on": "status", "data": { "status": "却下" } } }
    ]));
    assert_eq!(
        located(&errors),
        vec![
            (STATUS_NOT_WRITABLE, "later", "/params/data/status"),
            (STATUS_NOT_WRITABLE, "keyed", "/params/data/status"),
        ]
    );
    assert!(
        errors[0]
            .message
            .contains("追加したレコードの状態は最初の状態「申請中」に決まります"),
        "{}",
        errors[0].message
    );
}

/// 状態の列を読めない保存者には、状態の列への書き込みを言わない（無い列と区別させない・承認と同じ）。材料の無い構成は
/// 照合しない。
#[test]
fn an_unreadable_status_column_is_not_named() {
    let nodes = json!([
        { "id": "patch", "type": "data.record.update", "params": {
            "table": MASKED, "record": { "$from": "input", "path": "/id" }, "patch": { "status": "承認済み" } } },
        { "id": "create", "type": "data.record.create", "params": {
            "table": MASKED, "data": { "status": "承認済み" } } }
    ]);
    assert!(run(nodes.clone()).is_empty());
    let mut none = Vec::new();
    v4(&ir(nodes), &Catalog::default(), &mut none);
    assert!(none.is_empty());
}

/// 保存の検証の全体（`validate`）では、足す数に書いた状態の列は状態の列の誤り 1 つだけになる（数の列でない誤りを重ねない）。
#[test]
fn validate_names_the_status_column_once() {
    let mut catalog = catalog();
    if let Some(t) = catalog.tables.as_mut().and_then(|t| t.get_mut(FLOW)) {
        t.fields = [("title", "text"), ("qty", "number"), ("status", "select")]
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
    }
    let wf = json!({
        "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
        "nodes": [{ "id": "add", "type": "data.record.update", "params": {
            "table": FLOW, "record": { "$from": "input", "path": "/id" },
            "add": { "status": 1, "qty": 1 } } }],
        "edges": []
    });
    let (wf, _) = crate::ir::scopes::complete_declared_scopes(&wf);
    let errs = super::super::validate(&wf, &catalog).unwrap_err();
    assert_eq!(
        located(&errs),
        vec![(STATUS_NOT_WRITABLE, "add", "/params/add/status")]
    );
}

/// まとめて登録の行（GAP-129）: id のある行と、同じレコードを探す列のある行の data に状態の列を書くと、値によらず止める
/// （書き換えるときに必ず断られる）。追加の行は、最初の状態でない状態を書くと止める（最初の状態・書かない・参照の値は
/// 通す）。
/// id が null の行は追加の行。行の全体・行の data・行の配列を参照で決めるもの、状態の列が読めない表・状態の進め方の
/// 無い表は照らさない。
#[test]
fn bulk_rows_follow_the_upsert_rule() {
    let id = "00000000-0000-0000-0000-00000000aaaa";
    let from = json!({ "$from": "input", "path": "/x" });
    let rows = json!([
        { "data": { "title": "a" } },
        { "data": { "title": "b", "status": "申請中" } },
        { "data": { "status": "承認済み" } },
        { "id": id, "data": { "status": "申請中" } },
        { "id": null, "data": { "status": "申請中" } },
        { "id": from, "data": { "title": "c", "status": "申請中" } },
        { "data": { "status": from } },
        from,
        { "data": from }
    ]);
    let bulk = |id: &str, table: &str, extra: serde_json::Value| {
        let mut params = json!({ "table": table, "rows": rows.clone() });
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        json!({ "id": id, "type": "data.bulk_upsert", "params": params })
    };
    let errors = run(json!([
        bulk("plain_rows", FLOW, json!({})),
        bulk("keyed", FLOW, json!({ "match_on": "code" })),
        { "id": "dynamic", "type": "data.bulk_upsert", "params": { "table": FLOW, "rows": from } },
        bulk("masked", MASKED, json!({})),
        bulk("plain", PLAIN, json!({}))
    ]));
    let at = |i: usize| format!("/params/rows/{i}/data/status");
    let want = [
        ("plain_rows", at(2)),
        ("plain_rows", at(3)),
        ("plain_rows", at(5)),
        ("keyed", at(1)),
        ("keyed", at(2)),
        ("keyed", at(3)),
        ("keyed", at(4)),
        ("keyed", at(5)),
        // 書き換えでは値によらず状態の列が断られる（値を参照で決めても止める）。
        ("keyed", at(6)),
    ];
    let want: Vec<(&str, &str, &str)> = want
        .iter()
        .map(|(n, p)| (STATUS_NOT_WRITABLE, *n, p.as_str()))
        .collect();
    assert_eq!(located(&errors), want);
    let m = |i: usize| errors[i].message.as_str();
    assert!(
        m(0).starts_with("「データをまとめて登録」の 3 行目で追加するレコードの状態は最初の状態「申請中」に決まります。")
            && m(0).contains("「状態」（status）は書かないか「申請中」にし"),
        "{}",
        m(0)
    );
    assert!(
        m(1).starts_with("「状態」（status）は状態の列なので、「データをまとめて登録」の 4 行目（id のある行）の data に書けません")
            && m(1).contains("「状態を進める」のブロックを使ってください"),
        "{}",
        m(1)
    );
    assert!(
        m(3).contains("2 行目の data に書けません（「同じレコードを探す列」で見つかったレコードを書き換えるときに必ず断られます）")
            && m(3).contains("追加したレコードは最初の状態「申請中」になり"),
        "{}",
        m(3)
    );
}
