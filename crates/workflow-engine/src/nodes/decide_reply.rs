//! 判定の応答（[`NodePorts::llm_decide`] の返り値）を型付きで読み、質問と突き合わせる（ai.decide・多層防御）。
//!
//! チョークポイント（llm-gateway）も応答を要求と突き合わせるが、エンジンはポートの実装（差し替え可能）を
//! 信用せず、もう一度見る。答えの過不足・種類の取り違え・値域の外・確率や確からしさの範囲の外は、後続へ
//! 渡さず `decision_invalid_response` にする（理由はサーバのログだけに残す）。
//!
//! [`NodePorts::llm_decide`]: super::ports::NodePorts::llm_decide

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ir::params::{DecideQuestion, DecideQuestionType, RoutedValue};

/// 確からしさの出し方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Method {
    Native,
    Verbalized,
    Vote,
}

/// 判定の応答（llm-gateway の `DecideResponse` の形・会計の usage などほかの項目は読まない）。
#[derive(Debug, Deserialize)]
pub(super) struct Reply {
    pub resolved_model: String,
    pub answers: BTreeMap<String, WireAnswer>,
    pub calibrated: bool,
    pub method: Method,
    pub samples: u8,
    /// 決定キャッシュの答えか（ポートが足す・無ければ false）。
    #[serde(default)]
    pub cached: bool,
}

/// 答え 1 件（llm-gateway の `Answer` の形）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum WireAnswer {
    Choice {
        value: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        value: f64,
        level_index: usize,
        confidence: f64,
        probabilities: Vec<f64>,
    },
    Boolean {
        value: bool,
        probability: f64,
        confidence: f64,
    },
}

impl WireAnswer {
    const fn confidence(&self) -> f64 {
        match self {
            WireAnswer::Choice { confidence, .. }
            | WireAnswer::Score { confidence, .. }
            | WireAnswer::Boolean { confidence, .. } => *confidence,
        }
    }
}

/// 0〜1 の有限の値か。
fn unit(x: f64) -> bool {
    x.is_finite() && (0.0..=1.0).contains(&x)
}

/// 応答を読み、質問と突き合わせる（`Err` はログに残す理由）。
pub(super) fn read(raw: &Value, questions: &[DecideQuestion]) -> Result<Reply, String> {
    let reply: Reply =
        serde_json::from_value(raw.clone()).map_err(|e| format!("応答の形が違う: {e}"))?;
    if reply.samples == 0 {
        return Err("samples が 0".into());
    }
    if reply.answers.len() != questions.len() {
        return Err(format!(
            "答えの数 {} が質問の数 {} と違う",
            reply.answers.len(),
            questions.len()
        ));
    }
    for q in questions {
        let a = reply
            .answers
            .get(&q.key)
            .ok_or_else(|| format!("質問 {} の答えが無い", q.key))?;
        check_answer(q, a).map_err(|why| format!("質問 {}: {why}", q.key))?;
    }
    Ok(reply)
}

fn check_answer(q: &DecideQuestion, a: &WireAnswer) -> Result<(), String> {
    if !unit(a.confidence()) {
        return Err("確からしさが 0〜1 の外".into());
    }
    match (q.kind, a) {
        (
            DecideQuestionType::Choice,
            WireAnswer::Choice {
                value,
                probabilities,
                ..
            },
        ) => {
            let keys: Vec<&str> = q.options.iter().flatten().map(|o| o.key.as_str()).collect();
            if !keys.contains(&value.as_str()) {
                return Err(format!("選択肢に無い答え {value}"));
            }
            let mut got: Vec<&str> = probabilities.keys().map(String::as_str).collect();
            let mut want = keys.clone();
            got.sort_unstable();
            want.sort_unstable();
            if got != want {
                return Err("確率の選択肢が質問と違う".into());
            }
            if !probabilities.values().all(|p| unit(*p)) {
                return Err("確率が 0〜1 の外".into());
            }
        }
        (
            DecideQuestionType::Score,
            WireAnswer::Score {
                value,
                level_index,
                probabilities,
                ..
            },
        ) => {
            let n = q.levels.as_ref().map_or(0, Vec::len);
            if *level_index >= n || probabilities.len() != n {
                return Err("段階の添字・確率の数が段階と合わない".into());
            }
            #[allow(clippy::cast_precision_loss)] // 段階は 10 以下。
            let top = (n - 1) as f64;
            if !value.is_finite() || !(0.0..=top).contains(value) {
                return Err("段階の平均の位置が範囲の外".into());
            }
            if !probabilities.iter().all(|p| unit(*p)) {
                return Err("確率が 0〜1 の外".into());
            }
        }
        (DecideQuestionType::Boolean, WireAnswer::Boolean { probability, .. }) => {
            if !unit(*probability) {
                return Err("確率が 0〜1 の外".into());
            }
        }
        _ => return Err("答えの種類が質問と違う".into()),
    }
    Ok(())
}

impl Reply {
    /// 振り分けに使う答え（choice の選択肢か boolean の答えと、その確からしさ）。
    pub(super) fn routed(&self, question: &str) -> Option<(RoutedValue<'_>, f64)> {
        match self.answers.get(question)? {
            WireAnswer::Choice {
                value, confidence, ..
            } => Some((RoutedValue::Choice(value), *confidence)),
            WireAnswer::Boolean {
                value, confidence, ..
            } => Some((RoutedValue::Boolean(*value), *confidence)),
            WireAnswer::Score { .. } => None,
        }
    }

    /// 出力の `answers`（選択肢の説明・段階の名前を添える。[`read`] を通った応答だけを渡す）。
    pub(super) fn answers_output(&self, questions: &[DecideQuestion]) -> Value {
        let mut out = serde_json::Map::new();
        for q in questions {
            let Some(a) = self.answers.get(&q.key) else {
                continue;
            };
            let v = match a {
                WireAnswer::Choice {
                    value,
                    confidence,
                    probabilities,
                } => {
                    let label = q
                        .options
                        .iter()
                        .flatten()
                        .find(|o| o.key == *value)
                        .and_then(|o| o.description.clone())
                        .filter(|d| !d.is_empty())
                        .unwrap_or_else(|| value.clone());
                    json!({ "type": "choice", "value": value, "label": label,
                            "confidence": confidence, "probabilities": probabilities })
                }
                WireAnswer::Score {
                    value,
                    level_index,
                    confidence,
                    probabilities,
                } => {
                    let level = q
                        .levels
                        .as_ref()
                        .and_then(|l| l.get(*level_index))
                        .cloned()
                        .unwrap_or_default();
                    json!({ "type": "score", "value": value, "level": level,
                            "level_index": level_index, "confidence": confidence,
                            "probabilities": probabilities })
                }
                WireAnswer::Boolean {
                    value,
                    probability,
                    confidence,
                } => json!({ "type": "boolean", "value": value, "probability": probability,
                             "confidence": confidence }),
            };
            out.insert(q.key.clone(), v);
        }
        Value::Object(out)
    }
}

#[cfg(test)]
#[path = "decide_reply_tests.rs"]
mod tests;
