//! 出力の形から決まる `$from` の path の照合（GAP-137）の試験。
#![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]

use serde_json::{json, Value};

use super::*;
use crate::ir::validate::{validate, Catalog, ValidationError};

const TABLE: &str = "00000000-0000-0000-0000-0000000000a1";
const SCRIPT: &str = "function main(i){ return i; }";

/// V5 だけを掛けた誤り。
fn v5(ir: Value) -> Vec<ValidationError> {
    let mut errs = Vec::new();
    super::super::check(&WorkflowIr::from_json(&ir).unwrap(), &mut errs);
    errs
}

/// 出力に無い所を指す誤りだけ。
fn unknown(errs: Vec<ValidationError>) -> Vec<ValidationError> {
    errs.into_iter()
        .filter(|e| e.code == UNKNOWN_OUTPUT_PATH)
        .collect()
}

/// 名前つきの data.query（`q`）。
fn query(query: Value) -> Value {
    json!({ "id": "q", "type": "data.query", "label": "貸出を調べる",
            "params": { "table": TABLE, "query": query } })
}

/// `q` の出力の `path` を指す参照。
fn at(path: &str) -> Value {
    json!({ "$from": "nodes.q.output", "path": path })
}

/// `target`（id は `q`）の後ろの script.run `use` が、`paths` を 1 つずつ読む IR で、誤りになった path。
fn flagged(target: Value, paths: &[&str]) -> Vec<String> {
    let refs: Vec<Value> = paths.iter().map(|p| at(p)).collect();
    let errs = unknown(v5(json!({
        "ir_version": 1, "name": "wf",
        "nodes": [
            target,
            { "id": "use", "type": "script.run",
              "params": { "source": { "inline": SCRIPT }, "input": { "v": refs } } }
        ],
        "edges": [{ "from": "q", "to": "use" }]
    })));
    errs.iter()
        .map(|e| {
            assert_eq!(e.node_id.as_deref(), Some("use"), "{e:?}");
            let i: usize = e
                .path
                .as_deref()
                .unwrap()
                .strip_prefix("/params/input/v/")
                .unwrap()
                .parse()
                .unwrap();
            paths[i].to_string()
        })
        .collect()
}

/// E1 の app11-return の形: 行の列を行の直下で読むと、分岐の条件が黙って偽になる。保存の誤りにし、行の列は data の
/// 下にあると言う。
#[test]
fn a_column_read_at_the_row_top_is_an_error_in_a_branch() {
    let wf = |path: &str| {
        json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                query(json!({ "limit": 1 })),
                { "id": "br", "type": "control.branch", "params": { "condition":
                    { "cmp": { "left": at(path), "op": "eq", "right": "貸出中" } } } }
            ],
            "edges": [{ "from": "q", "to": "br" }]
        })
    };
    let errs = v5(wf("/items/0/status"));
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].code, UNKNOWN_OUTPUT_PATH);
    assert_eq!(errs[0].node_id.as_deref(), Some("br"));
    assert_eq!(errs[0].path.as_deref(), Some("/params/condition/cmp/left"));
    assert_eq!(
        errs[0].message,
        "「貸出を調べる」（q）の結果の行に status はありません（行の項目: id, table_id, data, rev, owner, created_at, updated_at）。行の列の値は data の下にあります（/items/0/data/status）。条件の中では値が無いものとして扱われ、「等しい」はいつも偽、「等しくない」はいつも真になります。"
    );
    assert!(v5(wf("/items/0/data/status")).is_empty());
}

