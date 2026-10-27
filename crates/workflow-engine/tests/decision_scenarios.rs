//! 判定の受け入れシナリオのテンプレート（`eval/decision/scenarios/*.json`・FEAT-1 の T8）が、今の IR の契約で
//! 保存できる形のままであることを確かめる（DB は使わない）。
//!
//! - プレースホルダ（`{{table.<ref>}}`・`{{model.decide}}`・`{{role.*}}`）を仮の値に置き換えると、保存時検証
//!   V1〜V7 を誤りなく通り、保存を止めない警告も出ない（スコープは保存の入口と同じく補ってから検証する）。
//! - ファイルが使うプレースホルダは、どれも `placeholders` に説明がある（説明だけで使わないものも無い）。
//!   表の `ref` と同意（grants）の表の名前が合っている。
//! - 判定のノードの質問は評価の質問のファイル（`eval/decision/datasets/*.questions.json`）と同じで、しきい値は
//!   凍結（`frozen.json`）の値。評価で測ったものと違う質問・しきい値でシナリオを流さない（PIT-80）。
//! - 判定のノードの出口は、確からしさで振り分ける `out`・`uncertain` と、失敗を人へ回す `error`。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use workflow_engine::{complete_declared_scopes, node_ports, save_warnings, validate, Catalog};

const SCENARIOS: [&str; 2] = ["inquiry_triage", "expense_policy"];

fn eval_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eval/decision")
}

fn read_json(rel: &str) -> Value {
    let path = eval_dir().join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn scenario(name: &str) -> Value {
    read_json(&format!("scenarios/{name}.json"))
}

/// 値の中の文字列に現れる `{{...}}` をすべて集める。
fn placeholders_in(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(start) = rest.find("{{") {
                let Some(len) = rest[start..].find("}}") else {
                    break;
                };
                out.insert(rest[start..start + len + 2].to_string());
                rest = &rest[start + len + 2..];
            }
        }
        Value::Array(xs) => xs.iter().for_each(|x| placeholders_in(x, out)),
        Value::Object(m) => m.values().for_each(|x| placeholders_in(x, out)),
        _ => {}
    }
}

/// プレースホルダを仮の値にする（表は UUID・判定用のモデルは評価と同じ id・部署は名前）。
fn fill(v: &Value) -> Value {
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for (i, t) in ["inquiries", "routing_rules", "expense_claims"]
                .iter()
                .enumerate()
            {
                out = out.replace(
                    &format!("{{{{table.{t}}}}}"),
                    &format!("00000000-0000-4000-8000-00000000000{}", i + 1),
                );
            }
            out = out.replace("{{model.decide}}", "decide-haiku");
            for role in ["billing", "support", "sales", "accounting"] {
                out = out.replace(&format!("{{{{role.{role}}}}}"), &format!("team-{role}"));
            }
            Value::String(out)
        }
        Value::Array(xs) => Value::Array(xs.iter().map(fill).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), fill(x))).collect()),
        other => other.clone(),
    }
}

fn decide_nodes(ir: &Value) -> Vec<&Value> {
    ir["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["type"] == "ai.decide")
        .collect()
}

#[test]
fn templates_pass_the_save_validation_without_warnings() {
    for name in SCENARIOS {
        let s = scenario(name);
        let (ir, _) = complete_declared_scopes(&fill(&s["workflow"]));
        let parsed = validate(&ir, &Catalog::default())
            .unwrap_or_else(|errors| panic!("{name}: 保存時検証の誤り {errors:#?}"));
        let warnings = save_warnings(&parsed, &Catalog::default());
        assert!(
            warnings.is_empty(),
            "{name}: 保存を止めない警告 {warnings:#?}"
        );
        // 判定のノードは確からしさで振り分け、失敗も人へ回す（error を張らないと run が落ちる）。
        let ports = node_ports(&parsed);
        for node in decide_nodes(&s["workflow"]) {
            let id = node["id"].as_str().unwrap();
            assert_eq!(ports[id], ["out", "uncertain", "error"], "{name}: {id}");
        }
    }
}

#[test]
fn placeholders_are_declared_and_tables_match_the_grants() {
    for name in SCENARIOS {
        let s = scenario(name);
        let declared: BTreeSet<String> = s["placeholders"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut used = BTreeSet::new();
        placeholders_in(&s["workflow"], &mut used);
        placeholders_in(&s["tables"], &mut used);
        assert_eq!(
            declared, used,
            "{name}: 説明のあるプレースホルダと使っているもの"
        );
        let refs: BTreeSet<&str> = s["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["ref"].as_str().unwrap())
            .collect();
        for r in &refs {
            assert!(
                declared.contains(&format!("{{{{table.{r}}}}}")),
                "{name}: 表 {r} の id のプレースホルダ"
            );
        }
        for g in s["grants"].as_array().unwrap() {
            assert!(refs.contains(g["table"].as_str().unwrap()), "{name}: {g}");
        }
        // 置き換えた後に残るプレースホルダは無い（このテストの仮の値に漏れが無い）。
        let mut left = BTreeSet::new();
        placeholders_in(&fill(&s["workflow"]), &mut left);
        placeholders_in(&fill(&s["tables"]), &mut left);
        assert!(
            left.is_empty(),
            "{name}: 置き換わらないプレースホルダ {left:?}"
        );
    }
}

#[test]
fn questions_and_threshold_are_the_frozen_ones() {
    let frozen = read_json("frozen.json");
    for name in SCENARIOS {
        let s = scenario(name);
        assert_eq!(s["dataset"], name);
        let theta = &frozen["theta"][name];
        assert_eq!(&s["theta"], theta, "{name}: シナリオの説明のしきい値");
        assert_eq!(s["prompt_version"], frozen["prompt_version"], "{name}");
        let questions = read_json(&format!("datasets/{name}.questions.json"));
        let nodes = decide_nodes(&s["workflow"]);
        assert_eq!(nodes.len(), 1, "{name}");
        let params = &nodes[0]["params"];
        assert_eq!(
            params["questions"], questions,
            "{name}: 評価の質問のファイルと同じ（質問を変えたら評価し直して凍結する）"
        );
        assert_eq!(&params["route"]["min_confidence"], theta, "{name}");
        assert_eq!(params["route"]["by"], "confidence", "{name}");
    }
}
