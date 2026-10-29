//! ai.decide の params 契約と出口の規則の試験。

#![allow(clippy::panic)]

use serde_json::{json, Value};

use super::*;
use crate::ir::params::check_params;
use crate::vocab::NodeType;

/// 問い合わせの種類（choice）・緊急度（score）・不満（boolean）の 3 問。
fn base() -> Value {
    json!({
        "model": "decide-stub",
        "state": { "$from": "input", "path": "/body" },
        "questions": [
            { "key": "category", "type": "choice", "instructions": "問い合わせの種類はどれか",
              "options": [ { "key": "billing", "description": "請求・返金" },
                           { "key": "bug", "description": "不具合" },
                           { "key": "other" } ] },
            { "key": "priority", "type": "score", "instructions": "対応の緊急度",
              "levels": ["低", "中", "高"] },
            { "key": "angry", "type": "boolean", "instructions": "顧客は強い不満を示しているか",
              "yes": "苦情・強い不満", "no": "通常の問い合わせ" }
        ]
    })
}

/// `pointer` の位置に値を置く（無い項目は足す）。
fn with(mut p: Value, pointer: &str, v: Value) -> Value {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    let parent = p.pointer_mut(parent).unwrap();
    match parent {
        Value::Array(a) => a[key.parse::<usize>().unwrap()] = v,
        Value::Object(m) => {
            m.insert(key.to_string(), v);
        }
        _ => panic!("{pointer}"),
    }
    p
}

fn with_route(route: Value) -> Value {
    let mut p = base();
    p["route"] = route;
    p
}

fn err(p: &Value) -> ParamsIssue {
    check_params(NodeType::AiDecide, p).unwrap_err()
}

fn parsed(p: &Value) -> AiDecideParams {
    check_params(NodeType::AiDecide, p).unwrap();
    serde_json::from_value(p.clone()).unwrap()
}

#[test]
fn a_valid_node_passes_and_round_trips() {
    let p =
        with_route(json!({ "question": "category", "by": "confidence", "min_confidence": 0.8 }));
    let got = parsed(&p);
    assert_eq!(serde_json::to_value(&got).unwrap(), p);
    assert_eq!(got.route.as_ref().unwrap().by(), DecideRouteBy::Confidence);
    // by を省くと値で分ける。
    let p = with_route(json!({ "question": "angry", "min_confidence": 0.7 }));
    assert_eq!(parsed(&p).route.unwrap().by(), DecideRouteBy::Value);
}

#[test]
fn questions_are_literals_and_keys_follow_the_shape() {
    // 質問は値式にできない（作者のコード・出口が保存時に決まる）。
    let e = err(&with(base(), "/questions", json!({ "$from": "input" })));
    assert_eq!(e.path, "/params/questions");
    let e = err(&with(
        base(),
        "/questions/0/instructions",
        json!({ "$from": "input" }),
    ));
    assert_eq!(e.path, "/params/questions/0/instructions");
    for bad in ["Category", "1st", "has-dash", "", &"a".repeat(65)] {
        let e = err(&with(base(), "/questions/0/key", json!(bad)));
        assert_eq!(e.path, "/params/questions/0/key", "{bad}");
    }
    let e = err(&with(base(), "/questions/1/key", json!("category")));
    assert!(e.message.contains("重なって"), "{}", e.message);
    // 空の問いは保存を止めない（実行と自動実行のオンの前に止める・readiness・D-108）。長すぎる問いは止める。
    for blank in ["", "  "] {
        parsed(&with(base(), "/questions/0/instructions", json!(blank)));
    }
    let e = err(&with(
        base(),
        "/questions/0/instructions",
        json!("あ".repeat(MAX_DECIDE_INSTRUCTIONS_CHARS + 1)),
    ));
    assert_eq!(e.path, "/params/questions/0/instructions");
    let e = err(&with(base(), "/questions", json!([])));
    assert_eq!(e.path, "/params/questions");
    let many: Vec<Value> = (0..=MAX_DECIDE_QUESTIONS)
        .map(|i| json!({ "key": format!("q{i}"), "type": "boolean", "instructions": "x" }))
        .collect();
    assert_eq!(
        err(&with(base(), "/questions", json!(many))).path,
        "/params/questions"
    );
    // 型に無い項目・未知の項目。
    let e = err(&with(base(), "/questions/0/type", json!("number")));
    assert_eq!(e.path, "/params/questions/0/type");
    let mut p = base();
    p["questions"][0]["weight"] = json!(1);
    assert_eq!(err(&p).path, "/params/questions/0/weight");
}

