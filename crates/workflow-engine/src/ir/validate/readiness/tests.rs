//! 保存は止めず、有効化と実行の前に止める条件（readiness）の試験。

use super::*;
use serde_json::json;

#[allow(clippy::needless_pass_by_value)]
fn ir(triggers: Value, nodes: Value) -> WorkflowIr {
    WorkflowIr::from_json(&json!({
        "ir_version": 1, "name": "wf", "triggers": triggers, "nodes": nodes, "edges": []
    }))
    .unwrap()
}

/// （コード・ノード id・path）の並び。
type Located<'a> = (&'a str, Option<&'a str>, Option<&'a str>);

fn codes(errs: &[ValidationError]) -> Vec<Located<'_>> {
    errs.iter()
        .map(|e| (e.code.as_str(), e.node_id.as_deref(), e.path.as_deref()))
        .collect()
}

#[test]
fn blank_required_texts_are_listed_with_their_path() {
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            { "id": "ask", "type": "llm.invoke", "params": { "prompt": "  " } },
            { "id": "agent", "type": "agent.invoke", "params": { "instruction": { "$template": "", "vars": {} } } },
            { "id": "search", "type": "rag.search", "params": { "query": "" } },
            { "id": "sum", "type": "csv.query", "params": { "file": "00000000-0000-0000-0000-000000000001", "sql": "" } },
            // 参照は実行時まで分からない・中身のある文字は空でない。
            { "id": "ok_ref", "type": "llm.invoke", "params": { "prompt": { "$from": "input", "path": "/q" } } },
            { "id": "ok_text", "type": "rag.search", "params": { "query": "規程" } }
        ]),
    );
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (REQUIRED_TEXT_EMPTY, Some("ask"), Some("/params/prompt")),
            (
                REQUIRED_TEXT_EMPTY,
                Some("agent"),
                Some("/params/instruction")
            ),
            (REQUIRED_TEXT_EMPTY, Some("search"), Some("/params/query")),
            (REQUIRED_TEXT_EMPTY, Some("sum"), Some("/params/sql")),
        ]
    );
    assert!(w[0].message.contains("「AI への指示」"), "{}", w[0].message);
    // 対話の起動も同じ理由で止める。
    assert_eq!(run_blockers(&ir).len(), 4);
    assert_eq!(enable_blockers(&ir).len(), 4);
}

/// D-108: 判定の質問の「AI に尋ねること」が空なら、保存の警告にとどめ、実行と自動実行のオンの前に止める
/// （質問ごとの位置を返す）。参照は書けない欄なので、中身のある文だけが空でない。
#[test]
fn a_blank_decide_question_blocks_run_and_enable() {
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([{ "id": "triage", "type": "ai.decide", "params": {
            "model": "decide-stub", "state": "本文",
            "questions": [
                { "key": "q1", "type": "choice", "instructions": "",
                  "options": [{ "key": "option_1" }, { "key": "option_2" }] },
                { "key": "q2", "type": "boolean", "instructions": "請求か" },
                { "key": "q3", "type": "boolean", "instructions": "  " }
            ] } }]),
    );
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (
                REQUIRED_TEXT_EMPTY,
                Some("triage"),
                Some("/params/questions/0/instructions")
            ),
            (
                REQUIRED_TEXT_EMPTY,
                Some("triage"),
                Some("/params/questions/2/instructions")
            )
        ]
    );
    assert_eq!(
        w[0].message,
        "「AI に尋ねること」が空です。入れるまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(run_blockers(&ir).len(), 2);
    assert_eq!(enable_blockers(&ir).len(), 2);
}

