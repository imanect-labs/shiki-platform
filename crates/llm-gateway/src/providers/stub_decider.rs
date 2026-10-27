//! stub の決定器（FEAT-1）。実 LLM なしで判定ノードの振り分け・失敗の経路を結合テストと E2E で通すため、
//! 決まった規則で答える。
//!
//! - choice: 選択肢のキーか説明が判定材料に現れたら、その選択肢（先に並ぶもの）を確からしさ 0.9 で選ぶ。
//!   現れなければ先頭を 0.4 で選ぶ。
//! - score: 段階の名前が現れたら、その段階（先に並ぶもの）を 0.9 で選ぶ。無ければ最も低い段階を 0.4 で選ぶ。
//! - boolean: 「はい」の説明か `{key}:yes` が現れたら true を 0.9、「いいえ」の説明か `{key}:no` が現れたら
//!   false を 0.9 で選ぶ。どちらも無ければ false（「はい」の確率 0.4・確からしさ 0.6）。
//! - 材料に `stub:fail` があれば失敗する（使用量つき・会計の `#f` の経路を通すため）。`stub:malformed`
//!   があれば先頭の質問の答えを欠いた応答を返す（gateway の突き合わせで弾かれる経路）。
//!
//! 確からしさは較正されていない（`calibrated = false`）。

use std::collections::BTreeMap;

use crate::decision::{
    Answer, DecideFailure, DecideRequest, DecideResponse, DecisionMethod, DecisionProvider,
    QuestionKind,
};
use crate::model::Usage;
use crate::provider::LlmError;

/// 判定材料にこの文字列があれば失敗する。
pub const FAIL_TRIGGER: &str = "stub:fail";
/// 判定材料にこの文字列があれば、先頭の質問の答えを欠いた応答を返す。
pub const MALFORMED_TRIGGER: &str = "stub:malformed";

/// stub の決定器の版（応答の `resolved_model`・決定キャッシュのキー）。
pub const STUB_DECIDER_VERSION: &str = "stub-decider/v1";

/// 手がかりがあったときの確からしさ。
const HIT: f64 = 0.9;
/// 手がかりが無いときの確からしさ（choice・score）と、boolean の「はい」の確率。
const MISS: f64 = 0.4;

/// 決まった規則で答える決定器。
#[derive(Debug, Default, Clone, Copy)]
pub struct StubDecider;

