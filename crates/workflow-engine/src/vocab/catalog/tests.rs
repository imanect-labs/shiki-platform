//! ノードカタログの単一定義の試験（行数ゲート対応で catalog.rs から分離）。

use super::output_shape::{
    AGGREGATE_GROUP_KEYS, DATA_RECORD_KEYS, DELETE_RESULT_KEYS, EACH_KEYS, QUERY_RESULT_KEYS,
    UPSERT_RESULT_KEYS,
};
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
            "provider",
            "calibrated",
            "method",
            "samples",
            "cached",
            "routed",
            "raised_questions"
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
    // フォルダ一覧は上限で切ったことを truncated で言い、続きの位置を next_cursor で返す（BUG-180・D-131）。文の件数は
    // 上限の単一定義と同じ。
    let list = find(NodeType::StorageList);
    assert_eq!(
        list.output_shape,
        "{ items: Array<{ id: string, name: string, kind: string, size: number }>（名前の順・最大 100 件・kind は file か folder）, truncated: boolean（続きがあるとき true・残りは items に入らない）, next_cursor: string | null（続きの位置・同じフォルダーで cursor に渡すと続きを読む・続きが無ければ null） }"
    );
    let max = format!("{} 件", crate::vocab::STORAGE_LIST_MAX_ITEMS);
    assert!(list.output_fields[0].note.contains(&max), "{max}");
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

/// BUG-108: 設定で決まる項目は、その設定の場所を持つ（画面が値の候補を出し分ける）。場所はその種類の params に
/// 実在する（params の型が受け付ける）。
#[test]
fn fields_that_depend_on_params_say_where() {
    let catalog = node_catalog();
    let present_if = |nt: NodeType, name: &str| {
        catalog
            .iter()
            .find(|e| e.node_type == nt)
            .and_then(|e| e.output_fields.iter().find(|f| f.name == name))
            .map(|f| f.present_if)
            .unwrap()
    };
    assert_eq!(
        present_if(NodeType::DataQuery, "count"),
        "/query/with_count"
    );
    assert_eq!(
        present_if(NodeType::DataQuery, "groups"),
        "/query/aggregate"
    );
    assert_eq!(present_if(NodeType::DataQuery, "items"), "");
    assert_eq!(present_if(NodeType::LlmInvoke, "data"), "/output_schema");
    // 集計（aggregate）を書くと、data は items も count も返さない（`query::run` の集計の枝・7o の指摘）。
    let absent_if = |nt: NodeType, name: &str| {
        catalog
            .iter()
            .find(|e| e.node_type == nt)
            .and_then(|e| e.output_fields.iter().find(|f| f.name == name))
            .map(|f| f.absent_if)
            .unwrap()
    };
    assert_eq!(absent_if(NodeType::DataQuery, "items"), "/query/aggregate");
    assert_eq!(absent_if(NodeType::DataQuery, "count"), "/query/aggregate");
    assert_eq!(absent_if(NodeType::DataQuery, "groups"), "");
    for e in &catalog {
        for f in e.output_fields {
            for at in [f.present_if, f.absent_if] {
                if !at.is_empty() {
                    assert!(
                        at.starts_with('/') && f.optional,
                        "{}: {}",
                        e.node_type.as_str(),
                        f.name
                    );
                }
            }
        }
    }
    // 場所は params の型が受け付ける（綴りの誤りで、画面が候補を出さなくならない）。
    let table = "00000000-0000-0000-0000-000000000001";
    let q: crate::ir::params::DataQueryParams = serde_json::from_value(serde_json::json!({
        "table": table,
        "query": { "with_count": true, "group_by": ["k"], "aggregate": [{ "metric": "count" }] }
    }))
    .unwrap();
    let q = q.query.unwrap();
    assert!(q.with_count && !q.aggregate.is_empty());
}

/// GAP-132: data.query の集計の行の形は、data の `AggregateGroup` の項目をすべて名指しする（ノードは data の型を
/// そのまま出す。項目を足したり名前を変えたりしたら、AI が読む形も直す）。
#[test]
fn data_query_groups_name_every_field_of_the_aggregate_row() {
    let catalog = node_catalog();
    let groups = catalog
        .iter()
        .find(|e| e.node_type == NodeType::DataQuery)
        .and_then(|e| e.output_fields.iter().find(|f| f.name == "groups"))
        .unwrap();
    let row = serde_json::to_value(data::AggregateGroup {
        key: serde_json::Map::new(),
        value: serde_json::Value::Null,
        values: Vec::new(),
    })
    .unwrap();
    let keys: Vec<&String> = row.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["key", "value", "values"]);
    for k in keys {
        assert!(groups.ty.contains(&format!(" {k}: ")), "{k}: {}", groups.ty);
    }
    // 説明文（output_shape）にも同じ形が出る。
    let shape = &catalog
        .iter()
        .find(|e| e.node_type == NodeType::DataQuery)
        .unwrap()
        .output_shape;
    assert!(
        shape.contains(groups.ty) && shape.contains("values は aggregate と同じ順"),
        "{shape}"
    );
}