/// #1037: CSV を集計のつなぐ CSV（`tables` の各要素）の名前・ファイルが空（項目が無いのも空）なら、保存の警告に
/// とどめ、実行と自動実行のオンの前に止める（要素ごとの位置を返す）。参照は実行時まで分からないので空でない。
/// `tables` 自体を値式で書いたときは見ない。
#[test]
fn a_blank_joined_csv_blocks_run_and_enable() {
    const FILE: &str = "00000000-0000-0000-0000-000000000001";
    let sum = |id: &str, tables: Value| {
        json!({ "id": id, "type": "csv.query",
                "params": { "file": FILE, "sql": "SELECT * FROM data", "tables": tables } })
    };
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            sum(
                "join",
                json!([
                    { "name": "master", "file": FILE },
                    { "name": "m2", "file": "" },
                    { "name": "m3" },
                    { "name": " ", "file": { "$from": "input", "path": "/f" } },
                    { "name": { "$from": "input", "path": "/n" }, "file": FILE }
                ])
            ),
            sum("dynamic", json!({ "$from": "input", "path": "/tables" }))
        ]),
    );
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (
                REQUIRED_TEXT_EMPTY,
                Some("join"),
                Some("/params/tables/1/file")
            ),
            (
                REQUIRED_TEXT_EMPTY,
                Some("join"),
                Some("/params/tables/2/file")
            ),
            (
                REQUIRED_TEXT_EMPTY,
                Some("join"),
                Some("/params/tables/3/name")
            ),
        ]
    );
    assert_eq!(
        w[0].message,
        "「2 つ目のつなぐ CSV」が空です。選ぶまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(
        w[2].message,
        "「4 つ目の CSV の SQL での名前」が空です。入れるまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(run_blockers(&ir).len(), 3);
    assert_eq!(enable_blockers(&ir).len(), 3);
    assert_eq!(unattended_run_blockers(&ir).len(), 3);
}

/// D-88: 出口を分ける判定のしきい値は既定を置かないので、空なら保存の警告にとどめ、実行と自動実行のオンの
/// 前に止める（項目が無い・null のどちらも）。しきい値を書いた判定と、出口を分けない判定は止めない。
#[test]
fn a_blank_decide_threshold_blocks_run_and_enable() {
    let decide = |id: &str, route: Option<Value>| {
        let mut n = json!({ "id": id, "type": "ai.decide", "params": {
            "model": "decide-stub", "state": "本文",
            "questions": [{ "key": "q1", "type": "boolean", "instructions": "請求か" }] } });
        if let Some(r) = route {
            n["params"]["route"] = r;
        }
        n
    };
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            decide("missing", Some(json!({ "question": "q1" }))),
            decide(
                "null",
                Some(json!({ "question": "q1", "min_confidence": null }))
            ),
            decide(
                "set",
                Some(json!({ "question": "q1", "min_confidence": 0.8 }))
            ),
            decide("no_route", None)
        ]),
    );
    let w = required_text_empty(&ir);
    let at = Some("/params/route/min_confidence");
    assert_eq!(
        codes(&w),
        vec![
            (REQUIRED_TEXT_EMPTY, Some("missing"), at),
            (REQUIRED_TEXT_EMPTY, Some("null"), at)
        ]
    );
    assert_eq!(
        w[0].message,
        "「しきい値（AI の自信）」が空です。入れるまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(run_blockers(&ir).len(), 2);
    assert_eq!(enable_blockers(&ir).len(), 2);
}