/// 結果の最上位は data の `QueryResult` の項目すべてを読める（説明文に無い項目も）。無い項目は誤り。
#[test]
fn the_query_result_top_follows_the_result_type() {
    let ok = [
        "/items",
        "/items/0",
        "/items/12/data/x",
        "/items/0/id",
        "/items/0/rev",
        "/items/0/data",
        "/items/0/link_labels",
        "/items/0/suppressed_fields",
        "/next_cursor",
        "/count",
        "/count/exact",
        "/suppressed",
        "/sort_precision",
        "/sort_degraded",
        "/shares_truncated",
        "/snapshot_seq",
    ];
    let q = query(json!({ "with_count": true }));
    assert!(flagged(q.clone(), &ok).is_empty());
    let ng = [
        "/status",
        "/rows",
        "/items/status",
        "/items/length",
        "/items/01/id",
        "/items/+1/id",
        "/items/-/id",
        "/items/0/status",
        "/items/0/data_status",
    ];
    assert_eq!(flagged(q, &ng), ng);
    // 最上位の文は結果の項目（説明文が教えるもの）と、行の列の読み方を言う。
    let errs = unknown(v5(json!({
        "ir_version": 1, "name": "wf",
        "nodes": [query(json!({})),
            { "id": "use", "type": "script.run",
              "params": { "source": { "inline": SCRIPT }, "input": at("/status") } }],
        "edges": [{ "from": "q", "to": "use" }]
    })));
    assert_eq!(
        errs[0].message,
        "「貸出を調べる」（q）の結果に status はありません（結果の項目: items, groups, next_cursor, count）。行の列の値は、行の data の下にあります（/items/0/data/status）。実行すると、このブロックは値を解決できずに失敗します。"
    );
    let errs = unknown(v5(json!({
        "ir_version": 1, "name": "wf",
        "nodes": [query(json!({})),
            { "id": "use", "type": "script.run",
              "params": { "source": { "inline": SCRIPT }, "input": at("/items/status") } }],
        "edges": [{ "from": "q", "to": "use" }]
    })));
    assert!(
        errs[0]
            .message
            .starts_with("「貸出を調べる」（q）の結果の items は行の並びなので、/items の次は 0 から数えた番号です（/items/0/data/<列>）。"),
        "{}",
        errs[0].message
    );
}

/// 件数は with_count を書いたときだけ入る（false も書いていないのと同じ）。
#[test]
fn count_is_there_only_with_with_count() {
    for (with_count, bad) in [
        (json!(true), false),
        (json!(false), true),
        (Value::Null, true),
    ] {
        let mut q = json!({ "limit": 5 });
        if !with_count.is_null() {
            q["with_count"] = with_count.clone();
        }
        assert_eq!(
            flagged(query(q), &["/count"]).len(),
            usize::from(bad),
            "{with_count}"
        );
    }
}

/// 集計を書いた data.query の結果は groups だけ・書いていなければ items だけ。集計の行は key・value・values。
#[test]
fn aggregate_decides_between_items_and_groups() {
    let agg = query(json!({ "group_by": ["region"], "aggregate": [{ "metric": "count" }] }));
    assert_eq!(
        flagged(
            agg.clone(),
            &[
                "/items",
                "/groups",
                "/groups/0/key/region",
                "/groups/0/value",
                "/groups/0/values/1",
                "/groups/0/total",
                "/groups/x",
                "/suppressed",
            ]
        ),
        ["/items", "/groups/0/total", "/groups/x"]
    );
    for plain in [json!({}), json!({ "aggregate": [] })] {
        assert_eq!(
            flagged(
                query(plain.clone()),
                &["/groups", "/groups/0/key", "/items"]
            ),
            ["/groups", "/groups/0/key"],
            "{plain}"
        );
    }
    // query を省いた data.query も、集計の無い読み方。
    let bare = json!({ "id": "q", "type": "data.query", "params": { "table": TABLE } });
    assert_eq!(flagged(bare, &["/groups", "/items/0/id"]), ["/groups"]);
}

/// `on_error: continue` の失敗の出力 `{ error }` は、data.query でも data.record.* でも読める。
#[test]
fn error_is_readable_only_with_on_error_continue() {
    let paths = ["/error", "/error/message"];
    for (ty, params) in [
        ("data.query", json!({ "table": TABLE, "query": {} })),
        (
            "data.record.create",
            json!({ "table": TABLE, "data": { "a": 1 } }),
        ),
        (
            "data.record.delete",
            json!({ "table": TABLE, "record": "r" }),
        ),
    ] {
        let node = |on_error: &str| json!({ "id": "q", "type": ty, "label": "読む", "on_error": on_error, "params": params });
        assert!(flagged(node("continue"), &paths).is_empty(), "{ty}");
        assert_eq!(flagged(node("fail_run"), &paths), paths, "{ty}");
    }
    let errs = unknown(v5(json!({
        "ir_version": 1, "name": "wf",
        "nodes": [query(json!({})),
            { "id": "use", "type": "script.run",
              "params": { "source": { "inline": SCRIPT }, "input": at("/error/message") } }],
        "edges": [{ "from": "q", "to": "use" }]
    })));
    assert!(
        errs[0].message.starts_with(
            "「貸出を調べる」（q）の結果に error はありません。error は、on_error を continue にしたブロックが失敗したときだけ入ります。"
        ),
        "{}",
        errs[0].message
    );
}

