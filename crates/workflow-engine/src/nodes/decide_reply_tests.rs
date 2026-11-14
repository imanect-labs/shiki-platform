//! 判定の応答の読み取りと突き合わせの試験。

use serde_json::{json, Value};

use super::*;

fn questions() -> Vec<DecideQuestion> {
    serde_json::from_value(json!([
        { "key": "category", "type": "choice", "instructions": "種類",
          "options": [ { "key": "billing", "description": "請求・返金" }, { "key": "bug" } ] },
        { "key": "priority", "type": "score", "instructions": "緊急度", "levels": ["低", "中", "高"] },
        { "key": "angry", "type": "boolean", "instructions": "不満" }
    ]))
    .unwrap()
}

fn reply() -> Value {
    json!({
        "resolved_model": "stub-decider",
        "answers": {
            "category": { "type": "choice", "value": "billing", "confidence": 0.9,
                          "probabilities": { "billing": 0.9, "bug": 0.1 } },
            "priority": { "type": "score", "value": 1.2, "level_index": 1, "confidence": 0.7,
                          "probabilities": [0.1, 0.7, 0.2] },
            "angry": { "type": "boolean", "value": false, "probability": 0.2, "confidence": 0.8 }
        },
        "usage": { "prompt_tokens": 10, "completion_tokens": 5 },
        "calibrated": false, "method": "native", "samples": 1
    })
}

fn with(mut v: Value, pointer: &str, x: Value) -> Value {
    *v.pointer_mut(pointer).unwrap() = x;
    v
}

#[test]
fn a_matching_reply_is_read_and_labelled() {
    let r = read(&reply(), &questions()).unwrap();
    assert_eq!(r.method, Method::Native);
    let out = r.answers_output(&questions());
    assert_eq!(out["category"]["label"], "請求・返金");
    assert_eq!(out["category"]["probabilities"]["bug"], 0.1);
    assert_eq!(out["priority"]["level"], "中");
    assert_eq!(out["priority"]["level_index"], 1);
    assert_eq!(
        out["angry"],
        json!({ "type": "boolean", "value": false, "probability": 0.2, "confidence": 0.8 })
    );
    // 説明の無い選択肢はキーを名前にする。
    let r = read(
        &with(reply(), "/answers/category/value", json!("bug")),
        &questions(),
    )
    .unwrap();
    assert_eq!(r.answers_output(&questions())["category"]["label"], "bug");
    assert!(
        matches!(r.routed("category"), Some((RoutedValue::Choice("bug"), c)) if (c - 0.9).abs() < 1e-9)
    );
    assert!(matches!(
        r.routed("angry"),
        Some((RoutedValue::Boolean(false), _))
    ));
    assert!(r.routed("priority").is_none());
}

/// 選択肢に名前（D-87）があれば、答えの名前は説明ではなく名前になる（テーブルの選択肢の列に書き込む値）。
#[test]
fn an_option_label_becomes_the_answer_label() {
    let labelled: Vec<DecideQuestion> = serde_json::from_value(with(
        serde_json::to_value(questions()).unwrap(),
        "/0/options/0",
        json!({ "key": "billing", "label": "請求", "description": "請求書や返金についての問い合わせ" }),
    ))
    .unwrap();
    let r = read(&reply(), &labelled).unwrap();
    assert_eq!(r.answers_output(&labelled)["category"]["label"], "請求");
    assert_eq!(r.answers_output(&labelled)["category"]["value"], "billing");
}

#[test]
fn mismatching_replies_are_rejected() {
    let bad = [
        ("答えが欠けた", {
            let mut v = reply();
            v["answers"].as_object_mut().unwrap().remove("angry");
            v
        }),
        ("余計な答え", {
            let mut v = reply();
            v["answers"]["extra"] =
                json!({ "type": "boolean", "value": true, "probability": 1.0, "confidence": 1.0 });
            v
        }),
        (
            "種類の取り違え",
            with(
                reply(),
                "/answers/angry",
                json!({ "type": "choice", "value": "billing",
            "confidence": 0.9, "probabilities": { "billing": 0.9, "bug": 0.1 } }),
            ),
        ),
        (
            "選択肢に無い答え",
            with(reply(), "/answers/category/value", json!("refund")),
        ),
        (
            "確率の選択肢が違う",
            with(
                reply(),
                "/answers/category/probabilities",
                json!({ "billing": 1.0 }),
            ),
        ),
        (
            "確率が範囲の外",
            with(reply(), "/answers/category/probabilities/bug", json!(1.5)),
        ),
        (
            "確からしさが範囲の外",
            with(reply(), "/answers/angry/confidence", json!(-0.1)),
        ),
        (
            "段階の添字が外",
            with(reply(), "/answers/priority/level_index", json!(3)),
        ),
        (
            "段階の確率の数",
            with(
                reply(),
                "/answers/priority/probabilities",
                json!([0.5, 0.5]),
            ),
        ),
        (
            "段階の位置が外",
            with(reply(), "/answers/priority/value", json!(2.5)),
        ),
        (
            "はいの確率が外",
            with(reply(), "/answers/angry/probability", json!(2)),
        ),
        ("出し方が未知", with(reply(), "/method", json!("magic"))),
        ("samples が 0", with(reply(), "/samples", json!(0))),
        ("形が違う", json!({ "text": "billing" })),
    ];
    for (name, v) in bad {
        assert!(read(&v, &questions()).is_err(), "{name}");
    }
}

/// 引き上げた質問（BUG-124・D-114）は、無い応答（古いポート・決定キャッシュの古い答え）なら空。あれば質問の順に
/// 並べて出し、質問に無いキー・重なりは突き合わせで弾く。
#[test]
fn raised_questions_are_read_in_question_order_and_checked() {
    let r = read(&reply(), &questions()).unwrap();
    assert!(r.raised_output(&questions()).is_empty());
    let mut raised = reply();
    raised["raised_questions"] = json!(["angry", "category"]);
    let r = read(&raised, &questions()).unwrap();
    assert_eq!(r.raised_output(&questions()), ["category", "angry"]);
    for bad in [
        json!(["unknown"]),
        json!(["angry", "angry"]),
        json!("angry"),
    ] {
        let mut v = reply();
        v["raised_questions"] = bad.clone();
        assert!(read(&v, &questions()).is_err(), "{bad}");
    }
}