/// D-83: 置いたばかりの選ぶ欄・文字の欄（調べる値・振り分ける値・繰り返すリスト・判定用のモデルと判定の
/// 材料・ファイル名と内容・URL）が空なら、保存の警告にとどめ、実行と自動実行のオンの前に止める。
/// 参照や中身のある値は止めない。文言は欄の見出しを名指しし、選ぶ欄は「選ぶまで」と言う。
#[test]
fn freshly_placed_blank_fields_block_run_and_enable() {
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            { "id": "branch", "type": "control.branch",
              "params": { "condition": { "cmp": { "left": "", "op": "eq", "right": "" } } } },
            { "id": "nested", "type": "control.branch",
              "params": { "condition": { "all": [
                  { "cmp": { "left": { "$from": "input", "path": "/kind" }, "op": "exists" } },
                  { "not": { "cmp": { "left": "  ", "op": "exists" } } }
              ] } } },
            { "id": "switch", "type": "control.switch", "params": { "value": "", "cases": [] } },
            { "id": "map", "type": "control.map", "params": { "items": "" } },
            { "id": "map_body", "type": "storage.read", "parent": "map",
              "params": { "file": { "$from": "each", "path": "/item" } } },
            { "id": "decide", "type": "ai.decide", "params": {
                "model": "", "state": "",
                "questions": [{ "key": "q1", "type": "boolean", "instructions": "請求か" }] } },
            { "id": "save", "type": "storage.write",
              "params": { "name": "", "content": "", "on_name_taken": "add_number" } },
            { "id": "csv", "type": "csv.write", "params": { "name": "", "content": "" } },
            { "id": "http", "type": "http.request", "params": { "url": "" } },
            // 選んだ値・書いた値は止めない。
            { "id": "ok_switch", "type": "control.switch",
              "params": { "value": { "$from": "input", "path": "/kind" }, "cases": [] } },
            { "id": "ok_save", "type": "storage.write",
              "params": { "name": "a.md", "content": { "$from": "input", "path": "/body" } } }
        ]),
    );
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (
                REQUIRED_TEXT_EMPTY,
                Some("branch"),
                Some("/params/condition/cmp/left")
            ),
            (
                REQUIRED_TEXT_EMPTY,
                Some("nested"),
                Some("/params/condition/all/1/not/cmp/left")
            ),
            (REQUIRED_TEXT_EMPTY, Some("switch"), Some("/params/value")),
            (REQUIRED_TEXT_EMPTY, Some("map"), Some("/params/items")),
            (REQUIRED_TEXT_EMPTY, Some("decide"), Some("/params/model")),
            (REQUIRED_TEXT_EMPTY, Some("decide"), Some("/params/state")),
            (REQUIRED_TEXT_EMPTY, Some("save"), Some("/params/name")),
            (REQUIRED_TEXT_EMPTY, Some("save"), Some("/params/content")),
            (REQUIRED_TEXT_EMPTY, Some("csv"), Some("/params/name")),
            (REQUIRED_TEXT_EMPTY, Some("csv"), Some("/params/content")),
            (REQUIRED_TEXT_EMPTY, Some("http"), Some("/params/url")),
        ]
    );
    assert_eq!(
        w[0].message,
        "「調べる値」が空です。選ぶまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(
        w[6].message,
        "「ファイル名」が空です。入れるまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(run_blockers(&ir).len(), w.len());
    assert_eq!(enable_blockers(&ir).len(), w.len());
}

/// BUG-151: 置いたばかりの業務データのブロック（テーブル・値・レコード・次の状態・行が空）は、保存の警告にとどめ、
/// 実行と自動実行のオンの前に止める。項目が無いのも空。中身の無い項目のまとまり・リスト（「項目を並べる」で 0 行）
/// は止めず、保存の注意（`ir.empty_values`）だけにする。選んだ値・参照・中身のある値は止めない。
#[test]
fn blank_data_block_fields_block_run_and_enable() {
    const T: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
    let r = json!({ "$from": "input", "path": "/id" });
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            { "id": "query", "type": "data.query", "params": {} },
            { "id": "add", "type": "data.record.create", "params": { "table": "", "data": "" } },
            { "id": "add_rows", "type": "data.record.create", "params": { "table": T, "data": {} } },
            { "id": "edit", "type": "data.record.update", "params": { "table": T, "record": " ", "patch": {} } },
            { "id": "del", "type": "data.record.delete", "params": { "table": T } },
            { "id": "move", "type": "data.transition", "params": { "table": T, "record": r, "to": "" } },
            { "id": "bulk", "type": "data.bulk_upsert", "params": { "table": T, "rows": [] } },
            // 選んだ値・参照・中身のある値は止めない。
            { "id": "ok_add", "type": "data.record.create",
              "params": { "table": T, "data": { "title": "a" } } },
            { "id": "ok_bulk", "type": "data.bulk_upsert",
              "params": { "table": { "$from": "input", "path": "/t" }, "rows": { "$from": "input", "path": "/rows" } } }
        ]),
    );
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (REQUIRED_TEXT_EMPTY, Some("query"), Some("/params/table")),
            (REQUIRED_TEXT_EMPTY, Some("add"), Some("/params/table")),
            (REQUIRED_TEXT_EMPTY, Some("add"), Some("/params/data")),
            (REQUIRED_TEXT_EMPTY, Some("edit"), Some("/params/record")),
            (REQUIRED_TEXT_EMPTY, Some("del"), Some("/params/record")),
            (REQUIRED_TEXT_EMPTY, Some("move"), Some("/params/to")),
        ]
    );
    // 中身の無い入れ物は、保存の注意だけ（止めない）。
    let e = empty_values(&ir);
    assert_eq!(
        codes(&e),
        vec![
            (EMPTY_VALUES, Some("add_rows"), Some("/params/data")),
            (EMPTY_VALUES, Some("edit"), Some("/params/patch")),
            (EMPTY_VALUES, Some("bulk"), Some("/params/rows")),
        ]
    );
    assert_eq!(
        e[0].message,
        "「レコードの値」に列がありません。このままでは、どの列にも値を入れずにレコードを追加します。"
    );
    assert_eq!(
        w[0].message,
        "「対象のテーブル」が空です。選ぶまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(
        w[2].message,
        "「レコードの値」が空です。入れるまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(run_blockers(&ir).len(), w.len());
    assert_eq!(enable_blockers(&ir).len(), w.len());
}