#[test]
fn fields_must_match_the_question_type() {
    let e = err(&with(base(), "/questions/0/levels", json!(["a", "b"])));
    assert_eq!(e.path, "/params/questions/0/levels");
    let e = err(&with(
        base(),
        "/questions/1/options",
        json!([{ "key": "a" }, { "key": "b" }]),
    ));
    assert_eq!(e.path, "/params/questions/1/options");
    let mut p = base();
    p["questions"][1]["yes"] = json!("x");
    assert_eq!(err(&p).path, "/params/questions/1/yes");
    let e = err(&with(base(), "/questions/2/levels", json!(["a", "b"])));
    assert_eq!(e.path, "/params/questions/2/levels");
    let mut p = base();
    p["questions"][0]["no"] = json!("x");
    assert_eq!(err(&p).path, "/params/questions/0/no");
}

#[test]
fn options_and_levels_have_bounds() {
    let e = err(&with(
        base(),
        "/questions/0/options",
        json!([{ "key": "a" }]),
    ));
    assert_eq!(e.path, "/params/questions/0/options");
    let mut p = base();
    p["questions"][0].as_object_mut().unwrap().remove("options");
    assert_eq!(err(&p).path, "/params/questions/0/options");
    let many: Vec<Value> = (0..=MAX_DECIDE_OPTIONS)
        .map(|i| json!({ "key": format!("o{i}") }))
        .collect();
    assert_eq!(
        err(&with(base(), "/questions/0/options", json!(many))).path,
        "/params/questions/0/options"
    );
    for reserved in RESERVED_DECIDE_KEYS {
        let e = err(&with(base(), "/questions/0/options/2/key", json!(reserved)));
        assert_eq!(e.path, "/params/questions/0/options/2/key", "{reserved}");
        assert!(e.message.contains("使えません"), "{}", e.message);
    }
    let e = err(&with(
        base(),
        "/questions/0/options/1/key",
        json!("billing"),
    ));
    assert_eq!(e.path, "/params/questions/0/options/1/key");
    let long = "あ".repeat(MAX_DECIDE_DESCRIPTION_CHARS + 1);
    let e = err(&with(
        base(),
        "/questions/0/options/0/description",
        json!(long),
    ));
    assert_eq!(e.path, "/params/questions/0/options/0/description");
    let e = err(&with(base(), "/questions/1/levels", json!(["低"])));
    assert_eq!(e.path, "/params/questions/1/levels");
    let e = err(&with(base(), "/questions/1/levels", json!(["低", "低"])));
    assert_eq!(e.path, "/params/questions/1/levels/1");
    let e = err(&with(base(), "/questions/1/levels", json!(["低", " "])));
    assert_eq!(e.path, "/params/questions/1/levels/1");
    let e = err(&with(base(), "/questions/2/yes", json!(long)));
    assert_eq!(e.path, "/params/questions/2/yes");
}

#[test]
fn model_and_state_are_checked() {
    // 空のモデルは保存を止めない（実行と自動実行のオンの前に止める・readiness・D-83）。
    assert!(check_params(NodeType::AiDecide, &with(base(), "/model", json!(""))).is_ok());
    // モデルはリテラルの文字列（値式にしない）。
    assert_eq!(
        err(&with(base(), "/model", json!({ "$from": "input" }))).path,
        "/params/model"
    );
    for bad in [json!(1), json!(true), json!(null)] {
        let e = err(&with(base(), "/state", bad.clone()));
        assert_eq!(e.path, "/params/state", "{bad}");
    }
    for ok in [json!("本文"), json!({ "a": 1 }), json!([1, 2])] {
        assert!(check_params(NodeType::AiDecide, &with(base(), "/state", ok)).is_ok());
    }
}

#[test]
fn the_route_must_point_at_a_choice_or_boolean_question() {
    let e = err(&with_route(
        json!({ "question": "nope", "min_confidence": 0.8 }),
    ));
    assert_eq!(e.path, "/params/route/question");
    let e = err(&with_route(
        json!({ "question": "priority", "min_confidence": 0.8 }),
    ));
    assert_eq!(e.path, "/params/route/question");
    assert!(e.message.contains("level_index"), "{}", e.message);
    for bad in [0.49, 1.01, -1.0] {
        let e = err(&with_route(
            json!({ "question": "category", "min_confidence": bad }),
        ));
        assert_eq!(e.path, "/params/route/min_confidence", "{bad}");
    }
    for ok in [0.5, 1.0] {
        let p = with_route(json!({ "question": "category", "min_confidence": ok }));
        assert!(check_params(NodeType::AiDecide, &p).is_ok(), "{ok}");
    }
    let e = err(&with_route(
        json!({ "question": "category", "min_confidence": 0.3 }),
    ));
    assert_eq!(e.message, "しきい値は 0.50〜1.00 にしてください。");
    // しきい値は既定を置かない（版ごとに決めるもの・PIT-80）。空のまま保存でき、実行と自動実行のオンの前に
    // readiness が止める（D-88）。空のしきい値では答えに従わない（人の確認へ回す・多層防御）。
    for blank in [
        json!({ "question": "category" }),
        json!({ "question": "category", "min_confidence": null }),
    ] {
        let p = parsed(&with_route(blank));
        assert_eq!(p.route.as_ref().unwrap().min_confidence, None);
        assert_eq!(
            p.port_for(Some((RoutedValue::Choice("billing"), 0.99))),
            "uncertain"
        );
        assert_eq!(
            serde_json::to_value(p.route.as_ref().unwrap()).unwrap(),
            json!({ "question": "category" })
        );
    }
    let e = err(&with_route(
        json!({ "question": "category", "by": "best", "min_confidence": 0.8 }),
    ));
    assert_eq!(e.path, "/params/route/by");
}