/// data.record.* の結果の最上位はレコード（upsert は created を足し、delete は id と deleted だけ）。
#[test]
fn record_results_follow_the_record_type() {
    let node = |ty: &str| json!({ "id": "q", "type": ty, "label": "書く", "params": {} });
    for ty in [
        "data.record.create",
        "data.record.update",
        "data.transition",
    ] {
        assert_eq!(
            flagged(
                node(ty),
                &[
                    "/data/x",
                    "/id",
                    "/rev",
                    "/updated_at",
                    "/status",
                    "/created"
                ]
            ),
            ["/status", "/created"],
            "{ty}"
        );
    }
    assert_eq!(
        flagged(
            node("data.record.upsert"),
            &["/created", "/data/x", "/status"]
        ),
        ["/status"]
    );
    assert_eq!(
        flagged(node("data.record.delete"), &["/deleted", "/id", "/data"]),
        ["/data"]
    );
    let errs = unknown(v5(json!({
        "ir_version": 1, "name": "wf",
        "nodes": [node("data.record.create"),
            { "id": "use", "type": "script.run",
              "params": { "source": { "inline": SCRIPT }, "input": at("/status") } }],
        "edges": [{ "from": "q", "to": "use" }]
    })));
    assert!(
        errs[0].message.starts_with(
            "「書く」（q）の結果のレコードに status はありません（レコードの項目: id, table_id, data, rev, owner, created_at, updated_at）。列の値は data の下にあります（/data/status）。"
        ),
        "{}",
        errs[0].message
    );
}

/// 形の決まらない種類（項目の表に漏れがある llm.invoke・戻り値そのものの script.run・繰り返しの結果・フォルダ
/// 一覧など）は照らさない。
#[test]
fn other_kinds_are_not_checked() {
    for (ty, params) in [
        ("llm.invoke", json!({ "prompt": "x" })),
        (
            "script.run",
            json!({ "source": { "inline": SCRIPT }, "input": {} }),
        ),
        ("control.map", json!({ "items": [] })),
        ("storage.list", json!({})),
        ("csv.query", json!({})),
        ("data.bulk_upsert", json!({})),
    ] {
        let node = json!({ "id": "q", "type": ty, "params": params });
        assert!(
            flagged(node, &["/stop_reason", "/anything/0/x"]).is_empty(),
            "{ty}"
        );
    }
}

/// 繰り返しの中で `lead` の繰り返しが `items` を回すとき、中のブロックが `$from: each` の `paths` を読む IR の誤り。
fn each_flagged(lead: Value, items: Value, paths: &[&str]) -> Vec<String> {
    let refs: Vec<Value> = paths
        .iter()
        .map(|p| json!({ "$from": "each", "path": p }))
        .collect();
    let errs = unknown(v5(json!({
        "ir_version": 1, "name": "wf",
        "nodes": [
            lead,
            { "id": "m", "type": "control.map", "params": { "items": items } },
            { "id": "use", "type": "script.run", "parent": "m",
              "params": { "source": { "inline": SCRIPT }, "input": { "v": refs } } }
        ],
        "edges": [{ "from": "q", "to": "m" }]
    })));
    errs.iter()
        .map(|e| {
            let i: usize = e
                .path
                .as_deref()
                .unwrap()
                .strip_prefix("/params/input/v/")
                .unwrap()
                .parse()
                .unwrap();
            paths[i].to_string()
        })
        .collect()
}

/// 繰り返し中の 1 件は item と index。並びが data.query の items なら 1 件はレコード、groups なら集計の行。
#[test]
fn each_follows_the_element_and_the_rows_it_walks() {
    let plain = query(json!({}));
    assert_eq!(
        each_flagged(
            plain.clone(),
            json!([1, 2]),
            &["/item", "/index", "/item/anything", "/value"]
        ),
        ["/value"]
    );
    assert_eq!(
        each_flagged(
            plain.clone(),
            at("/items"),
            &["/item/data/x", "/item/id", "/item/rev", "/item/status"]
        ),
        ["/item/status"]
    );
    let agg = query(json!({ "group_by": ["region"], "aggregate": [{ "metric": "count" }] }));
    assert_eq!(
        each_flagged(
            agg.clone(),
            at("/groups"),
            &[
                "/item/key/region",
                "/item/values/0",
                "/item/value",
                "/item/region",
                "/item/count"
            ]
        ),
        ["/item/region", "/item/count"]
    );
    // 集計を書いたのに items を回すときは繰り返しの側だけが誤り（1 件の側に重ねない）。
    let errs = unknown(v5(json!({
        "ir_version": 1, "name": "wf",
        "nodes": [
            agg,
            { "id": "m", "type": "control.map", "params": { "items": at("/items") } },
            { "id": "use", "type": "script.run", "parent": "m",
              "params": { "source": { "inline": SCRIPT },
                          "input": { "$from": "each", "path": "/item/status" } } }
        ],
        "edges": [{ "from": "q", "to": "m" }]
    })));
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].node_id.as_deref(), Some("m"));
    // 形の決まらない並び（CSV の集計・スクリプトの戻り値・入れ子の参照）の 1 件の中は照らさない。
    for (ty, path) in [
        ("csv.query", "/items"),
        ("script.run", "/rows"),
        ("data.query", ""),
    ] {
        let lead = json!({ "id": "q", "type": ty, "params": {} });
        assert!(
            each_flagged(lead, at(path), &["/item/status", "/item/x/y"]).is_empty(),
            "{ty}"
        );
    }
}