/// キーで追加か更新（GAP-16）の置いたばかりの欄（テーブル・探す列・入れる値）は、実行と自動実行のオンの前に止める。
/// 足す数だけで書き換えるレコードの更新は、変更する値が空でも止めない（何も書き換えないとも言わない）。
#[test]
fn upsert_and_add_only_updates_are_ready_when_filled() {
    const T: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
    let r = json!({ "$from": "input", "path": "/id" });
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            { "id": "up", "type": "data.record.upsert",
              "params": { "table": "", "match_on": "", "data": "" } },
            { "id": "ok_up", "type": "data.record.upsert",
              "params": { "table": T, "match_on": "sku", "data": { "sku": "A" } } },
            { "id": "add_only", "type": "data.record.update",
              "params": { "table": T, "record": r, "add": { "stock": -1 } } },
            { "id": "add_empty_patch", "type": "data.record.update",
              "params": { "table": T, "record": r, "patch": {}, "add": { "stock": -1 } } },
            { "id": "no_add", "type": "data.record.update",
              "params": { "table": T, "record": r, "add": {} } }
        ]),
    );
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (REQUIRED_TEXT_EMPTY, Some("up"), Some("/params/table")),
            (REQUIRED_TEXT_EMPTY, Some("up"), Some("/params/match_on")),
            (REQUIRED_TEXT_EMPTY, Some("up"), Some("/params/data")),
            (REQUIRED_TEXT_EMPTY, Some("no_add"), Some("/params/patch")),
        ]
    );
    assert_eq!(
        w[1].message,
        "「同じレコードを探す列」が空です。選ぶまで、実行も自動実行のオンもできません。"
    );
    assert!(empty_values(&ir).is_empty(), "{:?}", empty_values(&ir));
    assert!(required_pick_fields().contains(&("data.record.upsert", "match_on")));
}

/// BUG-151: ファイルを選ぶブロック（読むファイル・集計と編集の CSV ファイル・編集前の版・編集の内容）と、実行する
/// スキルも、置いたばかりの空のまま保存でき、実行と自動実行のオンの前に止める。選んだファイル・参照は止めない。
#[test]
fn blank_file_and_skill_fields_block_run_and_enable() {
    const F: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            { "id": "read", "type": "storage.read", "params": { "file": "" } },
            { "id": "sum", "type": "csv.query", "params": { "sql": "SELECT 1" } },
            { "id": "edit", "type": "csv.patch", "params": { "file": F, "ops": [] } },
            { "id": "skill", "type": "skill.invoke", "params": { "skill": "" } },
            { "id": "ok_read", "type": "storage.read",
              "params": { "file": { "$from": "input", "path": "/file" } } },
            { "id": "ok_edit", "type": "csv.patch",
              "params": { "file": F, "base_rev": 3, "ops": [{ "op": "row_delete", "row": 0 }] } }
        ]),
    );
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (REQUIRED_TEXT_EMPTY, Some("read"), Some("/params/file")),
            (REQUIRED_TEXT_EMPTY, Some("sum"), Some("/params/file")),
            (REQUIRED_TEXT_EMPTY, Some("edit"), Some("/params/base_rev")),
            (REQUIRED_TEXT_EMPTY, Some("skill"), Some("/params/skill")),
        ]
    );
    assert_eq!(
        codes(&empty_values(&ir)),
        vec![(EMPTY_VALUES, Some("edit"), Some("/params/ops"))]
    );
    assert_eq!(
        w[0].message,
        "「読むファイル」が空です。選ぶまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(run_blockers(&ir).len(), w.len());
    assert_eq!(enable_blockers(&ir).len(), w.len());
}