/// GAP-112: 設定で決まる項目の「値がある」の判定（`present_if` の約束・画面の `hasParamAt` と同じ）。場所か、そこへ
/// 至る途中が値式（`$from`・`$template`）なら、設定からは決まらない（`None`）。
#[test]
fn present_in_follows_the_present_if_contract() {
    let field = |at: &'static str| OutputField {
        name: "x",
        ty: "unknown",
        optional: true,
        note: "",
        present_if: at,
        absent_if: "",
    };
    let params = serde_json::json!({ "a": 1, "z": 0, "t": "x", "e": "", "f": false, "n": null,
        "l": [1], "el": [], "o": { "k": 1 }, "eo": {}, "nested": { "x": [0] },
        "q": { "$from": "input", "path": "/q" }, "tpl": { "$template": "{a}", "vars": { "a": 1 } },
        "deep": { "agg": { "$from": "input" } } });
    for at in ["", "/a", "/z", "/t", "/l", "/o", "/nested/x", "/nested/x/0"] {
        assert_eq!(field(at).present_in(&params), Some(true), "{at}");
    }
    for at in [
        "/e",
        "/f",
        "/n",
        "/el",
        "/eo",
        "/missing",
        "/nested/y",
        "/nested/x/1",
        "/a/b",
    ] {
        assert_eq!(field(at).present_in(&params), Some(false), "{at}");
    }
    for at in ["/q", "/q/aggregate", "/tpl", "/deep/agg", "/deep/agg/0"] {
        assert_eq!(field(at).present_in(&params), None, "{at}");
    }
    // カタログの項目でも同じ（data.query の groups は集計を書いたときだけ）。
    let groups = NodeType::DataQuery
        .output_fields()
        .iter()
        .find(|f| f.name == "groups")
        .unwrap();
    let with = |query: serde_json::Value| serde_json::json!({ "table": "t", "query": query });
    assert_eq!(
        groups.present_in(&with(
            serde_json::json!({ "aggregate": [{ "metric": "count" }] })
        )),
        Some(true)
    );
    assert_eq!(
        groups.present_in(&with(serde_json::json!({ "aggregate": [] }))),
        Some(false)
    );
    assert_eq!(
        groups.present_in(&with(serde_json::json!({ "$from": "input" }))),
        None
    );
}

/// 設定で消える項目（`absent_if`）の判定。場所に値があれば入らない（present_if の結果にかかわらず）。場所に値が
/// 無ければ present_if の結果のまま。場所が値式なら、present_if が入らないと決めていない限り決まらない（`None`）。
#[test]
fn present_in_follows_the_absent_if_contract() {
    let field = |present_if: &'static str, absent_if: &'static str| OutputField {
        name: "x",
        ty: "unknown",
        optional: true,
        note: "",
        present_if,
        absent_if,
    };
    let params = serde_json::json!({ "on": true, "off": false, "el": [],
        "q": { "$from": "input", "path": "/q" } });
    for (present_if, absent_if, want) in [
        ("", "/on", Some(false)),
        ("", "/off", Some(true)),
        ("", "/el", Some(true)),
        ("", "/missing", Some(true)),
        ("", "/q", None),
        ("/on", "/on", Some(false)),
        ("/on", "/off", Some(true)),
        ("/off", "/off", Some(false)),
        ("/off", "/q", Some(false)),
        ("/on", "/q", None),
        ("/q", "/on", Some(false)),
        ("/q", "/off", None),
    ] {
        let f = field(present_if, absent_if);
        assert_eq!(
            f.present_in(&params),
            want,
            "present_if={present_if} absent_if={absent_if}"
        );
        // absent_in は、absent_if の場所に値があると決まるときだけ真。
        assert_eq!(
            f.absent_in(&params),
            absent_if == "/on",
            "present_if={present_if} absent_if={absent_if}"
        );
    }
    assert!(!field("", "").absent_in(&params));
    // カタログの data.query: 集計を書くと items と count は入らない（with_count を書いても）。
    let fields = NodeType::DataQuery.output_fields();
    let get = |n: &str| fields.iter().find(|f| f.name == n).unwrap();
    let with = |query: serde_json::Value| serde_json::json!({ "table": "t", "query": query });
    let agg = serde_json::json!([{ "metric": "count" }]);
    for (query, items, count, groups) in [
        (serde_json::json!({}), true, false, false),
        (serde_json::json!({ "with_count": true }), true, true, false),
        (
            serde_json::json!({ "with_count": true, "aggregate": [] }),
            true,
            true,
            false,
        ),
        (serde_json::json!({ "aggregate": agg }), false, false, true),
        (
            serde_json::json!({ "with_count": true, "aggregate": agg }),
            false,
            false,
            true,
        ),
    ] {
        let p = with(query.clone());
        assert_eq!(get("items").present_in(&p), Some(items), "{query}");
        assert_eq!(get("count").present_in(&p), Some(count), "{query}");
        assert_eq!(get("groups").present_in(&p), Some(groups), "{query}");
        assert_eq!(get("next_cursor").present_in(&p), Some(true), "{query}");
    }
}