#[test]
fn ports_follow_the_route() {
    assert_eq!(parsed(&base()).ports().unwrap(), ["out"]);
    let by_conf = parsed(&with_route(
        json!({ "question": "category", "by": "confidence", "min_confidence": 0.8 }),
    ));
    assert_eq!(by_conf.ports().unwrap(), ["out", "uncertain"]);
    let by_choice = parsed(&with_route(
        json!({ "question": "category", "min_confidence": 0.8 }),
    ));
    assert_eq!(
        by_choice.ports().unwrap(),
        ["billing", "bug", "other", "uncertain"]
    );
    let by_bool = parsed(&with_route(
        json!({ "question": "angry", "min_confidence": 0.7 }),
    ));
    assert_eq!(by_bool.ports().unwrap(), ["true", "false", "uncertain"]);
}

/// 出口は振り分けに関わる項目だけで決まる。ほかの質問の書きかけ（問いが空）・モデルの未選択・しきい値の範囲の
/// 外では導ける（エディタが古い出口を出し続けない）。振り分けの質問と選択肢のキーが出口にできないときは導けない。
#[test]
fn ports_need_only_the_routed_question() {
    // 型だけ読む（V1 の整合の検査は通さない）。
    let typed = |v: &Value| -> AiDecideParams { serde_json::from_value(v.clone()).unwrap() };
    let mut v = with_route(json!({ "question": "category", "min_confidence": 0.3 }));
    v["model"] = json!("");
    v["questions"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "key": "draft", "type": "choice", "instructions": "", "options": [] }));
    let p = typed(&v);
    assert!(p.check_cross_fields().is_err(), "V1 は誤りを報告する");
    assert_eq!(p.ports().unwrap(), ["billing", "bug", "other", "uncertain"]);
    // 振り分けに使う質問の問いが空でも、出口（選択肢のキー）は決まる。
    let blank = with(
        with_route(json!({ "question": "category", "min_confidence": 0.8 })),
        "/questions/0/instructions",
        json!(""),
    );
    assert_eq!(
        typed(&blank).ports().unwrap(),
        ["billing", "bug", "other", "uncertain"]
    );

    let ports_with = |pointer: &str, value: Value| {
        let v = with(
            with_route(json!({ "question": "category", "min_confidence": 0.8 })),
            pointer,
            value,
        );
        typed(&v).ports()
    };
    // 選択肢のキーが重なる・予約名・形に合わない・1 件だけ。
    assert_eq!(
        ports_with("/questions/0/options/1/key", json!("billing")),
        None
    );
    assert_eq!(ports_with("/questions/0/options/1/key", json!("out")), None);
    assert_eq!(ports_with("/questions/0/options/1/key", json!("Bug")), None);
    assert_eq!(
        ports_with("/questions/0/options", json!([{ "key": "billing" }])),
        None
    );
    // 振り分けの質問のキーが重なる（どちらの質問か決まらない）・無い質問・段階の質問（どちらの分け方でも）。
    assert_eq!(ports_with("/questions/2/key", json!("category")), None);
    assert_eq!(ports_with("/route/question", json!("nope")), None);
    for by in ["value", "confidence"] {
        let v = with_route(json!({ "question": "priority", "by": by, "min_confidence": 0.8 }));
        assert_eq!(typed(&v).ports(), None, "{by}");
    }
    // 選択肢の説明の長さは出口に関わらない。
    assert!(ports_with(
        "/questions/0/options/0/description",
        json!("あ".repeat(600))
    )
    .is_some());
}