/// D-129: 置いたばかりの承認を待つ（件名・テーブル・行き先が空の文字・承認者が 0 件）は、保存の警告にとどめ、
/// 実行と自動実行のオンの前に止める。並びはエディタの欄の並び（件名・テーブル・承認者・判断の行き先）。文言は
/// 欄の見出しを名指しし、選ぶ欄は「選ぶまで」、承認者は「追加するまで」と言う。
#[test]
fn a_freshly_placed_approval_blocks_run_and_enable() {
    let placed = json!({ "id": "ok", "type": "human.approval", "params": {
        "title": "",
        "record": { "table": "", "id": { "$from": "trigger", "path": "/record_id" } },
        "assignees": [],
        "approve": { "to": "" }
    } });
    let ir = ir(json!([{ "kind": "interactive" }]), json!([placed]));
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (REQUIRED_TEXT_EMPTY, Some("ok"), Some("/params/title")),
            (
                REQUIRED_TEXT_EMPTY,
                Some("ok"),
                Some("/params/record/table")
            ),
            (REQUIRED_TEXT_EMPTY, Some("ok"), Some("/params/assignees")),
            (REQUIRED_TEXT_EMPTY, Some("ok"), Some("/params/approve/to")),
        ]
    );
    let messages: Vec<&str> = w.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "「受信箱の件名」が空です。入れるまで、実行も自動実行のオンもできません。",
            "「承認するテーブル」が空です。選ぶまで、実行も自動実行のオンもできません。",
            "「承認者」がいません。追加するまで、実行も自動実行のオンもできません。",
            "「承認したときに進める状態」が空です。選ぶまで、実行も自動実行のオンもできません。",
        ]
    );
    assert_eq!(codes(&run_blockers(&ir)), codes(&w));
    assert_eq!(codes(&enable_blockers(&ir)), codes(&w));
}

/// D-129: 承認者は、列を選んでいない承認者をその承認者として言い（実行すると列を読めずに失敗する）、値のある
/// 承認者が 1 人もいなければ欄として言う。空の文字のユーザー・部署は、ほかに承認者がいれば止めない（実行は
/// 数えずに動く）。使う却下・差し戻しの空の行き先も止める。埋めた承認は止めない。
#[test]
fn approval_assignees_and_optional_destinations_are_checked_when_blank() {
    const T: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
    let node = |id: &str, assignees: Value, extra: Value| {
        let mut n = json!({ "id": id, "type": "human.approval", "params": {
            "title": "経費申請の承認",
            "record": { "table": T, "id": { "$from": "trigger", "path": "/record_id" } },
            "assignees": assignees,
            "approve": { "to": "approved" }
        } });
        for (k, v) in extra.as_object().unwrap() {
            n["params"][k] = v.clone();
        }
        n
    };
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            node(
                "col",
                json!([{ "role": "keiri" }, { "field": "" }]),
                json!({})
            ),
            node("users", json!([{ "user": "" }, { "role": " " }]), json!({})),
            node(
                "exits",
                json!([{ "field": "approver" }]),
                json!({ "reject": { "to": "" }, "return": { "to": " " } })
            ),
            // 値のある承認者がいれば、空の文字のユーザーは止めない。参照の承認者も止めない。
            node(
                "ok",
                json!([{ "user": "" }, { "user": { "$from": "input", "path": "/boss" } }]),
                json!({})
            ),
            node(
                "ok_exits",
                json!([{ "field": "approver" }]),
                json!({ "reject": { "to": "rejected" } })
            )
        ]),
    );
    let w = required_text_empty(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (
                REQUIRED_TEXT_EMPTY,
                Some("col"),
                Some("/params/assignees/1/field")
            ),
            (
                REQUIRED_TEXT_EMPTY,
                Some("users"),
                Some("/params/assignees")
            ),
            (
                REQUIRED_TEXT_EMPTY,
                Some("exits"),
                Some("/params/reject/to")
            ),
            (
                REQUIRED_TEXT_EMPTY,
                Some("exits"),
                Some("/params/return/to")
            ),
        ]
    );
    assert_eq!(
        w[0].message,
        "「承認者 2 の列」が空です。選ぶまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(
        w[3].message,
        "「差し戻したときに進める状態」が空です。選ぶまで、実行も自動実行のオンもできません。"
    );
    assert_eq!(run_blockers(&ir).len(), w.len());
}