/// 直列化した値の最上位のキー（順は問わない）。
fn keys_of(v: &serde_json::Value) -> std::collections::BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn set(keys: &[&str]) -> std::collections::BTreeSet<String> {
    keys.iter().map(|k| (*k).to_string()).collect()
}

/// 空でない値を入れたレコード（省かれる項目も出す）。
fn full_record() -> data::DataRecord {
    data::DataRecord {
        id: uuid::Uuid::nil(),
        table_id: uuid::Uuid::nil(),
        data: serde_json::json!({ "a": 1 }),
        rev: 1,
        owner: "alice".into(),
        created_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        updated_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        suppressed_fields: vec!["sum".into()],
        link_labels: std::collections::BTreeMap::from([("r1".into(), "東京".into())]),
    }
}

/// GAP-137: 保存時に path を照らす閉じた形は、出力を組む data の型の直列化の項目と一致する（型に項目を足したら、
/// ここが落ちて表を直させる）。説明文が教える項目（`output_fields`）は閉じた形に含まれる。
#[test]
fn closed_output_shapes_match_the_data_types() {
    let record = serde_json::to_value(full_record()).unwrap();
    assert_eq!(keys_of(&record), set(DATA_RECORD_KEYS));
    let group = serde_json::to_value(data::AggregateGroup {
        key: serde_json::Map::new(),
        value: serde_json::Value::Null,
        values: Vec::new(),
    })
    .unwrap();
    assert_eq!(keys_of(&group), set(AGGREGATE_GROUP_KEYS));
    let result = serde_json::to_value(data::QueryResult {
        items: Some(vec![full_record()]),
        groups: Some(Vec::new()),
        next_cursor: Some("c".into()),
        count: Some(data::CountResult::Exact(1)),
        sort_precision: data::SortPrecision::Exact,
        sort_degraded: false,
        shares_truncated: false,
        suppressed: false,
        snapshot_seq: Some(1),
    })
    .unwrap();
    assert_eq!(keys_of(&result), set(QUERY_RESULT_KEYS));
    // upsert はレコードに created を足し、delete は id と deleted だけ。
    let mut upsert = set(DATA_RECORD_KEYS);
    upsert.insert("created".into());
    assert_eq!(set(UPSERT_RESULT_KEYS), upsert);
    assert_eq!(set(DELETE_RESULT_KEYS), set(&["id", "deleted"]));
    // 照らす種類と、行・1 件の表。
    for nt in NodeType::ALL {
        let Some(keys) = nt.closed_output_keys() else {
            continue;
        };
        for f in nt.output_fields() {
            assert!(keys.contains(&f.name), "{}: {}", nt.as_str(), f.name);
        }
    }
    let checked: Vec<&str> = NodeType::ALL
        .iter()
        .filter(|nt| nt.closed_output_keys().is_some())
        .map(|nt| nt.as_str())
        .collect();
    assert_eq!(
        checked,
        [
            "data.query",
            "data.record.create",
            "data.record.update",
            "data.record.upsert",
            "data.transition",
            "data.record.delete"
        ]
    );
    assert_eq!(
        NodeType::DataQuery.closed_row_keys("items"),
        Some(DATA_RECORD_KEYS)
    );
    assert_eq!(
        NodeType::DataQuery.closed_row_keys("groups"),
        Some(AGGREGATE_GROUP_KEYS)
    );
    assert_eq!(NodeType::DataQuery.closed_row_keys("count"), None);
    assert_eq!(NodeType::ControlMap.element_keys(), Some(EACH_KEYS));
    assert_eq!(NodeType::ScriptRun.element_keys(), None);
}