/// 実行時の振り分けは、しきい値以上で答えに従い、届かなければ uncertain。選んだ出口は必ず ports にある。
#[test]
fn port_for_picks_one_of_the_ports() {
    let cases = [
        (base(), None, "out"),
        (
            with_route(
                json!({ "question": "category", "by": "confidence", "min_confidence": 0.8 }),
            ),
            Some((RoutedValue::Choice("bug"), 0.8)),
            "out",
        ),
        (
            with_route(
                json!({ "question": "category", "by": "confidence", "min_confidence": 0.8 }),
            ),
            Some((RoutedValue::Choice("bug"), 0.79)),
            "uncertain",
        ),
        (
            with_route(json!({ "question": "category", "min_confidence": 0.8 })),
            Some((RoutedValue::Choice("billing"), 0.9)),
            "billing",
        ),
        (
            with_route(json!({ "question": "category", "min_confidence": 0.8 })),
            Some((RoutedValue::Choice("billing"), 0.5)),
            "uncertain",
        ),
        (
            with_route(json!({ "question": "angry", "min_confidence": 0.7 })),
            Some((RoutedValue::Boolean(false), 0.7)),
            "false",
        ),
        (
            with_route(json!({ "question": "angry", "min_confidence": 0.7 })),
            Some((RoutedValue::Boolean(true), f64::NAN)),
            "uncertain",
        ),
    ];
    for (p, routed, want) in cases {
        let p = parsed(&p);
        let got = p.port_for(routed);
        assert_eq!(got, want, "{routed:?}");
        assert!(p.ports().unwrap().contains(&got), "{got} は出口に無い");
    }
}

/// 選択肢の名前（D-87）: 名前は答えの名前になり、説明は AI への手がかりに残る。名前の無い定義は今までどおり
/// 説明かキーを名前にし、AI への手がかりも名前を足す前と同じ文になる。
#[test]
fn an_option_label_names_the_answer_and_joins_the_hint() {
    let option = |v: Value| -> DecideOption { serde_json::from_value(v).unwrap() };
    let both = option(json!({ "key": "option_1", "label": "請求", "description": "請求書や返金" }));
    assert_eq!(both.name(), "請求");
    assert_eq!(both.ai_hint(), "請求: 請求書や返金");
    let label_only = option(json!({ "key": "option_2", "label": " 不具合 " }));
    // 名前は書いたまま返す（出口とテーブルの列に書く値。前後の空白は保存の前に画面が外す）。
    assert_eq!(label_only.name(), " 不具合 ");
    assert_eq!(label_only.ai_hint(), "不具合");
    // 名前を足す前の定義（説明だけ・キーだけ）は、名前も手がかりも今までと同じ。
    let legacy = option(json!({ "key": "billing", "description": "請求・返金" }));
    assert_eq!(
        (legacy.name(), legacy.ai_hint().as_str()),
        ("請求・返金", "請求・返金")
    );
    let bare = option(json!({ "key": "bug" }));
    assert_eq!((bare.name(), bare.ai_hint().as_str()), ("bug", ""));
    // 空白だけの名前は書いていないものとして扱う。
    let blank = option(json!({ "key": "other", "label": "  ", "description": "そのほか" }));
    assert_eq!(
        (blank.name(), blank.ai_hint().as_str()),
        ("そのほか", "そのほか")
    );
    // 保存した形に名前が残る（省略は書かない）。
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        json!({ "key": "bug" })
    );
    assert_eq!(
        serde_json::to_value(&both).unwrap(),
        json!({ "key": "option_1", "label": "請求", "description": "請求書や返金" })
    );
}

/// 選択肢の名前の決まり（D-87）: 100 字まで・質問の中で重ならない・説明とつないだ手がかりが説明の上限に収まる。
#[test]
fn option_labels_are_checked() {
    let options = |o: Value| with(base(), "/questions/0/options", o);
    assert!(check_params(
        NodeType::AiDecide,
        &options(json!([{ "key": "a", "label": "請求" }, { "key": "b", "label": "不具合" }]))
    )
    .is_ok());
    let e = err(&options(
        json!([{ "key": "a", "label": "x".repeat(101) }, { "key": "b" }]),
    ));
    assert_eq!(e.path, "/params/questions/0/options/0/label");
    assert!(e.message.contains("100 字まで"), "{}", e.message);
    let e = err(&options(
        json!([{ "key": "a", "label": "請求" }, { "key": "b", "label": " 請求" }]),
    ));
    assert_eq!(e.path, "/params/questions/0/options/1/label");
    assert!(
        e.message.contains("ほかの選択肢でも使っています"),
        "{}",
        e.message
    );
    // 名前と説明を合わせて 498 字まで（つなぐ「: 」の 2 字を足して説明の上限 500 字に収める）。
    let fits = json!([{ "key": "a", "label": "x".repeat(98), "description": "y".repeat(400) }, { "key": "b" }]);
    assert!(check_params(NodeType::AiDecide, &options(fits)).is_ok());
    let e = err(&options(
        json!([{ "key": "a", "label": "x".repeat(99), "description": "y".repeat(400) }, { "key": "b" }]),
    ));
    assert_eq!(e.path, "/params/questions/0/options/0/description");
    assert!(e.message.contains("合わせて 498 字まで"), "{}", e.message);
}