/// 選ぶ欄の表は、止める文言の「選ぶまで」と同じ欄を挙げる（エディタの欄の下の促しの出し分けに使う）。
#[test]
fn pick_fields_list_the_fields_blocked_until_picked() {
    let picks = required_pick_fields();
    for want in [
        ("data.record.create", "table"),
        ("data.transition", "to"),
        ("storage.read", "file"),
        ("skill.invoke", "skill"),
        ("workflow.start", "name"),
        // 承認を待つは、欄の先頭の項目の名前で挙げる（エディタは誤りの位置の先頭の項目で欄を引く・D-129）。
        ("human.approval", "record"),
        ("human.approval", "assignees"),
        ("human.approval", "approve"),
        ("human.approval", "reject"),
        ("human.approval", "return"),
    ] {
        assert!(picks.contains(&want), "{want:?}");
    }
    for text in [
        ("storage.write", "name"),
        ("data.record.create", "data"),
        ("llm.invoke", "prompt"),
        ("human.approval", "title"),
        ("workflow.reply", "value"),
    ] {
        assert!(!picks.contains(&text), "{text:?}");
    }
    // 同じ項目を 2 回挙げない。
    let mut seen = picks.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), picks.len());
}

/// 中身の無い入れ物（`data: {}`・`patch: {}`・`rows: []`・`ops: []`）は、実行も自動実行のオンも止めない。
/// 状態の進め方のあるテーブルに最初の状態だけのレコードを作る `data: {}` は正しい使い方で、前から保存されている
/// 定義（子の実行・ミニアプリが固定した版も同じ起動の道を通る）を止めない。
#[test]
fn empty_containers_do_not_block_run_or_enable() {
    const T: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
    let r = json!({ "$from": "input", "path": "/id" });
    let ir = ir(
        json!([{ "kind": "interactive" }, { "kind": "schedule", "cron": "0 9 * * 1", "tz": "Asia/Tokyo" }]),
        json!([
            { "id": "add", "type": "data.record.create", "params": { "table": T, "data": {} } },
            { "id": "edit", "type": "data.record.update", "params": { "table": T, "record": r, "patch": {} } },
            { "id": "bulk", "type": "data.bulk_upsert", "params": { "table": T, "rows": [] } },
            { "id": "csv", "type": "csv.patch", "params": { "file": T, "base_rev": 1, "ops": [] } }
        ]),
    );
    assert!(run_blockers(&ir).is_empty(), "{:?}", run_blockers(&ir));
    assert!(
        enable_blockers(&ir).is_empty(),
        "{:?}",
        enable_blockers(&ir)
    );
    assert_eq!(empty_values(&ir).len(), 4);
}

/// D-85: 中にブロックの無い繰り返しは、保存の警告にとどめ、実行と自動実行のオンの前に止める（置いた直後の
/// 繰り返しを保存できるように）。中にブロックのある繰り返し（入れ子の中の繰り返しも）は止めない。文言は
/// 繰り返しの名前（付けた名前・無ければ種類の名前）を名指しする。
#[test]
fn an_empty_map_blocks_run_and_enable() {
    let ir = ir(
        json!([{ "kind": "interactive" }]),
        json!([
            { "id": "named", "type": "control.map", "label": "申請ごとに",
              "params": { "items": { "$from": "input", "path": "/rows" } } },
            { "id": "unnamed", "type": "control.map",
              "params": { "items": { "$from": "input", "path": "/rows" } } },
            { "id": "outer", "type": "control.map",
              "params": { "items": { "$from": "input", "path": "/rows" } } },
            { "id": "inner", "type": "control.map", "parent": "outer",
              "params": { "items": { "$from": "each", "path": "/item" } } },
            { "id": "leaf", "type": "storage.read", "parent": "inner",
              "params": { "file": { "$from": "each", "path": "/item" } } }
        ]),
    );
    let w = empty_regions(&ir);
    assert_eq!(
        codes(&w),
        vec![
            (EMPTY_REGION, Some("named"), None),
            (EMPTY_REGION, Some("unnamed"), None)
        ]
    );
    assert_eq!(
        w[0].message,
        "「申請ごとに」の中にブロックがありません。1 件ごとに行う処理を中に置くまで、実行も自動実行のオンもできません。"
    );
    assert!(
        w[1].message.starts_with("「繰り返し」の中に"),
        "{}",
        w[1].message
    );
    // 必須の欄は埋まっているので、止める理由は中の空だけ。
    assert!(required_text_empty(&ir).is_empty());
    assert_eq!(codes(&run_blockers(&ir)), codes(&w));
    assert_eq!(codes(&enable_blockers(&ir)), codes(&w));
}

