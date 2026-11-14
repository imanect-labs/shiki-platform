//! 出口の導出の試験（共有の fixture・V2 のポート存在検査）。

use serde_json::{json, Value};

use super::*;
use crate::ir::validate::ValidationError;

/// 共有の fixture（api の検証 API の試験も同じデータを読む）。
const PORT_CASES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/port_cases.json"
));

fn node(v: &Value) -> Node {
    serde_json::from_value(v.clone()).unwrap()
}

/// fixture の各ノードの出口は期待どおり（順序も含む・導けないものは None）。
#[test]
fn shared_fixture_cases_match() {
    let doc: Value = serde_json::from_str(PORT_CASES).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    let mut ids = std::collections::BTreeSet::new();
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let n = node(&c["node"]);
        assert!(ids.insert(n.id.clone()), "{name}: id が重なる");
        let want: Option<Vec<String>> = serde_json::from_value(c["ports"].clone()).unwrap();
        assert_eq!(output_ports(&n), want, "{name}");
    }
}

/// node_ports は IR の各ノードを同じ関数で導き、導けないノードは載せない。
#[test]
fn node_ports_covers_every_derivable_node() {
    let doc: Value = serde_json::from_str(PORT_CASES).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    let nodes: Vec<Value> = cases.iter().map(|c| c["node"].clone()).collect();
    let underivable = cases.iter().filter(|c| c["ports"].is_null()).count();
    let ir = WorkflowIr::from_json(&json!({
        "ir_version": 1, "name": "wf", "nodes": nodes, "edges": []
    }))
    .unwrap();
    let got = node_ports(&ir);
    assert_eq!(
        got["decide_value"],
        ["billing", "bug", "other", "uncertain"]
    );
    assert!(!got.contains_key("decide_bad"));
    assert!(!got.contains_key("decide_dup"));
    assert_eq!(got["decide_draft"], ["billing", "bug", "uncertain"]);
    assert_eq!(got.len(), ir.nodes.len() - underivable);
}

/// 2 ノード（`a` の出口 `port` → `b`）の IR を V2 に通し、ir.unknown_port のエラーを返す。
#[allow(clippy::needless_pass_by_value)] // json! をそのまま渡す。
fn unknown_ports(a: Value, port: &str) -> Vec<ValidationError> {
    let mut errs = Vec::new();
    super::super::v2_graph::check(
        &WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf",
            "nodes": [ a, { "id": "b", "type": "storage.read", "params": {} } ],
            "edges": [ { "from": "a", "from_port": port, "to": "b" } ]
        }))
        .unwrap(),
        &mut errs,
    );
    errs.retain(|e| e.code == "ir.unknown_port");
    errs
}

fn decide(route: Value) -> Value {
    let mut params = json!({ "model": "decide-stub", "state": { "$from": "input" },
        "questions": [
            { "key": "category", "type": "choice", "instructions": "種類はどれか",
              "options": [ { "key": "billing" }, { "key": "bug" } ] },
            { "key": "angry", "type": "boolean", "instructions": "強い不満があるか" } ] });
    if !route.is_null() {
        params["route"] = route;
    }
    json!({ "id": "a", "type": "ai.decide", "params": params })
}

/// 値で振り分ける判定から out・default に張ったエッジは発火しないので保存で拒否する（switch の規則に乗せない）。
#[test]
fn a_value_routed_decide_rejects_out_and_default_edges() {
    let by_value = decide(json!({ "question": "category", "min_confidence": 0.8 }));
    for bad in ["out", "default", "true"] {
        let errs = unknown_ports(by_value.clone(), bad);
        assert_eq!(errs.len(), 1, "{bad}: {errs:?}");
        assert!(
            errs[0]
                .message
                .contains("使える出口: billing, bug, uncertain"),
            "{}",
            errs[0].message
        );
        assert!(errs[0].message.contains("route"), "{}", errs[0].message);
    }
    for ok in ["billing", "bug", "uncertain"] {
        assert!(unknown_ports(by_value.clone(), ok).is_empty(), "{ok}");
    }
    let by_bool = decide(json!({ "question": "angry", "min_confidence": 0.7 }));
    for ok in ["true", "false", "uncertain"] {
        assert!(unknown_ports(by_bool.clone(), ok).is_empty(), "{ok}");
    }
    assert_eq!(unknown_ports(by_bool, "out").len(), 1);
    let by_conf =
        decide(json!({ "question": "category", "by": "confidence", "min_confidence": 0.8 }));
    for ok in ["out", "uncertain"] {
        assert!(unknown_ports(by_conf.clone(), ok).is_empty(), "{ok}");
    }
    assert_eq!(unknown_ports(by_conf, "billing").len(), 1);
    // 振り分けが無ければ out だけ（uncertain は振り分けを書いたときだけ）。
    let plain = decide(Value::Null);
    assert!(unknown_ports(plain.clone(), "out").is_empty());
    let errs = unknown_ports(plain, "uncertain");
    assert_eq!(errs.len(), 1);
    assert!(errs[0].message.contains("route"), "{}", errs[0].message);
}

/// params が契約に合わない判定は V2 で出口を検査しない（V1 だけが報告する・同じ誤りを二重に出さない）。
#[test]
fn an_invalid_decide_is_left_to_v1() {
    let bad = decide(json!({ "question": "nope", "min_confidence": 0.8 }));
    assert!(unknown_ports(bad, "anything").is_empty());
}

/// ほかの質問を書きかけ（問いが空）でも、振り分けの出口は決まり、V2 は無い出口（out）へのエッジを報告する
/// （エディタが古い出口を描き続けて、利用者が out へつながない）。
#[test]
fn a_draft_question_does_not_hide_the_routed_ports() {
    let mut draft = decide(json!({ "question": "category", "min_confidence": 0.8 }));
    draft["params"]["questions"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "key": "q3", "type": "boolean", "instructions": "" }));
    assert_eq!(
        output_ports(&node(&draft)).unwrap(),
        ["billing", "bug", "uncertain"]
    );
    assert_eq!(unknown_ports(draft.clone(), "out").len(), 1);
    assert!(unknown_ports(draft, "billing").is_empty());
}
