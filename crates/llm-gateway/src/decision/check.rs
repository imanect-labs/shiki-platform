//! 判定の要求の上限と、応答と要求の突き合わせ（チョークポイントの多層防御）。
//!
//! gateway はワークフロー以外（ミニアプリ・script）からも通る前提なので、呼び出し側（エンジンの保存時の
//! 検証）と二重に見る。応答の突き合わせは、決定器（差し替え可能）が値域の外・欠けた答えを返しても、
//! 呼び出し側へ渡さないためにある。

use std::collections::BTreeSet;

use super::{Answer, DecideRequest, DecideResponse, QuestionKind};

/// 質問の数の上限。
pub const MAX_QUESTIONS: usize = 20;
/// 選択肢の数（下限・上限）。
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 100;
/// 段階の数（下限・上限）。
pub const MIN_LEVELS: usize = 2;
pub const MAX_LEVELS: usize = 10;
/// 判定材料（JSON に直した長さ）の上限。
pub const MAX_STATE_BYTES: usize = 64 * 1024;
/// 質問文の上限（文字数）。
pub const MAX_INSTRUCTIONS_CHARS: usize = 2000;
/// 選択肢・はい/いいえの説明の上限（文字数）。
pub const MAX_DESCRIPTION_CHARS: usize = 500;
/// 段階の名前の上限（文字数）。
pub const MAX_LEVEL_CHARS: usize = 100;
/// キー（質問・選択肢）の上限（文字数）。
pub const MAX_KEY_CHARS: usize = 64;

/// キーの形（英数字・`_`・`-` で 1〜64 文字）。出力の形（JSON Schema）のプロパティ名・列挙の値になる。
fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.chars().count() <= MAX_KEY_CHARS
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 要求が上限・形の決まりに合うか（合わなければ利用者に出せる理由）。
pub fn check_request(req: &DecideRequest) -> Result<(), String> {
    if req.questions.is_empty() || req.questions.len() > MAX_QUESTIONS {
        return Err(format!("質問は 1〜{MAX_QUESTIONS} 件にしてください"));
    }
    let state_len = serde_json::to_string(&req.state).map_or(usize::MAX, |s| s.len());
    if state_len > MAX_STATE_BYTES {
        return Err(format!(
            "判定材料が大きすぎます（{} KiB まで）",
            MAX_STATE_BYTES / 1024
        ));
    }
    let mut keys = BTreeSet::new();
    for q in &req.questions {
        if !key_ok(&q.key) {
            return Err(format!(
                "質問のキー「{}」は英数字・_・- の 1〜{MAX_KEY_CHARS} 文字にしてください",
                q.key
            ));
        }
        if !keys.insert(q.key.as_str()) {
            return Err(format!("質問のキー「{}」が重複しています", q.key));
        }
        let len = q.instructions.trim().chars().count();
        if len == 0 || len > MAX_INSTRUCTIONS_CHARS {
            return Err(format!(
                "質問「{}」の問いは 1〜{MAX_INSTRUCTIONS_CHARS} 文字にしてください",
                q.key
            ));
        }
        check_kind(&q.key, &q.kind)?;
    }
    Ok(())
}

fn check_kind(key: &str, kind: &QuestionKind) -> Result<(), String> {
    match kind {
        QuestionKind::Choice { options } => {
            if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&options.len()) {
                return Err(format!(
                    "質問「{key}」の選択肢は {MIN_OPTIONS}〜{MAX_OPTIONS} 件にしてください"
                ));
            }
            let mut seen = BTreeSet::new();
            for o in options {
                if !key_ok(&o.key) {
                    return Err(format!(
                        "質問「{key}」の選択肢のキー「{}」は英数字・_・- の 1〜{MAX_KEY_CHARS} 文字にしてください",
                        o.key
                    ));
                }
                if !seen.insert(o.key.as_str()) {
                    return Err(format!(
                        "質問「{key}」の選択肢のキー「{}」が重複しています",
                        o.key
                    ));
                }
                if o.description.chars().count() > MAX_DESCRIPTION_CHARS {
                    return Err(format!(
                        "質問「{key}」の選択肢の説明は {MAX_DESCRIPTION_CHARS} 文字までにしてください"
                    ));
                }
            }
        }
        QuestionKind::Score { levels } => {
            if !(MIN_LEVELS..=MAX_LEVELS).contains(&levels.len()) {
                return Err(format!(
                    "質問「{key}」の段階は {MIN_LEVELS}〜{MAX_LEVELS} 件にしてください"
                ));
            }
            let mut seen = BTreeSet::new();
            for l in levels {
                let n = l.trim().chars().count();
                if n == 0 || l.chars().count() > MAX_LEVEL_CHARS {
                    return Err(format!(
                        "質問「{key}」の段階の名前は 1〜{MAX_LEVEL_CHARS} 文字にしてください"
                    ));
                }
                if !seen.insert(l.as_str()) {
                    return Err(format!("質問「{key}」の段階「{l}」が重複しています"));
                }
            }
        }
        QuestionKind::Boolean { yes, no } => {
            let too_long = [yes, no]
                .into_iter()
                .flatten()
                .any(|d| d.chars().count() > MAX_DESCRIPTION_CHARS);
            if too_long {
                return Err(format!(
                    "質問「{key}」のはい/いいえの説明は {MAX_DESCRIPTION_CHARS} 文字までにしてください"
                ));
            }
        }
    }
    Ok(())
}