/// 文は 1 件の形を言う（行の列は data の下・集計の行は key と values）。
#[test]
fn each_messages_name_the_row_shape() {
    let msg = |lead: Value, items: Value, path: &str| {
        let errs = unknown(v5(json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                lead,
                { "id": "m", "type": "control.map", "params": { "items": items } },
                { "id": "use", "type": "script.run", "parent": "m",
                  "params": { "source": { "inline": SCRIPT },
                              "input": { "$from": "each", "path": path } } }
            ],
            "edges": [{ "from": "q", "to": "m" }]
        })));
        assert_eq!(errs.len(), 1, "{errs:?}");
        errs[0].message.clone()
    };
    assert_eq!(
        msg(query(json!({})), json!([]), "/value"),
        "繰り返し中の 1 件に value はありません（項目: item, index）。要素の中は item の下にあります（/item/value）。実行すると、このブロックは値を解決できずに失敗します。"
    );
    assert_eq!(
        msg(query(json!({})), at("/items"), "/item/status"),
        "繰り返し中の 1 件（「貸出を調べる」（q）の結果の行）に status はありません（行の項目: id, table_id, data, rev, owner, created_at, updated_at）。行の列の値は data の下にあります（/item/data/status）。実行すると、このブロックは値を解決できずに失敗します。"
    );
    let agg = query(json!({ "group_by": ["region"], "aggregate": [{ "metric": "count" }] }));
    assert!(
        msg(agg, at("/groups"), "/item/region").starts_with(
            "繰り返し中の 1 件（「貸出を調べる」（q）の集計の行）に region はありません（集計の行の項目: key, value, values）。まとめた列の値は key の下（/item/key/<列>）"
        )
    );
}

/// 文の後ろは、解決しないときの実行の振る舞いを置き場所で言い分ける（条件の比較の値そのもの・値の位置・既定値つき）。
#[test]
fn the_consequence_depends_on_where_the_ref_is() {
    let cond = "条件の中では値が無いものとして扱われ、「等しい」はいつも偽、「等しくない」はいつも真になります。";
    let fails = "実行すると、このブロックは値を解決できずに失敗します。";
    let fallback = "この参照は値を解決できないので、いつも既定値（default）が使われます。";
    let bad = at("/items/0/status");
    let mut with_default = bad.clone();
    with_default["default"] = json!("");
    let wf = |br: Value, after: Value| {
        json!({
            "ir_version": 1, "name": "wf",
            "nodes": [query(json!({})),
                { "id": "br", "type": "control.branch", "params": br },
                { "id": "w", "type": "control.wait", "params": after }],
            "edges": [{ "from": "q", "to": "br" }, { "from": "br", "from_port": "true", "to": "w" }]
        })
    };
    let ends = |br: Value, after: Value| -> Vec<(String, bool, bool, bool)> {
        unknown(v5(wf(br, after)))
            .into_iter()
            .map(|e| {
                (
                    e.path.unwrap(),
                    e.message.ends_with(cond),
                    e.message.ends_with(fails),
                    e.message.ends_with(fallback),
                )
            })
            .collect()
    };
    let got = ends(
        json!({ "condition": { "any": [
            { "cmp": { "left": bad.clone(), "op": "neq", "right": "x" } },
            { "cmp": { "left": "x", "op": "eq", "right": bad.clone() } },
            { "cmp": { "left": "x", "op": "in", "right": [bad.clone()] } },
            { "cmp": { "left": with_default.clone(), "op": "eq", "right": "x" } },
            { "cmp": { "left": { "$template": "{s}", "vars": { "s": bad.clone() } }, "op": "eq", "right": "x" } }
        ] } }),
        // 待つ条件の参照は止まるときにリテラルへ束縛する（解決しなければ止まれずに失敗する）。
        json!({ "kind": "event", "source": "storage.write", "scope": {},
                "filter": { "cmp": { "left": bad.clone(), "op": "eq", "right": "x" } } }),
    );
    let c = "/params/condition/any";
    assert_eq!(
        got,
        [
            (format!("{c}/0/cmp/left"), true, false, false),
            (format!("{c}/1/cmp/right"), true, false, false),
            (format!("{c}/2/cmp/right/0"), false, true, false),
            (format!("{c}/3/cmp/left"), false, false, true),
            (format!("{c}/4/cmp/left/vars/s"), false, true, false),
            ("/params/filter/cmp/left".to_string(), false, true, false),
        ]
    );
}

