//! TypeSafe 互換アダプタの本文の組み立てと答えの写し方（HTTP を介さない部分・HTTP はモックの結合テスト
//! tests/decision_typesafe_it.rs）。

use super::super::llm_decider_prompt::tests::request;
use super::*;
use crate::decision::check::check_response;

fn decider() -> TypesafeDecider {
    TypesafeDecider::new(reqwest::Client::new(), "http://mock/", "k", "jev-1.13.0")
}

/// 質問は名前 → `{type, instructions, criteria}` の表にし、はい/いいえは noul、選択肢は キー → 説明（無ければ
/// キー）、段階は低い順の配列。書いていない「いいえ」の説明は送らない。
#[test]
fn the_request_body_follows_the_published_shape() {
    let body = decider().body(&request());
    assert_eq!(
        body,
        json!({
            "model": "jev-1.13.0",
            "state": { "body": "請求書の金額が違います" },
            "questions": {
                "category": { "type": "choice", "instructions": "問い合わせの種類はどれか",
                              "criteria": { "billing": "請求・返金", "bug": "不具合", "other": "other" } },
                "priority": { "type": "score", "instructions": "対応の緊急度", "criteria": ["低", "中", "高"] },
                "angry": { "type": "noul", "instructions": "強い不満を示しているか",
                           "criteria": { "true": "苦情・解約の示唆" } }
            }
        })
    );
    let mut bare = request();
    bare.questions[2].kind = QuestionKind::Boolean {
        yes: None,
        no: None,
    };
    assert!(decider().body(&bare)["questions"]["angry"]
        .get("criteria")
        .is_none());
    assert_eq!(decider().base_url, "http://mock", "末尾の / を落とす");
}

fn wire(v: Value) -> BTreeMap<String, WireAnswer> {
    serde_json::from_value(v).unwrap()
}

/// 公開文書の例の形を中立形へ写す。確からしさは選んだ答えの確率（はい/いいえは max(p, 1-p)）。
#[test]
fn published_answers_map_to_the_neutral_shape() {
    let req = request();
    let answers = to_answers(
        &req,
        wire(json!({
            "category": { "type": "choice", "choice": "billing", "confidence": 0.7,
                          "probabilities": { "billing": 0.8, "bug": 0.15, "other": 0.05 } },
            "priority": { "type": "score", "score": 1.7, "confidence": 0.55,
                          "legend": { "0": "低", "1": "中", "2": "高" },
                          "probabilities": { "0": 0.1, "1": 0.1, "2": 0.8 } },
            "angry": { "type": "noul", "noul": 0.2 }
        })),
    )
    .unwrap();
    assert_eq!(
        answers["category"],
        Answer::Choice {
            value: "billing".into(),
            // TypeSafe の confidence（0.7）ではなく、選んだ選択肢の確率。
            confidence: 0.8,
            probabilities: BTreeMap::from([
                ("billing".into(), 0.8),
                ("bug".into(), 0.15),
                ("other".into(), 0.05)
            ]),
        }
    );
    assert_eq!(
        answers["priority"],
        Answer::Score {
            value: 1.7,
            level_index: 2,
            confidence: 0.8,
            probabilities: vec![0.1, 0.1, 0.8],
        }
    );
    assert_eq!(
        answers["angry"],
        Answer::Boolean {
            value: false,
            probability: 0.2,
            confidence: 0.8,
        }
    );
    let resp = DecideResponse {
        resolved_model: "jev-1.13.0".into(),
        answers,
        usage: Usage::default(),
        calibrated: false,
        method: DecisionMethod::Native,
        samples: 1,
        raised_questions: Vec::new(),
    };
    assert_eq!(
        check_response(&req, &resp),
        Ok(()),
        "gateway の突き合わせも通る"
    );
}

/// 確率の無い選択肢は 0。段階の確率が同じなら重み付けの位置に近い方を選ぶ。
#[test]
fn missing_probabilities_and_ties_are_resolved() {
    let req = request();
    let answers = to_answers(
        &req,
        wire(json!({
            "category": { "type": "choice", "choice": "bug", "probabilities": { "bug": 0.6, "billing": 0.4 } },
            "priority": { "type": "score", "score": 1.6, "probabilities": { "0": 0.2, "1": 0.4, "2": 0.4 } },
            "angry": { "type": "noul", "noul": 0.5 }
        })),
    )
    .unwrap();
    assert!(
        matches!(&answers["category"], Answer::Choice { probabilities, .. } if probabilities["other"] == 0.0)
    );
    assert!(matches!(
        answers["priority"],
        Answer::Score { level_index: 2, .. }
    ));
    assert!(matches!(
        answers["angry"],
        Answer::Boolean { value: true, confidence, .. } if (confidence - 0.5).abs() < 1e-12
    ));
}

/// 質問と合わない答え（欠け・余り・種類の違い・値域の外・段階に無いキー）は理由つきで拒む。
#[test]
fn mismatching_answers_are_rejected() {
    let req = request();
    let good = json!({
        "category": { "type": "choice", "choice": "billing", "probabilities": { "billing": 1.0 } },
        "priority": { "type": "score", "score": 0.0, "probabilities": { "0": 1.0 } },
        "angry": { "type": "noul", "noul": 0.9 }
    });
    assert!(to_answers(&req, wire(good.clone())).is_ok());
    let cases: Vec<(&str, Value)> = vec![
        ("/category", Value::Null),
        ("/extra", json!({ "type": "noul", "noul": 0.1 })),
        (
            "/angry",
            json!({ "type": "choice", "choice": "billing", "probabilities": {} }),
        ),
        ("/angry/noul", json!(1.5)),
        ("/category/choice", json!("refund")),
        ("/category/probabilities/refund", json!(0.1)),
        ("/priority/score", json!(2.5)),
        ("/priority/probabilities/3", json!(0.1)),
        ("/priority/probabilities/0", json!(-0.1)),
    ];
    for (at, value) in cases {
        let mut v = good.clone();
        let (parent, leaf) = at.rsplit_once('/').unwrap();
        let target = if parent.is_empty() {
            &mut v
        } else {
            v.pointer_mut(parent).unwrap()
        };
        if value.is_null() {
            target.as_object_mut().unwrap().remove(leaf);
        } else {
            target[leaf] = value;
        }
        assert!(to_answers(&req, wire(v)).is_err(), "{at} を拒まない");
    }
}

/// 使用量は形の崩れた応答からも読めるだけ読む（課金された分を会計に載せる）。
#[test]
fn usage_is_read_even_from_a_malformed_body() {
    let u =
        usage_of(&json!({ "usage": { "input_tokens": 120, "output_tokens": 12 }, "answers": 3 }));
    assert_eq!((u.prompt_tokens, u.completion_tokens), (120, 12));
    assert_eq!(usage_of(&json!({})), Usage::default());
}

/// 版ガードは区切りまで見る（`jev-1.1` に固定した設定は `jev-1.13.0` の答えを受けない）。
#[test]
fn the_version_guard_needs_a_delimiter_after_the_pinned_id() {
    assert!(same_version("jev-1.13.0", "jev-1.13.0"));
    for suffixed in ["jev-1.13.0-20261027", "jev-1.13.0+build.7", "jev-1.13.0@us"] {
        assert!(same_version("jev-1.13.0", suffixed), "{suffixed}");
    }
    for other in [
        "jev-1.13.0.1",
        "jev-1.130",
        "jev-1.14.0",
        "jev-1.1",
        "x-jev-1.13.0",
    ] {
        assert!(!same_version("jev-1.13.0", other), "{other}");
    }
    assert!(!same_version("jev-1.1", "jev-1.13.0"));
}
