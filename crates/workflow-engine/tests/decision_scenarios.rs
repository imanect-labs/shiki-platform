//! 判定の受け入れシナリオのテンプレート（`eval/decision/scenarios/*.json`・FEAT-1 の T8）が、今の IR の契約で
//! 保存できる形のままであることを確かめる（DB は使わない）。
//!
//! - プレースホルダ（`{{table.<ref>}}`・`{{model.decide}}`・`{{role.*}}`）を仮の値に置き換えると、保存時検証
//!   V1〜V7 を誤りなく通り、保存を止めない警告も出ない（スコープは保存の入口と同じく補ってから検証する）。
//!   照合の材料（カタログ）は、テンプレートの表（列と種別・状態の列）と状態の遷移の規則（状態）と、評価と同じ
//!   判定用のモデルから組む（V3 のモデル・V4 の承認のテーブル・行き先の状態・受信箱に出す列も照らす）。
//! - ファイルが使うプレースホルダは、どれも `placeholders` に説明がある（説明だけで使わないものも無い）。
//!   表の `ref` と同意（grants）の表の名前が合っている。
//! - 判定のノードの質問は評価の質問のファイル（`eval/decision/datasets/*.questions.json`）と同じで、しきい値は
//!   凍結（`frozen.json`）の値。評価で測ったものと違う質問・しきい値でシナリオを流さない（PIT-80）。
//! - 判定のノードの出口は、確からしさで振り分ける `out`・`uncertain` と、失敗を人へ回す `error`。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use workflow_engine::{
    complete_declared_scopes, node_ports, save_warnings, validate, Catalog, CatalogTable,
};

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

/// テンプレートの表の ref（仮の UUID の番号の順）。
const TABLE_REFS: [&str; 4] = ["inquiries", "routing_rules", "expense_claims", "approvers"];

/// 表の ref の仮の UUID。
fn table_id(r: &str) -> String {
    let i = TABLE_REFS
        .iter()
        .position(|t| *t == r)
        .unwrap_or_else(|| panic!("表 {r} の仮の id が無い（TABLE_REFS に足す）"));
    format!("00000000-0000-4000-8000-00000000000{}", i + 1)
}