/// 参照そのものの誤り（map の外の each・祖先でない参照）には、出力の形の誤りを重ねない。祖先でなくても default が
/// あれば照らす。
#[test]
fn ref_errors_are_not_doubled() {
    let errs = v5(json!({
        "ir_version": 1, "name": "wf",
        "nodes": [
            query(json!({})),
            { "id": "out", "type": "script.run",
              "params": { "source": { "inline": SCRIPT },
                          "input": { "$from": "each", "path": "/value" } } },
            { "id": "far", "type": "script.run",
              "params": { "source": { "inline": SCRIPT }, "input": at("/status") } },
            { "id": "soft", "type": "script.run",
              "params": { "source": { "inline": SCRIPT },
                          "input": { "$from": "nodes.q.output", "path": "/status", "default": 0 } } },
            { "id": "bad_path", "type": "script.run",
              "params": { "source": { "inline": SCRIPT },
                          "input": { "$from": "nodes.q.output", "path": "status" } } }
        ],
        "edges": [{ "from": "q", "to": "bad_path" }]
    }));
    let got: Vec<(&str, &str)> = errs
        .iter()
        .map(|e| (e.node_id.as_deref().unwrap(), e.code.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("out", "ir.bad_ref"),
            ("far", "ir.bad_ref"),
            ("soft", UNKNOWN_OUTPUT_PATH),
            ("bad_path", "ir.bad_ref"),
        ]
    );
}

/// 保存の検証の全体でも出る（typed の params の data.query を通る形）。query の中の値（絞り込みの値）が `$from`
/// でも、集計を書いたかどうかはリテラルで決まるので照らす。
#[test]
fn validate_reports_it_through_the_typed_query() {
    let ir = json!({
        "ir_version": 1, "name": "wf", "declared_scopes": ["data.read"],
        "input_schema": { "type": "object", "properties": { "s": { "type": "string" } } },
        "triggers": [{ "kind": "interactive" }],
        "nodes": [
            { "id": "q", "type": "data.query", "params": { "table": TABLE, "query": {
                "filter": { "cmp": { "field": "status", "op": "eq",
                                     "value": { "$from": "input", "path": "/s" } } } } } },
            { "id": "br", "type": "control.branch", "params": { "condition":
                { "cmp": { "left": at("/groups"), "op": "exists" } } } }
        ],
        "edges": [{ "from": "q", "to": "br" }]
    });
    let errs = validate(&ir, &Catalog::default()).unwrap_err();
    assert_eq!(
        errs.iter().map(|e| e.code.as_str()).collect::<Vec<_>>(),
        [UNKNOWN_OUTPUT_PATH],
        "{errs:?}"
    );
    assert!(
        errs[0]
            .message
            .starts_with("「データを調べる」（q）は集計（aggregate）を書いていないので、結果に groups はありません。"),
        "{}",
        errs[0].message
    );
}

/// 番号の段は serde_json の JSON Pointer の読み方と同じ（`0`・0 で始まらない数字の並びだけ）。
#[test]
fn index_segments_follow_the_json_pointer() {
    let rows = Value::Array(vec![Value::Null; 11]);
    for s in [
        "0", "1", "10", "01", "00", "+1", "-", "-1", " 1", "1a", "", "١",
    ] {
        assert_eq!(
            is_index(s),
            rows.pointer(&format!("/{s}")).is_some(),
            "{s:?}"
        );
    }
    assert!(is_index("123456"));
    assert!(!is_index("99999999999999999999999"));
}

/// path の段は JSON Pointer のエスケープを戻して照らす。空の path（全体）と / で始まらない path は照らさない。
#[test]
fn segments_unescape_and_skip_whole_outputs() {
    assert_eq!(segments(""), None);
    assert_eq!(segments("items"), None);
    assert_eq!(
        segments("/a~1b/c~0d/0"),
        Some(vec!["a/b".to_string(), "c~d".to_string(), "0".to_string()])
    );
    assert!(flagged(query(json!({})), &["", "/items/0/data/a~1b"]).is_empty());
}