/// D-83: schedule / event で動く IR の、一覧にするフォルダを省略した storage.list は有効化を止める。対話だけの
/// IR と、フォルダを決めたものは止めない。
#[test]
fn unattended_lists_need_a_folder() {
    let nodes = json!([
        { "id": "ls", "type": "storage.list", "params": {} },
        { "id": "ls_null", "type": "storage.list", "params": { "folder": null } },
        { "id": "ls_ok", "type": "storage.list",
          "params": { "folder": "00000000-0000-0000-0000-000000000002" } }
    ]);
    let scheduled = ir(
        json!([{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]),
        nodes.clone(),
    );
    let blockers = enable_blockers(&scheduled);
    assert_eq!(
        codes(&blockers),
        vec![
            (LIST_FOLDER_REQUIRED, Some("ls"), Some("/params/folder")),
            (
                LIST_FOLDER_REQUIRED,
                Some("ls_null"),
                Some("/params/folder")
            ),
        ]
    );
    assert!(blockers[0]
        .message
        .starts_with("「一覧にするフォルダー」を選んでください。"));
    assert!(run_blockers(&scheduled).is_empty());
    assert!(enable_blockers(&ir(json!([{ "kind": "interactive" }]), nodes)).is_empty());
}

/// D-40: schedule / event で動く IR の保存先フォルダの省略（null も同じ）は有効化を止める。
/// 対話だけの IR は止めない（本人のルートに書ける）。フォルダを参照で決めるものは止めない。
#[test]
fn unattended_writes_need_a_folder() {
    let nodes = json!([
        { "id": "save", "type": "storage.write", "params": { "name": "a.txt", "content": "x" } },
        { "id": "save_null", "type": "storage.write", "params": { "folder": null, "name": "a.txt", "content": "x" } },
        { "id": "export", "type": "csv.write", "params": { "name": "a.csv", "content": "a,b" } },
        { "id": "save_ref", "type": "storage.write",
          "params": { "folder": { "$from": "input", "path": "/folder" }, "name": "a.txt", "content": "x" } },
        { "id": "save_lit", "type": "storage.write",
          "params": { "folder": "00000000-0000-0000-0000-000000000002", "name": "a.txt", "content": "x" } }
    ]);
    let scheduled = ir(
        json!([{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]),
        nodes.clone(),
    );
    assert_eq!(
        codes(&enable_blockers(&scheduled)),
        vec![
            (WRITE_FOLDER_REQUIRED, Some("save"), Some("/params/folder")),
            (
                WRITE_FOLDER_REQUIRED,
                Some("save_null"),
                Some("/params/folder")
            ),
            (
                WRITE_FOLDER_REQUIRED,
                Some("export"),
                Some("/params/folder")
            ),
        ]
    );
    assert!(enable_blockers(&scheduled)[0]
        .message
        .starts_with("保存先のフォルダを指定してください"));
    let on_save = ir(
        json!([{ "kind": "event", "source": "storage.write",
                 "scope": { "folder": "00000000-0000-0000-0000-000000000003" } }]),
        nodes.clone(),
    );
    assert_eq!(enable_blockers(&on_save).len(), 3);
    let interactive = ir(json!([{ "kind": "interactive" }]), nodes);
    assert!(enable_blockers(&interactive).is_empty());
    assert!(run_blockers(&scheduled).is_empty());
}