/// プレースホルダを仮の値にする（表は UUID・判定用のモデルは評価と同じ id・部署は名前）。
fn fill(v: &Value) -> Value {
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for t in TABLE_REFS {
                out = out.replace(&format!("{{{{table.{t}}}}}"), &table_id(t));
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

/// 保存時の照合の材料: テンプレートの表（保存者＝作った人には全部の列が見える）・状態の遷移の規則の状態・
/// 評価と同じ判定用のモデル（保存 API が組織の設定と保存者の見え方で詰めるものの代わり）。
fn catalog(s: &Value) -> Catalog {
    let tables = s["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let fields = t["schema"]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    (
                        f["key"].as_str().unwrap().to_string(),
                        f["type"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            let states = t["fsm"]["states"].as_array().map(|xs| {
                xs.iter()
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            });
            let status_field = states
                .as_ref()
                .and(t["schema"]["status_field"].as_str().map(str::to_string));
            let table = CatalogTable {
                states,
                status_field,
                fields,
                unique: std::collections::BTreeSet::new(),
            };
            (table_id(t["ref"].as_str().unwrap()), table)
        })
        .collect();
    Catalog {
        models: Some(Vec::new()),
        decision_models: Some(vec!["decide-haiku".into()]),
        tables: Some(tables),
        ..Catalog::default()
    }
}

#[test]
fn templates_pass_the_save_validation_without_warnings() {
    for name in SCENARIOS {
        let s = scenario(name);
        let catalog = catalog(&s);
        let (ir, _) = complete_declared_scopes(&fill(&s["workflow"]));
        let parsed = validate(&ir, &catalog)
            .unwrap_or_else(|errors| panic!("{name}: 保存時検証の誤り {errors:#?}"));
        let warnings = save_warnings(&parsed, &catalog);
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

/// 照合の材料が効いている（承認の行き先の状態を書き誤ると V4 で落ちる・判定用でないモデルは V3 で落ちる）。
#[test]
fn the_catalog_catches_mistakes_in_the_templates() {
    let s = scenario("expense_policy");
    let catalog = catalog(&s);
    let codes = |ir: &Value| -> Vec<String> {
        let (ir, _) = complete_declared_scopes(&fill(ir));
        validate(&ir, &catalog)
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.code)
            .collect()
    };
    let mut wrong_state = s["workflow"].clone();
    for n in wrong_state["nodes"].as_array_mut().unwrap() {
        if n["id"] == "manager" {
            n["params"]["approve"]["to"] = "承認済み".into();
        }
    }
    assert!(codes(&wrong_state).iter().any(|c| c == "ir.unknown_state"));
    let mut wrong_field = s["workflow"].clone();
    for n in wrong_field["nodes"].as_array_mut().unwrap() {
        if n["id"] == "manager" {
            n["params"]["show_fields"] = serde_json::json!(["title", "ghost"]);
        }
    }
    assert!(codes(&wrong_field).iter().any(|c| c == "ir.unknown_field"));
    let mut wrong_model = s["workflow"].clone();
    for n in wrong_model["nodes"].as_array_mut().unwrap() {
        if n["id"] == "judge" {
            n["params"]["model"] = "claude-haiku-5-5".into();
        }
    }
    assert!(!codes(&wrong_model).is_empty());
}

/// 経費のテンプレートは自己承認を状態の遷移の規則で塞ぎ、申請者が書けるのを申請中の間だけにする。
#[test]
fn the_expense_template_blocks_self_approval() {
    let s = scenario("expense_policy");
    let t = &s["tables"][0];
    assert_eq!(t["ref"], "expense_claims");
    let actor = |from: &str, to: &str| -> Value {
        t["fsm"]["transitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["from"] == from && x["to"] == to)
            .unwrap_or_else(|| panic!("{from} → {to}"))["actor"]
            .clone()
    };
    let not_applicant = serde_json::json!({ "field_cmp": { "field": "applicant", "op": "ne", "value": "user_id" } });
    // 承認へ進める判断（上長の承認・経理の確認）は、申請者本人には許さない。
    for (from, to) in [("上長承認待ち", "承認"), ("経理確認", "上長承認待ち")] {
        let a = actor(from, to);
        assert!(
            a["all"]
                .as_array()
                .is_some_and(|xs| xs.contains(&not_applicant)),
            "{from} → {to}: {a}"
        );
    }
    // 申請者・上長の列は user_ref で、申請者は入れない（ワークフローが作成者と承認者の表から入れる）。
    let field = |k: &str| -> Value {
        t["schema"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == k)
            .unwrap()
            .clone()
    };
    for k in ["applicant", "approver"] {
        assert_eq!(field(k)["type"], "user_ref", "{k}");
        assert_ne!(
            field(k)["required"],
            true,
            "{k}: ワークフローが入れるので必須にしない"
        );
    }
    // 申請者（作成者）が書けるのは申請中の間だけ（ほかは経理の部署）。
    let write = &t["schema"]["row_policy"]["write"];
    assert_eq!(
        write["any"][0],
        serde_json::json!({ "all": ["is_owner", { "field_cmp": { "field": "status", "op": "eq", "value": { "lit": "申請中" } } }] })
    );
    // ワークフローは最初に受付中へ進め、受け付けた値（遷移の出力）で規程と判定を行う。
    let nodes = s["workflow"]["nodes"].as_array().unwrap();
    assert_eq!(nodes[0]["id"], "lock");
    assert_eq!(nodes[0]["params"]["to"], "受付中");
    let rules = nodes.iter().find(|n| n["id"] == "rules").unwrap();
    assert_eq!(
        rules["params"]["input"]["record"]["$from"],
        "nodes.lock.output"
    );
}