fn state_text(state: &serde_json::Value) -> String {
    match state {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 選んだ値に `c`、残りに均等に配った確率。
fn spread(n: usize, chosen: usize, c: f64) -> Vec<f64> {
    #[allow(clippy::cast_precision_loss)] // 値域は 100 件まで
    let rest = (1.0 - c) / (n.saturating_sub(1).max(1) as f64);
    (0..n).map(|i| if i == chosen { c } else { rest }).collect()
}

fn hit(text: &str, needle: &str) -> bool {
    !needle.trim().is_empty() && text.contains(needle)
}

fn answer(key: &str, kind: &QuestionKind, text: &str) -> Answer {
    match kind {
        QuestionKind::Choice { options } => {
            let found = options
                .iter()
                .position(|o| hit(text, &o.key) || hit(text, &o.description));
            let (i, c) = found.map_or((0, MISS), |i| (i, HIT));
            let probs = spread(options.len(), i, c);
            Answer::Choice {
                value: options[i].key.clone(),
                confidence: c,
                probabilities: options
                    .iter()
                    .zip(probs)
                    .map(|(o, p)| (o.key.clone(), p))
                    .collect(),
            }
        }
        QuestionKind::Score { levels } => {
            let found = levels.iter().position(|l| hit(text, l));
            let (i, c) = found.map_or((0, MISS), |i| (i, HIT));
            let probs = spread(levels.len(), i, c);
            Answer::Score {
                #[allow(clippy::cast_precision_loss)]
                value: probs.iter().enumerate().map(|(j, p)| j as f64 * p).sum(),
                level_index: i,
                confidence: c,
                probabilities: probs,
            }
        }
        QuestionKind::Boolean { yes, no } => {
            let says = |d: &Option<String>, tag: &str| {
                d.as_deref().is_some_and(|d| hit(text, d)) || text.contains(&format!("{key}:{tag}"))
            };
            let probability = if says(yes, "yes") {
                HIT
            } else if says(no, "no") {
                1.0 - HIT
            } else {
                MISS
            };
            Answer::Boolean {
                value: probability > 0.5,
                probability,
                confidence: probability.max(1.0 - probability),
            }
        }
    }
}

#[async_trait::async_trait]
impl DecisionProvider for StubDecider {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn decide(&self, req: &DecideRequest) -> Result<DecideResponse, DecideFailure> {
        let text = state_text(&req.state);
        let asked: usize = req
            .questions
            .iter()
            .map(|q| q.instructions.chars().count())
            .sum();
        let usage = Usage {
            prompt_tokens: u64::try_from((text.chars().count() + asked) / 4 + 1)
                .unwrap_or(u64::MAX),
            completion_tokens: u64::try_from(req.questions.len() * 8).unwrap_or(u64::MAX),
            ..Default::default()
        };
        if text.contains(FAIL_TRIGGER) {
            return Err(DecideFailure::new(
                LlmError::Unavailable("stub decider: 判定に失敗しました".into()),
                usage,
            ));
        }
        let mut answers: BTreeMap<String, Answer> = req
            .questions
            .iter()
            .map(|q| (q.key.clone(), answer(&q.key, &q.kind, &text)))
            .collect();
        if text.contains(MALFORMED_TRIGGER) {
            if let Some(first) = req.questions.first() {
                answers.remove(&first.key);
            }
        }
        Ok(DecideResponse {
            resolved_model: STUB_DECIDER_VERSION.into(),
            answers,
            usage,
            calibrated: false,
            method: DecisionMethod::Native,
            samples: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::llm_decider_prompt::tests::request;
    use super::*;
    use crate::decision::check::check_response;

    #[tokio::test]
    async fn hints_in_the_state_pick_the_answer_and_otherwise_it_is_unsure() {
        let mut req = request();
        // 材料は「請求書の金額が違います」: 選択肢の説明「請求・返金」は現れないが、キーも無い。
        let got = StubDecider.decide(&req).await.unwrap();
        assert!(check_response(&req, &got).is_ok());
        assert!(
            matches!(&got.answers["category"], Answer::Choice { value, confidence, .. } if value == "billing" && (*confidence - MISS).abs() < 1e-9)
        );
        assert!(matches!(
            got.answers["priority"],
            Answer::Score { level_index: 0, .. }
        ));
        assert!(
            matches!(got.answers["angry"], Answer::Boolean { value: false, confidence, .. } if (confidence - 0.6).abs() < 1e-9)
        );
        assert!(got.usage.prompt_tokens > 0 && got.usage.completion_tokens > 0);

        req.state = serde_json::json!({ "body": "不具合です。緊急度は高。苦情・解約の示唆" });
        let got = StubDecider.decide(&req).await.unwrap();
        assert!(
            matches!(&got.answers["category"], Answer::Choice { value, confidence, .. } if value == "bug" && (*confidence - HIT).abs() < 1e-9)
        );
        assert!(matches!(
            got.answers["priority"],
            Answer::Score { level_index: 2, .. }
        ));
        assert!(matches!(
            got.answers["angry"],
            Answer::Boolean { value: true, .. }
        ));
        req.state = serde_json::json!("angry:no");
        let got = StubDecider.decide(&req).await.unwrap();
        assert!(
            matches!(got.answers["angry"], Answer::Boolean { value: false, confidence, .. } if (confidence - HIT).abs() < 1e-9)
        );
    }

    #[tokio::test]
    async fn triggers_reach_the_failure_paths() {
        let mut req = request();
        req.state = serde_json::json!(format!("x {FAIL_TRIGGER}"));
        let f = StubDecider.decide(&req).await.unwrap_err();
        assert!(matches!(f.error, LlmError::Unavailable(_)));
        assert!(f.usage.prompt_tokens > 0, "失敗しても使用量を返す");
        req.state = serde_json::json!(MALFORMED_TRIGGER);
        let got = StubDecider.decide(&req).await.unwrap();
        assert!(check_response(&req, &got).is_err());
    }
}
