//! 有効化する人の制限でのモデルの照合（D-196）の単体テスト。
#![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]

use serde_json::json;

use super::*;

const SCHEDULE: &str = "schedule";
const INTERACTIVE: &str = "interactive";

/// きっかけ（`schedule` か `interactive`）とブロックを持つ IR。
fn ir(trigger: &str, nodes: serde_json::Value) -> WorkflowIr {
    let trigger = if trigger == SCHEDULE {
        json!({ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" })
    } else {
        json!({ "kind": "interactive" })
    };
    WorkflowIr::from_json(&json!({
        "ir_version": 1, "name": "wf", "triggers": [trigger], "nodes": nodes, "edges": []
    }))
    .unwrap()
}

/// llm.invoke・agent.invoke・ai.decide を 1 つずつ持つ IR（`None` はモデルを省く）。
fn model_ir(trigger: &str, llm: Option<&str>, agent: Option<&str>, decide: &str) -> WorkflowIr {
    let with_model = |mut params: serde_json::Value, model: Option<&str>| {
        if let Some(m) = model {
            params["model"] = json!(m);
        }
        params
    };
    ir(
        trigger,
        json!([
            { "id": "ask", "type": "llm.invoke", "params": with_model(json!({ "prompt": "hi" }), llm) },
            { "id": "work", "type": "agent.invoke", "params": with_model(json!({ "instruction": "調べて" }), agent) },
            { "id": "judge", "type": "ai.decide", "params": {
                "model": decide, "state": { "$from": "input" },
                "questions": [{ "key": "angry", "type": "boolean", "instructions": "強い不満があるか" }] } }
        ]),
    )
}

/// 組織の chat モデル hidden・visible と判定用のモデル judge・decide のうち、hidden と judge が使えない人の材料。
/// 組織の既定（hidden）も使えない。
fn restricted() -> Catalog {
    Catalog {
        models: Some(vec!["visible".into()]),
        decision_models: Some(vec!["decide".into()]),
        user_restricted_models: vec!["hidden".into()],
        user_restricted_decision_models: vec!["judge".into()],
        default_model_unavailable: Some("hidden".into()),
        ..Catalog::default()
    }
}

/// 制限の無い人の材料。
fn unrestricted() -> Catalog {
    Catalog {
        models: Some(vec!["hidden".into(), "visible".into()]),
        decision_models: Some(vec!["judge".into(), "decide".into()]),
        ..Catalog::default()
    }
}

/// 誤りの（ノード id・位置・文）。どれも [`MODEL_UNAVAILABLE_FOR_USER`] で、有効化の文脈の 1 文で終わる。
fn blockers(ir: &WorkflowIr, catalog: &Catalog) -> Vec<(String, String, String)> {
    user_model_blockers(ir, catalog)
        .into_iter()
        .map(|e| {
            assert_eq!(e.code, MODEL_UNAVAILABLE_FOR_USER, "{e:?}");
            assert!(e.message.ends_with(ENABLE_HINT), "{e:?}");
            (
                e.node_id.unwrap_or_default(),
                e.path.unwrap_or_default(),
                e.message,
            )
        })
        .collect()
}

/// 書いたモデル・省いたモデル（組織の既定）・agent.invoke・ai.decide の判定用のモデルが使えなければ、ブロックの順に
/// 止める。位置とノード id は保存のときと同じ。
#[test]
fn unavailable_models_block_enabling_in_node_order() {
    let errs = blockers(
        &model_ir(SCHEDULE, Some("hidden"), None, "judge"),
        &restricted(),
    );
    let at: Vec<(&str, &str)> = errs
        .iter()
        .map(|(n, p, _)| (n.as_str(), p.as_str()))
        .collect();
    assert_eq!(
        at,
        [
            ("ask", "/params/model"),
            ("work", "/params/model"),
            ("judge", "/params/model")
        ]
    );
    assert_eq!(
        errs[0].2,
        "このモデルはあなたのアカウントでは使えません: hidden。一覧からモデルを選び直してください（使えるモデル: visible）。\
         自動実行は、このモデルを使える人にオンにしてもらうこともできます"
    );
    assert!(
        errs[1]
            .2
            .starts_with("モデルを省くと組織の既定のモデル（hidden）で動きますが"),
        "{errs:?}"
    );
    assert!(
        errs[2]
            .2
            .starts_with("この判定用のモデルはあなたのアカウントでは使えません: judge。"),
        "{errs:?}"
    );
    // 書いた agent.invoke のモデルも、省いた llm.invoke のモデルも同じく止める。
    let errs = blockers(
        &model_ir(SCHEDULE, None, Some("hidden"), "decide"),
        &restricted(),
    );
    let nodes: Vec<&str> = errs.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(nodes, ["ask", "work"]);
    // 使えるモデルなら止めない。
    assert!(blockers(
        &model_ir(SCHEDULE, Some("visible"), Some("visible"), "decide"),
        &restricted()
    )
    .is_empty());
}

/// 制限の無い材料と、schedule・event のきっかけを持たない IR では止めない（対話の実行は本人の文脈で動く）。
#[test]
fn nothing_blocks_without_a_restriction_or_an_unattended_trigger() {
    let saved = model_ir(SCHEDULE, Some("hidden"), None, "judge");
    assert!(blockers(&saved, &unrestricted()).is_empty());
    assert!(blockers(&saved, &Catalog::default()).is_empty());
    let manual = model_ir(INTERACTIVE, Some("hidden"), None, "judge");
    assert!(blockers(&manual, &restricted()).is_empty());
    // できごとのきっかけも照らす。
    let event = WorkflowIr::from_json(&json!({
        "ir_version": 1, "name": "wf",
        "triggers": [{ "kind": "event", "source": "storage.write",
                       "scope": { "folder": "00000000-0000-0000-0000-0000000000f1" } }],
        "nodes": [{ "id": "ask", "type": "llm.invoke", "params": { "prompt": "hi", "model": "hidden" } }],
        "edges": []
    }))
    .unwrap();
    assert_eq!(blockers(&event, &restricted()).len(), 1);
}

/// 制限の照合のほか（選択の禁止・組織で使えないモデル・種類の違い）は止めない（実行のときに断られる）。
#[test]
fn other_model_errors_are_left_to_the_run() {
    // 種類の違い（判定用のモデルを llm.invoke に・chat モデルを ai.decide に）と、組織で使えないモデル。
    let ir = model_ir(SCHEDULE, Some("judge"), Some("ghost"), "hidden");
    let all = user_model_blockers(
        &ir,
        &Catalog {
            default_model_unavailable: None,
            ..restricted()
        },
    );
    assert!(all.is_empty(), "{all:?}");
    // 選択の禁止（既定と違うモデル）。
    let locked = Catalog {
        model_lock: Some("visible".into()),
        default_model_unavailable: None,
        ..restricted()
    };
    let ir = model_ir(SCHEDULE, Some("hidden"), Some("visible"), "decide");
    assert!(user_model_blockers(&ir, &locked).is_empty());
}

/// スキル: shiki script のスキルは止めない。instructions だけのスキルは保存のときと同じ文で、入れていないスキルは
/// 確かめられないと言って止める。組織の既定が使えれば止めない。
#[test]
fn skills_block_unless_they_run_a_shiki_script() {
    let skills = ir(
        SCHEDULE,
        json!([
            { "id": "memo", "type": "skill.invoke", "params": { "skill": "skill:memo@1.0.0" } },
            { "id": "tidy", "type": "skill.invoke", "params": { "skill": "skill:tidy@1.0.0" } },
            { "id": "other", "type": "skill.invoke", "params": { "skill": "skill:other@2.0.0" } },
            { "id": "blank", "type": "skill.invoke", "params": { "skill": "" } }
        ]),
    );
    let catalog = Catalog {
        skills: [
            ("memo".to_string(), ["1.0.0".to_string()].into()),
            ("tidy".to_string(), ["1.0.0".to_string()].into()),
        ]
        .into(),
        script_skills: ["tidy@1.0.0".to_string()].into(),
        ..restricted()
    };
    let errs = blockers(&skills, &catalog);
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert_eq!(
        (errs[0].0.as_str(), errs[0].1.as_str()),
        ("memo", "/params/skill")
    );
    assert!(
        errs[0].2.starts_with(
            "このスキルは組織の既定のモデル（hidden）で動きますが、このモデルはあなたのアカウントでは使えません。"
        ),
        "{errs:?}"
    );
    assert_eq!(
        (errs[1].0.as_str(), errs[1].1.as_str()),
        ("other", "/params/skill")
    );
    assert_eq!(
        errs[1].2,
        "このスキル（other@2.0.0）はあなたがインストールしていないため、組織の既定のモデル（hidden）で動くかを\
         確かめられません。このモデルはあなたのアカウントでは使えないため、自動実行をオンにできません。\
         スキルをインストールするか、管理者に相談してください。自動実行は、このモデルを使える人にオンにしてもらうこともできます"
    );
    // 組織の既定が使えれば、どのスキルも止めない（入れていないものも）。
    let usable = Catalog {
        default_model_unavailable: None,
        ..catalog
    };
    assert!(blockers(&skills, &usable).is_empty());
}

/// 句点で終わる文には句点を重ねない。
#[test]
fn the_hint_does_not_double_the_full_stop() {
    let e = for_enabling(ValidationError::new(
        MODEL_UNAVAILABLE_FOR_USER,
        "使えません。",
    ));
    assert_eq!(e.message, format!("使えません。{ENABLE_HINT}"));
}