/// 0〜1 の有限の値か。
fn unit(x: f64) -> bool {
    x.is_finite() && (0.0..=1.0).contains(&x)
}

/// 応答が要求に合うか（答えの過不足・種類・値域・確率の範囲）。合わなければ原因（ログ用）。
pub fn check_response(req: &DecideRequest, resp: &DecideResponse) -> Result<(), String> {
    if resp.samples == 0 {
        return Err("samples が 0".into());
    }
    let asked: BTreeSet<&str> = req.questions.iter().map(|q| q.key.as_str()).collect();
    let answered: BTreeSet<&str> = resp.answers.keys().map(String::as_str).collect();
    if asked != answered {
        return Err(format!(
            "答えのキーが質問と合わない（質問 {asked:?}・答え {answered:?}）"
        ));
    }
    for q in &req.questions {
        let Some(a) = resp.answers.get(&q.key) else {
            return Err(format!("{}: 答えが無い", q.key));
        };
        check_answer(&q.key, &q.kind, a)?;
    }
    Ok(())
}

fn check_answer(key: &str, kind: &QuestionKind, answer: &Answer) -> Result<(), String> {
    match (kind, answer) {
        (
            QuestionKind::Choice { options },
            Answer::Choice {
                value,
                confidence,
                probabilities,
            },
        ) => {
            if !options.iter().any(|o| &o.key == value) {
                return Err(format!("{key}: 選択肢に無い値 {value}"));
            }
            // 確率のキーは選択肢と同じ集合（エンジンの突き合わせと同じ規則・一部の選択肢だけの確率は使わない）。
            let same_keys = probabilities.len() == options.len()
                && options.iter().all(|o| probabilities.contains_key(&o.key));
            if !same_keys || !unit(*confidence) || !probabilities.values().all(|p| unit(*p)) {
                return Err(format!(
                    "{key}: 確率が範囲の外か、確率のキーが選択肢と合わない"
                ));
            }
        }
        (
            QuestionKind::Score { levels },
            Answer::Score {
                value,
                level_index,
                confidence,
                probabilities,
            },
        ) => {
            let top = levels.len().saturating_sub(1);
            #[allow(clippy::cast_precision_loss)] // 段階は 10 件まで
            let in_range = value.is_finite() && (0.0..=top as f64).contains(value);
            if *level_index > top || !in_range {
                return Err(format!("{key}: 段階の位置が範囲の外"));
            }
            if probabilities.len() != levels.len()
                || !unit(*confidence)
                || !probabilities.iter().all(|p| unit(*p))
            {
                return Err(format!("{key}: 確率の数か値が範囲の外"));
            }
        }
        (
            QuestionKind::Boolean { .. },
            Answer::Boolean {
                probability,
                confidence,
                ..
            },
        ) => {
            if !unit(*probability) || !unit(*confidence) {
                return Err(format!("{key}: 確率が範囲の外"));
            }
        }
        _ => return Err(format!("{key}: 答えの種類が質問と合わない")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::{ChoiceOption, DecisionMethod, Question};
    use super::*;
    use crate::model::Usage;

    fn choice(key: &str, opts: &[&str]) -> Question {
        Question {
            key: key.into(),
            instructions: "どれか".into(),
            kind: QuestionKind::Choice {
                options: opts
                    .iter()
                    .map(|k| ChoiceOption {
                        key: (*k).into(),
                        description: String::new(),
                    })
                    .collect(),
            },
        }
    }

    fn req(questions: Vec<Question>) -> DecideRequest {
        DecideRequest {
            model: "d".into(),
            state: serde_json::json!("材料"),
            questions,
        }
    }

    #[test]
    fn request_limits_are_enforced() {
        assert!(check_request(&req(vec![choice("a", &["x", "y"])])).is_ok());
        assert!(check_request(&req(vec![])).is_err());
        let many = (0..=MAX_QUESTIONS)
            .map(|i| choice(&format!("q{i}"), &["x", "y"]))
            .collect();
        assert!(check_request(&req(many)).is_err());
        // 選択肢 1 件・重複したキー・使えない文字のキー。
        assert!(check_request(&req(vec![choice("a", &["x"])])).is_err());
        assert!(check_request(&req(vec![choice("a", &["x", "x"])])).is_err());
        assert!(check_request(&req(vec![choice("a b", &["x", "y"])])).is_err());
        assert!(check_request(&req(vec![
            choice("a", &["x", "y"]),
            choice("a", &["x", "y"])
        ]))
        .is_err());
        // 段階は 2〜10 件で重複なし。
        let score = |levels: &[&str]| Question {
            key: "s".into(),
            instructions: "どれくらい".into(),
            kind: QuestionKind::Score {
                levels: levels.iter().map(|l| (*l).to_string()).collect(),
            },
        };
        assert!(check_request(&req(vec![score(&["低", "高"])])).is_ok());
        assert!(check_request(&req(vec![score(&["低"])])).is_err());
        assert!(check_request(&req(vec![score(&["低", "低"])])).is_err());
        // 空の問い・大きすぎる材料。
        let mut empty = choice("a", &["x", "y"]);
        empty.instructions = "  ".into();
        assert!(check_request(&req(vec![empty])).is_err());
        let mut big = req(vec![choice("a", &["x", "y"])]);
        big.state = serde_json::json!("あ".repeat(MAX_STATE_BYTES));
        assert!(check_request(&big).unwrap_err().contains("大きすぎ"));
    }

    fn resp(answers: Vec<(&str, Answer)>) -> DecideResponse {
        DecideResponse {
            resolved_model: "m".into(),
            answers: answers
                .into_iter()
                .map(|(k, a)| (k.to_string(), a))
                .collect(),
            usage: Usage::default(),
            calibrated: false,
            method: DecisionMethod::Native,
            samples: 1,
        }
    }

    /// 選択肢 x・y の質問への答え（確率は両方の選択肢に付ける）。
    fn picked(value: &str, confidence: f64) -> Answer {
        Answer::Choice {
            value: value.into(),
            confidence,
            probabilities: ["x", "y"]
                .into_iter()
                .map(|k| {
                    let p = if k == value { confidence } else { 0.0 };
                    (k.to_string(), p)
                })
                .collect(),
        }
    }

    #[test]
    fn responses_must_match_the_questions() {
        let r = req(vec![choice("a", &["x", "y"])]);
        assert!(check_response(&r, &resp(vec![("a", picked("x", 0.9))])).is_ok());
        // 欠けた答え・余分な答え・選択肢に無い値・範囲外の確からしさ・種類違い。
        assert!(check_response(&r, &resp(vec![])).is_err());
        assert!(check_response(
            &r,
            &resp(vec![("a", picked("x", 0.9)), ("b", picked("x", 0.9))])
        )
        .is_err());
        assert!(check_response(&r, &resp(vec![("a", picked("z", 0.9))])).is_err());
        assert!(check_response(&r, &resp(vec![("a", picked("x", 1.5))])).is_err());
        assert!(check_response(&r, &resp(vec![("a", picked("x", f64::NAN))])).is_err());
        let boolean = Answer::Boolean {
            value: true,
            probability: 0.9,
            confidence: 0.9,
        };
        assert!(check_response(&r, &resp(vec![("a", boolean)])).is_err());
        // 確率のキーは選択肢と同じ集合（一部の選択肢だけ・選択肢に無いキーは使わない。エンジンと同じ規則）。
        let partial = Answer::Choice {
            value: "x".into(),
            confidence: 0.9,
            probabilities: BTreeMap::from([("x".to_string(), 0.9)]),
        };
        assert!(check_response(&r, &resp(vec![("a", partial)])).is_err());
        let extra = Answer::Choice {
            value: "x".into(),
            confidence: 0.9,
            probabilities: BTreeMap::from([
                ("x".to_string(), 0.9),
                ("y".to_string(), 0.1),
                ("z".to_string(), 0.0),
            ]),
        };
        assert!(check_response(&r, &resp(vec![("a", extra)])).is_err());
    }

    #[test]
    fn score_answers_stay_within_the_levels() {
        let r = req(vec![Question {
            key: "s".into(),
            instructions: "どれくらい".into(),
            kind: QuestionKind::Score {
                levels: vec!["低".into(), "中".into(), "高".into()],
            },
        }]);
        let score = |value: f64, level_index: usize, probabilities: Vec<f64>| Answer::Score {
            value,
            level_index,
            confidence: 0.8,
            probabilities,
        };
        assert!(check_response(&r, &resp(vec![("s", score(1.1, 1, vec![0.1, 0.7, 0.2]))])).is_ok());
        assert!(
            check_response(&r, &resp(vec![("s", score(1.1, 3, vec![0.1, 0.7, 0.2]))])).is_err()
        );
        assert!(
            check_response(&r, &resp(vec![("s", score(2.5, 2, vec![0.1, 0.7, 0.2]))])).is_err()
        );
        assert!(check_response(&r, &resp(vec![("s", score(1.0, 1, vec![0.3, 0.7]))])).is_err());
    }
}
